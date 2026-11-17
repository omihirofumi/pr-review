#[allow(dead_code)]
#[path = "../src/app.rs"]
mod app;
#[allow(dead_code)]
#[path = "../src/store.rs"]
mod store;
#[allow(dead_code)]
#[path = "../src/ui.rs"]
mod ui;

use app::{Action, AiStatus, App, Dialog, Focus, PageTab};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use prpr::{
    analysis::{Analysis, Coverage, Intent, ReadingStep, Verification},
    model::{PrOverview, Side},
    settings::{self, Provider},
};
use ratatui::{Terminal, backend::TestBackend};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(app: &mut App, character: char) -> anyhow::Result<Action> {
    app.key(key(KeyCode::Char(character)))
}

fn overview() -> PrOverview {
    let data = app::demo_data();
    PrOverview {
        id: data.pr.id,
        title: data.pr.title,
        body: data.pr.body,
        url: data.pr.url,
        author: data.pr.author,
        head_sha: data.pr.head_sha,
        base_tip_sha: data.pr.base_sha,
        draft: data.pr.draft,
        state: "open".into(),
        changed_files: data.files.len() as u64,
        additions: data.files.iter().map(|file| file.additions).sum(),
        deletions: data.files.iter().map(|file| file.deletions).sum(),
    }
}

fn analysis() -> Analysis {
    Analysis {
        summary: "Bound retry waits by the remaining deadline.".into(),
        intent: Intent {
            text: "Preserve the caller's deadline".into(),
            basis: "author_description".into(),
        },
        changes: Vec::new(),
        reading_order: vec![
            ReadingStep {
                path: "tests/deadline.rs".into(),
                line: Some(3),
                side: Side::Right,
                reason: "Start with the expected behavior".into(),
            },
            ReadingStep {
                path: "src/client.rs".into(),
                line: Some(20),
                side: Side::Left,
                reason: "Read the removed wait".into(),
            },
            ReadingStep {
                path: "src/client.rs".into(),
                line: Some(24),
                side: Side::Right,
                reason: "Read the send boundary".into(),
            },
        ],
        risks: Vec::new(),
        verification: Verification {
            existing_tests: vec!["tests/deadline.rs".into()],
            suggested_checks: Vec::new(),
            unknowns: Vec::new(),
        },
        coverage: Coverage::default(),
    }
}

fn screen(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 60)).unwrap();
    terminal.draw(|frame| ui::draw(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .chunks(120)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn metadata_is_visible_before_any_diff_or_comments_exist() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.opened(overview());
    assert_eq!(app.tab, PageTab::Overview);
    assert!(app.review.is_none());
    assert!(app.diff_loading);
    assert!(matches!(
        app.overview.as_ref().unwrap().ai,
        AiStatus::Disabled
    ));
    let rendered = screen(&mut app);
    assert!(rendered.contains("Keep request retries"));
    assert!(rendered.contains("Motivation"));
    assert!(rendered.contains("Retries currently ignore"));
}

#[test]
fn overview_and_diff_tabs_remain_switchable_while_the_diff_is_pending() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.opened(overview());
    press(&mut app, '2').unwrap();
    assert_eq!(app.tab, PageTab::Guide);
    press(&mut app, '3').unwrap();
    assert_eq!(app.tab, PageTab::Diff);
    assert!(app.review.is_none());
    app.key(key(KeyCode::Tab)).unwrap();
    assert_eq!(app.tab, PageTab::Overview);
    app.key(key(KeyCode::Tab)).unwrap();
    assert_eq!(app.tab, PageTab::Guide);
    app.key(key(KeyCode::BackTab)).unwrap();
    assert_eq!(app.tab, PageTab::Overview);
    app.key(key(KeyCode::BackTab)).unwrap();
    assert_eq!(app.tab, PageTab::Diff);
    press(&mut app, '1').unwrap();
    assert_eq!(app.tab, PageTab::Overview);
    assert!(app.diff_loading);
}

#[test]
fn arriving_diff_preserves_the_active_tab_and_overview_scroll() {
    for tab in [PageTab::Overview, PageTab::Diff] {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(directory.path().into(), false);
        app.opened(overview());
        app.tab = tab;
        app.overview.as_mut().unwrap().scroll = 3;
        app.accept_diff(app::demo_data()).unwrap();
        assert_eq!(app.tab, tab);
        assert_eq!(app.overview.as_ref().unwrap().scroll, 3);
        assert!(app.review.is_some());
        assert!(!app.diff_loading);
    }
}

#[test]
fn overview_navigation_does_not_move_the_saved_diff_cursor() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.opened(overview());
    app.accept_diff(app::demo_data()).unwrap();
    let review = app.review.as_mut().unwrap();
    review.selected_file = 1;
    review.positions[1] = 4;
    review.horizontal = 8;
    for character in ['j', 'j', 'k', 'n', 'p', 'g', 'e', 'g', 'g'] {
        press(&mut app, character).unwrap();
    }
    let review = app.review.as_ref().unwrap();
    assert_eq!(review.selected_file, 1);
    assert_eq!(review.positions[1], 4);
    assert_eq!(review.horizontal, 8);
    assert_eq!(app.tab, PageTab::Overview);
}

#[test]
fn reading_guide_jumps_to_the_requested_side_and_numbered_source_line() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.opened(overview());
    app.accept_diff(app::demo_data()).unwrap();
    app.overview.as_mut().unwrap().ai = AiStatus::Ready(Box::new(analysis()));

    press(&mut app, 'n').unwrap();
    app.key(key(KeyCode::Enter)).unwrap();
    assert_eq!(app.tab, PageTab::Overview, "Overview has no reading steps");
    press(&mut app, '2').unwrap();
    press(&mut app, 'n').unwrap();
    app.key(key(KeyCode::Enter)).unwrap();
    let review = app.review.as_ref().unwrap();
    assert_eq!(review.current_file().unwrap().filename, "src/client.rs");
    assert_eq!(review.current_lines()[review.position()].old_line, Some(20));
    assert_eq!(review.focus, Focus::Diff);
    assert_eq!(app.tab, PageTab::Diff);

    press(&mut app, '2').unwrap();
    press(&mut app, 'n').unwrap();
    app.key(key(KeyCode::Enter)).unwrap();
    let review = app.review.as_ref().unwrap();
    assert_eq!(review.current_lines()[review.position()].new_line, Some(24));

    press(&mut app, '2').unwrap();
    press(&mut app, 'p').unwrap();
    press(&mut app, 'p').unwrap();
    press(&mut app, 'g').unwrap();
    press(&mut app, 'f').unwrap();
    let review = app.review.as_ref().unwrap();
    assert_eq!(review.current_file().unwrap().filename, "tests/deadline.rs");
    assert_eq!(review.current_lines()[review.position()].new_line, Some(3));
}

#[test]
fn reading_guide_cannot_jump_until_the_diff_is_available() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.opened(overview());
    app.overview.as_mut().unwrap().ai = AiStatus::Ready(Box::new(analysis()));
    app.tab = PageTab::Guide;
    assert!(app.key(key(KeyCode::Enter)).is_err());
    assert_eq!(app.tab, PageTab::Guide);
    assert!(app.review.is_none());
}

#[test]
fn disabled_ai_picker_saves_the_selected_provider_before_requesting_analysis() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().join("state"), false);
    let config = directory.path().join("config.toml");
    app.config_path = Some(config.clone());
    app.opened(overview());
    press(&mut app, 'a').unwrap();
    assert!(matches!(
        app.dialog,
        Some(Dialog::AgentPicker {
            selected: 0,
            generate: true
        })
    ));
    press(&mut app, 'j').unwrap();
    let action = app.key(key(KeyCode::Enter)).unwrap();
    assert!(matches!(action, Action::Explain { force: false }));
    assert_eq!(app.settings.ai.provider, Provider::Claude);
    assert_eq!(
        settings::load(&config).unwrap().ai.provider,
        Provider::Claude
    );
    assert!(matches!(
        app.overview.as_ref().unwrap().ai,
        AiStatus::Waiting
    ));
}

#[test]
fn failed_settings_save_preserves_the_picker_and_previous_provider() {
    let directory = tempfile::tempdir().unwrap();
    let blocked = directory.path().join("blocked");
    std::fs::write(&blocked, "A file blocks the configuration directory").unwrap();
    let mut app = App::new(directory.path().join("state"), false);
    app.config_path = Some(blocked.join("config.toml"));
    app.opened(overview());
    press(&mut app, 'a').unwrap();
    press(&mut app, 'j').unwrap();
    assert!(app.key(key(KeyCode::Enter)).is_err());
    assert_eq!(app.settings.ai.provider, Provider::None);
    assert!(matches!(
        app.dialog,
        Some(Dialog::AgentPicker { selected: 1, .. })
    ));
    assert!(matches!(
        app.overview.as_ref().unwrap().ai,
        AiStatus::Disabled
    ));
}

#[test]
fn selecting_none_disables_an_inflight_analysis_without_leaving_the_pr() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.settings.ai.provider = Provider::Codex;
    app.opened(overview());
    app.overview.as_mut().unwrap().ai = AiStatus::Loading;
    press(&mut app, 'A').unwrap();
    press(&mut app, 'j').unwrap();
    press(&mut app, 'j').unwrap();
    let action = app.key(key(KeyCode::Enter)).unwrap();
    assert!(matches!(action, Action::DisableAi));
    assert_eq!(app.settings.ai.provider, Provider::None);
    assert!(matches!(
        app.overview.as_ref().unwrap().ai,
        AiStatus::Disabled
    ));
    assert_eq!(app.overview.as_ref().unwrap().metadata.id, overview().id);
}

#[test]
fn escape_from_either_pending_tab_invalidates_the_old_generation() {
    for tab in [PageTab::Overview, PageTab::Diff] {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(directory.path().into(), false);
        app.generation = 41;
        app.opened(overview());
        app.tab = tab;
        app.comments_loading = true;
        app.key(key(KeyCode::Esc)).unwrap();
        assert_eq!(app.generation, 42);
        assert!(app.overview.is_none());
        assert!(app.review.is_none());
        assert!(!app.diff_loading);
        assert!(!app.comments_loading);
        assert_eq!(app.repo.as_deref(), Some("example/service"));
    }
}

#[test]
fn mutating_operations_keep_escape_from_orphaning_the_active_review() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.opened(overview());
    app.busy = true;
    let generation = app.generation;
    app.key(key(KeyCode::Esc)).unwrap();
    assert!(app.overview.is_some());
    assert_eq!(app.generation, generation);
}

#[allow(dead_code)]
mod cli {
    include!("../src/cli.rs");

    #[cfg(test)]
    mod independent_background {
        use super::*;

        #[test]
        fn old_generation_results_do_not_replace_the_current_pr_or_clear_busy() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = App::new(directory.path().into(), false);
            app.generation = 7;
            app.opened(crate::overview());
            app.busy = true;
            app.comments_loading = true;
            app.overview.as_mut().unwrap().ai = app::AiStatus::Loading;
            let mut runtime = Runtime {
                provider: None,
                ask_request: 0,
                ask_cancel: None,
                ask_handle: None,
                generation: 7,
                ai_request: 9,
                pending_comments: None,
                pending_ai: None,
                ai_cancel: None,
                ai_handle: None,
                review_cancels: Vec::new(),
            };
            let (sender, _receiver) = mpsc::channel();
            for result in [
                JobResult::Overview {
                    generation: 6,
                    result: Ok(crate::overview()),
                },
                JobResult::Diff {
                    generation: 6,
                    result: Ok(Box::new(app::demo_data())),
                },
                JobResult::Comments {
                    generation: 6,
                    result: Ok(app::demo_data().comments),
                },
                JobResult::Analysis {
                    generation: 6,
                    request: 9,
                    result: Ok(Box::new(crate::analysis())),
                },
            ] {
                assert!(apply_background(result, &mut app, &sender, &mut runtime).is_none());
                assert!(app.review.is_none());
                assert!(app.diff_loading);
                assert!(app.comments_loading);
                assert!(app.busy);
                assert!(matches!(
                    app.overview.as_ref().unwrap().ai,
                    app::AiStatus::Loading
                ));
                assert!(runtime.pending_comments.is_none());
            }
        }

        #[test]
        fn changing_ai_request_rejects_an_old_result_for_the_same_pr() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = App::new(directory.path().into(), false);
            app.generation = 7;
            app.settings.ai.provider = Provider::Codex;
            app.opened(crate::overview());
            app.overview.as_mut().unwrap().ai = app::AiStatus::Loading;
            let mut runtime = Runtime {
                provider: None,
                ask_request: 0,
                ask_cancel: None,
                ask_handle: None,
                generation: 7,
                ai_request: 9,
                pending_comments: None,
                pending_ai: None,
                ai_cancel: None,
                ai_handle: None,
                review_cancels: Vec::new(),
            };
            let (sender, _receiver) = mpsc::channel();
            let stale = JobResult::Analysis {
                generation: 7,
                request: 8,
                result: Ok(Box::new(crate::analysis())),
            };
            apply_background(stale, &mut app, &sender, &mut runtime);
            assert!(matches!(
                app.overview.as_ref().unwrap().ai,
                app::AiStatus::Loading
            ));
            let current = JobResult::Analysis {
                generation: 7,
                request: 9,
                result: Ok(Box::new(crate::analysis())),
            };
            apply_background(current, &mut app, &sender, &mut runtime);
            assert!(matches!(
                app.overview.as_ref().unwrap().ai,
                app::AiStatus::Ready(_)
            ));
        }

        #[test]
        fn comments_that_arrive_before_the_diff_are_attached_when_it_arrives() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = App::new(directory.path().into(), false);
            app.opened(crate::overview());
            app.comments_loading = true;
            let mut runtime = Runtime::default();
            let (sender, _receiver) = mpsc::channel();
            let comments = app::demo_data().comments;
            apply_background(
                JobResult::Comments {
                    generation: 0,
                    result: Ok(comments.clone()),
                },
                &mut app,
                &sender,
                &mut runtime,
            );
            assert!(!app.comments_loading);
            assert!(app.review.is_none());
            let mut data = app::demo_data();
            data.comments.clear();
            apply_background(
                JobResult::Diff {
                    generation: 0,
                    result: Ok(Box::new(data)),
                },
                &mut app,
                &sender,
                &mut runtime,
            );
            let review = app.review.as_ref().unwrap();
            assert_eq!(review.data.comments.len(), comments.len());
            assert_eq!(review.data.comments[0].body, comments[0].body);
            assert!(runtime.pending_comments.is_none());
            assert_eq!(app.tab, app::PageTab::Overview);
        }

        #[test]
        fn background_diff_and_comments_do_not_unlock_a_foreground_mutation() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = App::new(directory.path().into(), false);
            app.opened(crate::overview());
            app.busy = true;
            let mut runtime = Runtime::default();
            let (sender, _receiver) = mpsc::channel();
            assert!(
                apply_background(
                    JobResult::Diff {
                        generation: 0,
                        result: Ok(Box::new(app::demo_data()))
                    },
                    &mut app,
                    &sender,
                    &mut runtime
                )
                .is_none()
            );
            assert!(app.busy);
            assert!(
                apply_background(
                    JobResult::Comments {
                        generation: 0,
                        result: Ok(Vec::new())
                    },
                    &mut app,
                    &sender,
                    &mut runtime
                )
                .is_none()
            );
            assert!(app.busy);
        }

        #[test]
        fn diff_failure_keeps_the_description_available_and_stops_its_spinner() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = App::new(directory.path().into(), false);
            app.opened(crate::overview());
            let mut runtime = Runtime::default();
            let (sender, _receiver) = mpsc::channel();
            apply_background(
                JobResult::Diff {
                    generation: 0,
                    result: Err(anyhow::anyhow!("Files unavailable")),
                },
                &mut app,
                &sender,
                &mut runtime,
            );
            assert!(!app.diff_loading);
            assert!(
                app.diff_error
                    .as_deref()
                    .unwrap()
                    .contains("Files unavailable")
            );
            assert!(app.review.is_none());
            assert_eq!(app.tab, app::PageTab::Overview);
            assert!(crate::screen(&mut app).contains("Retries currently ignore"));
        }

        #[test]
        fn leaving_a_pr_cancels_and_joins_ai_work_and_discards_pending_data() {
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            let handle = thread::spawn(move || {
                while !worker_cancel.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(1));
                }
            });
            let mut runtime = Runtime {
                provider: None,
                ask_request: 0,
                ask_cancel: None,
                ask_handle: None,
                generation: 7,
                pending_comments: Some(app::demo_data().comments),
                pending_ai: Some(true),
                ai_request: 9,
                ai_cancel: Some(cancel.clone()),
                ai_handle: Some(handle),
                review_cancels: Vec::new(),
            };
            runtime.sync(8);
            assert!(cancel.load(Ordering::Relaxed));
            assert!(runtime.ai_handle.is_none());
            assert!(runtime.ai_cancel.is_none());
            assert!(runtime.pending_comments.is_none());
            assert!(runtime.pending_ai.is_none());
            assert_eq!(runtime.ai_request, 10);
            assert_eq!(runtime.generation, 8);
        }

        #[test]
        fn dropping_the_runtime_does_not_leave_an_ai_worker_running() {
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            let (sender, receiver) = mpsc::channel();
            let handle = thread::spawn(move || {
                while !worker_cancel.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(1));
                }
                sender.send(()).unwrap();
            });
            {
                let _runtime = Runtime {
                    provider: None,
                    ask_request: 0,
                    ask_cancel: None,
                    ask_handle: None,
                    generation: 0,
                    pending_comments: None,
                    pending_ai: None,
                    ai_request: 0,
                    ai_cancel: Some(cancel.clone()),
                    ai_handle: Some(handle),
                    review_cancels: Vec::new(),
                };
            }
            assert!(cancel.load(Ordering::Relaxed));
            receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        }
    }
    #[cfg(test)]
    mod question_background {
        use super::*;
        fn answer() -> assistant::Answer {
            assistant::Answer {
                answer: "The deadline bounds the delay.".into(),
                references: vec![],
                unknowns: vec![],
                coverage: Default::default(),
            }
        }

        #[test]
        fn questions_and_preview_guides_ignore_stale_tokens_without_unlocking_mutations() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = App::new(directory.path().into(), false);
            app.generation = 7;
            app.busy = true;
            app.chat.pending = Some(("Why?".into(), "Entire PR".into()));
            let mut runtime = Runtime {
                generation: 7,
                ask_request: 3,
                ai_request: 4,
                provider: None,
                ask_cancel: None,
                ask_handle: None,
                pending_comments: None,
                pending_ai: None,
                ai_cancel: None,
                ai_handle: None,
                review_cancels: Vec::new(),
            };
            let (sender, _) = mpsc::channel();
            for (generation, request) in [(6, 3), (7, 2)] {
                apply_background(
                    JobResult::Answer {
                        generation,
                        request,
                        result: Ok(answer()),
                    },
                    &mut app,
                    &sender,
                    &mut runtime,
                );
                assert!(app.chat.pending.is_some());
                assert!(app.chat.turns.is_empty());
                assert!(app.busy);
            }
            apply_background(
                JobResult::Answer {
                    generation: 7,
                    request: 3,
                    result: Ok(answer()),
                },
                &mut app,
                &sender,
                &mut runtime,
            );
            assert_eq!(app.chat.turns.len(), 1);
            assert!(app.chat.pending.is_none());
            assert!(app.busy);
            let id = crate::overview().id;
            apply_background(
                JobResult::PreviewGuide {
                    generation: 7,
                    request: 3,
                    id: id.clone(),
                    revision: "old".into(),
                    result: Ok(Box::new(crate::analysis())),
                },
                &mut app,
                &sender,
                &mut runtime,
            );
            assert!(app.preview.guides.is_empty());
            apply_background(
                JobResult::PreviewGuide {
                    generation: 7,
                    request: 4,
                    id,
                    revision: "current".into(),
                    result: Ok(Box::new(crate::analysis())),
                },
                &mut app,
                &sender,
                &mut runtime,
            );
            assert_eq!(app.preview.guides.len(), 1);
            assert!(app.busy);
        }

        #[test]
        fn failed_question_restores_unsent_text_and_drop_cancels_the_worker() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = App::new(directory.path().into(), false);
            app.chat.pending = Some(("Original question".into(), "Entire PR".into()));
            app.dialog = Some(app::Dialog::Ask);
            let mut runtime = Runtime::default();
            let (sender, _) = mpsc::channel();
            apply_background(
                JobResult::Answer {
                    generation: 0,
                    request: 0,
                    result: Err(anyhow::anyhow!("offline")),
                },
                &mut app,
                &sender,
                &mut runtime,
            );
            assert_eq!(app.chat_input.text, "Original question");
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            runtime.ask_cancel = Some(cancel.clone());
            runtime.ask_handle = Some(thread::spawn(move || {
                while !worker_cancel.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(5));
                }
            }));
            drop(runtime);
            assert!(cancel.load(Ordering::Relaxed));
        }
    }
    #[cfg(test)]
    mod context_background {
        use super::*;
        #[test]
        fn file_context_and_browser_results_do_not_unlock_mutations_or_apply_stale_revisions() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, source) = crate::context_fixture(directory.path().into());
            app.generation = 9;
            app.busy = true;
            let pr = app.review.as_ref().unwrap().data.pr.clone();
            let filename = app.review.as_ref().unwrap().data.files[0].filename.clone();
            let mut runtime = Runtime::default();
            let (sender, _) = mpsc::channel();
            app.review.as_mut().unwrap().contexts[0].loading = true;
            apply_background(
                JobResult::FileContext {
                    generation: 8,
                    pr: Box::new(pr.clone()),
                    filename: filename.clone(),
                    result: Ok(source.clone()),
                },
                &mut app,
                &sender,
                &mut runtime,
            );
            assert!(app.review.as_ref().unwrap().contexts[0].loading);
            let mut old = pr.clone();
            old.head_sha = "0".repeat(40);
            apply_background(
                JobResult::FileContext {
                    generation: 9,
                    pr: Box::new(old),
                    filename: filename.clone(),
                    result: Ok(source.clone()),
                },
                &mut app,
                &sender,
                &mut runtime,
            );
            assert!(app.review.as_ref().unwrap().contexts[0].loading);
            apply_background(
                JobResult::FileContext {
                    generation: 9,
                    pr: Box::new(pr.clone()),
                    filename,
                    result: Ok(source),
                },
                &mut app,
                &sender,
                &mut runtime,
            );
            assert!(!app.review.as_ref().unwrap().contexts[0].loading);
            assert!(app.busy);
            apply_background(
                JobResult::Browser {
                    generation: 9,
                    id: pr.id,
                    result: Ok(()),
                },
                &mut app,
                &sender,
                &mut runtime,
            );
            assert!(app.busy);
            assert!(app.status.contains("on GitHub"));
        }
    }
}

fn browsing_app(root: std::path::PathBuf) -> App {
    let mut app = App::new(root, false);
    let metadata = overview();
    app.repo = Some(metadata.id.repo.clone());
    for number in [1, 2] {
        app.inbox.push(prpr::model::PrSummary {
            id: prpr::model::PrId {
                repo: metadata.id.repo.clone(),
                number,
            },
            title: format!("Review change {number}"),
            author: "author".into(),
            draft: number == 2,
            body: format!(
                "# Description {number}\n\nSelected body {number}\n\n{}",
                (0..150)
                    .map(|line| format!("- Item {number}/{line}\n"))
                    .collect::<String>()
            ),
        });
    }
    app
}

#[test]
fn list_preview_follows_selection_without_loading_diff_or_starting_an_agent() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = browsing_app(directory.path().into());
    assert!(screen(&mut app).contains("Selected body 1"));
    assert!(matches!(press(&mut app, 'j').unwrap(), Action::None));
    let output = screen(&mut app);
    assert!(output.contains("Selected body 2"));
    assert!(!output.contains("Selected body 1"));
    assert!(app.review.is_none() && app.overview.is_none());
    press(&mut app, 'l').unwrap();
    press(&mut app, 'g').unwrap();
    press(&mut app, 'e').unwrap();
    screen(&mut app);
    let end = app.preview.scroll;
    assert!(end > 0 && end < u16::MAX);
    press(&mut app, 'k').unwrap();
    screen(&mut app);
    assert_eq!(app.preview.scroll, end - 1);
    assert_eq!(app.selected_pr, 1);
    press(&mut app, 'p').unwrap();
    assert!(!screen(&mut app).contains("Selected body"));
    press(&mut app, 'p').unwrap();
    press(&mut app, 'P').unwrap();
    screen(&mut app);
    assert!(app.preview.actual_bottom);
    press(&mut app, 'h').unwrap();
    assert!(matches!(press(&mut app, 'o').unwrap(), Action::LoadPr(_)));
    app.opened(overview());
    assert_eq!(app.tab, PageTab::Diff);
}

#[test]
fn preview_is_responsive_and_keeps_description_readable_while_a_guide_loads() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = browsing_app(directory.path().into());
    app.settings.ai.provider = Provider::Codex;
    let id = app.inbox[0].id.clone();
    app.preview.guides.insert(
        app::preview_key(&id),
        app::PreviewGuide {
            revision: String::new(),
            status: AiStatus::Loading,
        },
    );
    app.preview.tab = app::PreviewTab::Guide;
    for (width, height) in [(48, 12), (80, 24), (140, 40)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let output = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(output.contains("PULL REQUESTS"));
        if height >= 24 {
            assert!(
                output.contains("Selected body 1"),
                "{width}x{height}: {output}"
            );
        }
    }
    app.preview.guides.insert(
        app::preview_key(&id),
        app::PreviewGuide {
            revision: "a".repeat(40),
            status: AiStatus::Ready(Box::new(analysis())),
        },
    );
    assert!(screen(&mut app).contains("Bound retry waits"));
}

#[test]
fn ask_and_explain_keep_ai_questions_separate_from_review_drafts() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.loaded(app::demo_data()).unwrap();
    app.settings.ai.provider = Provider::Codex;
    press(&mut app, 'x').unwrap();
    press(&mut app, 'x').unwrap();
    assert!(matches!(press(&mut app, 'Q').unwrap(), Action::None));
    for c in "Whyあ".chars() {
        press(&mut app, c).unwrap();
    }
    app.key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL))
        .unwrap();
    let action = app
        .key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        .unwrap();
    let Action::Ask { question, focus } = action else {
        panic!("Expected an AI question")
    };
    assert_eq!(question, "Why");
    assert_eq!(focus.unwrap().selected_lines.len(), 2);
    assert!(app.review.as_ref().unwrap().drafts.is_empty());
    app.chat.pending = Some((question, "selection".into()));
    assert!(matches!(
        app.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL))
            .unwrap(),
        Action::CancelAsk
    ));
    assert!(app.chat.pending.is_none());
    app.key(key(KeyCode::Esc)).unwrap();
    let action = press(&mut app, 'E').unwrap();
    assert!(matches!(action, Action::Ask { focus: Some(_), .. }));
    assert!(screen(&mut app).contains("AI chat · codex"));
}

#[test]
fn a_long_ai_response_opens_at_the_answer_and_remains_scrollable() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.loaded(app::demo_data()).unwrap();
    app.settings.ai.provider = Provider::Codex;
    app.chat.turns.push(prpr::assistant::Turn {
        question: "Why?".into(),
        scope: "Entire PR".into(),
        response: prpr::assistant::Answer {
            answer: format!("START_OF_ANSWER\n\n{}", "More explanation.\n\n".repeat(200)),
            references: vec![],
            unknowns: vec!["END_OF_RESPONSE".into()],
            coverage: Default::default(),
        },
    });
    app.dialog = Some(Dialog::Ask);
    assert!(screen(&mut app).contains("START_OF_ANSWER"));
    app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(!screen(&mut app).contains("START_OF_ANSWER"));
    assert!(app.chat_scroll > 0 && app.chat_scroll < u16::MAX);
}

#[test]
fn keeping_the_same_agent_preserves_chat_and_switching_agents_clears_it() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.settings.ai.provider = Provider::Codex;
    app.chat.error = Some("Preserved conversation state".into());
    app.dialog = Some(Dialog::AgentPicker {
        selected: 0,
        generate: false,
    });
    app.key(key(KeyCode::Enter)).unwrap();
    assert!(app.chat.error.is_some());
    app.dialog = Some(Dialog::AgentPicker {
        selected: 1,
        generate: false,
    });
    app.key(key(KeyCode::Enter)).unwrap();
    assert!(app.chat.error.is_none());
}

fn context_fixture(root: std::path::PathBuf) -> (App, prpr::context::FileContext) {
    let mut data = app::demo_data();
    data.comments.clear();
    data.files[0].patch = Some("@@ -9,3 +9,4 @@ first\n line 9\n-line 10\n+changed 10\n+extra 10\n line 11\n@@ -39,3 +40,3 @@ second\n line 39\n-line 40\n+changed 40\n line 41".into());
    data.files[0].additions = 3;
    data.files[0].deletions = 2;
    let base = (1..=60).map(|line| format!("line {line}\n")).collect();
    let head = (1..=60)
        .map(|line| match line {
            10 => "changed 10\nextra 10\n".into(),
            40 => "changed 40\n".into(),
            _ => format!("line {line}\n"),
        })
        .collect();
    let mut app = App::new(root, false);
    app.loaded(data).unwrap();
    app.settings.ai.provider = Provider::Codex;
    (app, prpr::context::FileContext { base, head })
}

#[test]
fn expanded_context_preserves_selection_editor_coordinates_and_original_review_patch() {
    let directory = tempfile::tempdir().unwrap();
    let (mut app, source) = context_fixture(directory.path().into());
    let review = app.review.as_mut().unwrap();
    let original = review.data.files[0].patch.clone();
    review.positions[0] = review.lines[0]
        .iter()
        .position(|row| row.old_line == Some(40) && row.new_line.is_none())
        .unwrap();
    let anchor = review.lines[0]
        .iter()
        .position(|row| row.old_line == Some(39))
        .unwrap();
    review.selection = Some(app::Selection {
        anchor,
        extending: true,
    });
    assert!(matches!(
        press(&mut app, '+').unwrap(),
        Action::LoadContext(0)
    ));
    app.review
        .as_mut()
        .unwrap()
        .accept_context(0, source)
        .unwrap();
    let review = app.review.as_ref().unwrap();
    assert_eq!(review.anchor(), Some((Side::Left, 40)));
    let (start, _) = review.selected_range().unwrap();
    assert_eq!(review.current_lines()[start].old_line, Some(39));
    assert_eq!(review.editor_target().unwrap().line, 40);
    assert_eq!(review.data.files[0].patch, original);
    press(&mut app, 'c').unwrap();
    assert!(
        matches!(&app.dialog, Some(Dialog::Compose { draft, .. }) if draft.start_line == Some(39) && draft.line == 40 && draft.side == Side::Left)
    );
    app.key(key(KeyCode::Esc)).unwrap();
    assert!(matches!(
        press(&mut app, 'E').unwrap(),
        Action::Ask { focus: Some(_), .. }
    ));
    app.key(key(KeyCode::Esc)).unwrap();
    press(&mut app, '-').unwrap();
    assert_eq!(
        app.review.as_ref().unwrap().anchor(),
        Some((Side::Left, 40))
    );
    press(&mut app, 'F').unwrap();
    assert_eq!(
        app.review.as_ref().unwrap().anchor(),
        Some((Side::Left, 40))
    );
    for (width, height) in [(48, 12), (80, 24), (140, 40)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    }
    assert!(screen(&mut app).contains("full file"));
}

#[test]
fn unchanged_expanded_lines_allow_ai_and_helix_but_not_github_review_drafts() {
    let directory = tempfile::tempdir().unwrap();
    let (mut app, source) = context_fixture(directory.path().into());
    assert!(matches!(
        press(&mut app, 'F').unwrap(),
        Action::LoadContext(0)
    ));
    app.review
        .as_mut()
        .unwrap()
        .accept_context(0, source)
        .unwrap();
    let review = app.review.as_mut().unwrap();
    review.positions[0] = review.lines[0]
        .iter()
        .position(|row| row.new_line == Some(1))
        .unwrap();
    assert_eq!(review.editor_target().unwrap().line, 1);
    press(&mut app, 'x').unwrap();
    assert!(
        press(&mut app, 'c')
            .err()
            .unwrap()
            .to_string()
            .contains("original diff")
    );
    let Action::Ask {
        focus: Some(focus), ..
    } = press(&mut app, 'E').unwrap()
    else {
        panic!("Missing AI focus")
    };
    assert_eq!(focus.selected_lines[0].new_line, Some(1));
    let review = app.review.as_ref().unwrap();
    assert!(
        review.ai_data(Some(&focus)).files[0]
            .patch
            .as_ref()
            .unwrap()
            .contains(&focus.patch)
    );
    assert!(review.drafts.is_empty());
    app.key(key(KeyCode::Esc)).unwrap();
    press(&mut app, 'F').unwrap();
    let review = app.review.as_ref().unwrap();
    assert!(review.position() < review.current_lines().len());
    assert!(review.selection.is_none());
}

#[test]
fn context_loading_keeps_other_files_navigable_and_reuses_loaded_source() {
    let directory = tempfile::tempdir().unwrap();
    let (mut app, source) = context_fixture(directory.path().into());
    assert!(matches!(
        press(&mut app, '+').unwrap(),
        Action::LoadContext(0)
    ));
    assert!(matches!(press(&mut app, '+').unwrap(), Action::None));
    press(&mut app, 'n').unwrap();
    let review = app.review.as_mut().unwrap();
    review.positions[1] = 3;
    review.accept_context(0, source).unwrap();
    assert_eq!(review.selected_file, 1);
    assert_eq!(review.position(), 3);
    press(&mut app, 'p').unwrap();
    assert!(matches!(press(&mut app, '+').unwrap(), Action::None));
    assert!(!app.review.as_ref().unwrap().contexts[0].loading);
}

#[test]
fn github_browser_shortcut_uses_selected_pr_in_all_views_and_does_not_intercept_text() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = browsing_app(directory.path().into());
    assert!(matches!(press(&mut app, 'B').unwrap(), Action::Browse(id) if id.number == 1));
    press(&mut app, 'j').unwrap();
    assert!(matches!(press(&mut app, 'B').unwrap(), Action::Browse(id) if id.number == 2));
    app.opened(overview());
    assert!(matches!(press(&mut app, 'B').unwrap(), Action::Browse(id) if id.number == 42));
    app.accept_diff(app::demo_data()).unwrap();
    app.tab = PageTab::Diff;
    assert!(
        matches!(press(&mut app, 'B').unwrap(), Action::Browse(id) if id.repo == "example/service")
    );
    app.dialog = Some(Dialog::Ask);
    assert!(matches!(press(&mut app, 'B').unwrap(), Action::None));
    assert_eq!(app.chat_input.text, "B");
    app.dialog = None;
    app.filtering = true;
    assert!(matches!(press(&mut app, 'B').unwrap(), Action::None));
    assert_eq!(app.filter, "B");
}

#[test]
fn local_review_keys_save_notes_reply_and_keep_github_submission_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    let mut data = app::demo_data();
    data.source = prpr::model::ReviewSource::Local {
        repository: directory.path().into(),
        label: "jj diff".into(),
    };
    app.local_spec = Some(prpr::local::Spec::Jj {
        repository: directory.path().into(),
        revision: None,
        from: None,
        to: None,
    });
    app.loaded(data).unwrap();
    let review = app.review.as_mut().unwrap();
    review.positions[0] = review.lines[0]
        .iter()
        .position(|row| row.new_line.is_some())
        .unwrap();
    press(&mut app, 'c').unwrap();
    for character in "Why??".chars() {
        press(&mut app, character).unwrap();
    }
    app.key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL))
        .unwrap();
    app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        .unwrap();
    let review = app.review.as_ref().unwrap();
    assert_eq!(review.notes[0].anchor.body, "Why?");
    assert!(review.drafts.is_empty());
    assert!(screen(&mut app).contains("Why?"));
    press(&mut app, 'N').unwrap();
    assert!(screen(&mut app).contains("Local notes"));
    press(&mut app, 'c').unwrap();
    for character in "Because.".chars() {
        press(&mut app, character).unwrap();
    }
    app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.review.as_ref().unwrap().notes[1].reply_to.is_some());
    for character in ['s', 'B', 'V', '#', 'w'] {
        assert!(matches!(press(&mut app, character).unwrap(), Action::None));
        assert!(app.dialog.is_none());
    }
    press(&mut app, 't').unwrap();
    assert!(!screen(&mut app).contains("Because."));
    press(&mut app, 'N').unwrap();
    press(&mut app, 'd').unwrap();
    assert!(app.review.as_ref().unwrap().notes.is_empty());
    assert!(
        prpr::notes::load(directory.path(), &app.review.as_ref().unwrap().data)
            .unwrap()
            .is_empty()
    );
}

fn ctrl(app: &mut App, character: char) -> anyhow::Result<Action> {
    app.key(KeyEvent::new(
        KeyCode::Char(character),
        KeyModifiers::CONTROL,
    ))
}

fn palette_app(root: &std::path::Path) -> App {
    let mut app = App::new(root.into(), false);
    app.settings.ai.provider = Provider::Codex;
    app.opened(overview());
    let mut data = app::demo_data();
    data.files = vec![
        prpr::model::ChangedFile {
            filename: "a.rs".into(),
            previous_filename: None,
            status: "modified".into(),
            additions: 2,
            deletions: 2,
            patch: Some("@@ -1 +1 @@\n-old\n+new\n@@ -10 +10 @@\n-before\n+after".into()),
        },
        prpr::model::ChangedFile {
            filename: "b.bin".into(),
            previous_filename: None,
            status: "modified".into(),
            additions: 0,
            deletions: 0,
            patch: None,
        },
        prpr::model::ChangedFile {
            filename: "c.rs".into(),
            previous_filename: None,
            status: "modified".into(),
            additions: 1,
            deletions: 1,
            patch: Some("@@ -3 +3 @@\n-a\n+b".into()),
        },
    ];
    app.accept_diff(data).unwrap();
    app.tab = PageTab::Diff;
    app
}

fn palette_run(app: &mut App, query: &str) -> Action {
    ctrl(app, 'k').unwrap();
    app.paste(query);
    app.key(key(KeyCode::Enter)).unwrap()
}

#[test]
fn palette_search_uses_shared_actions_and_never_submits_without_the_existing_preview() {
    let root = tempfile::tempdir().unwrap();
    let mut app = palette_app(root.path());
    press(&mut app, 'x').unwrap();
    let position = app.review.as_ref().unwrap().position();
    assert!(matches!(
        palette_run(&mut app, "file sidebar"),
        Action::None
    ));
    assert!(!app.file_sidebar);
    assert_eq!(app.review.as_ref().unwrap().position(), position);
    assert!(app.review.as_ref().unwrap().selection.is_some());
    assert!(!screen(&mut app).contains("CHANGES"));
    assert!(matches!(palette_run(&mut app, "next hunk"), Action::None));
    assert_eq!(app.review.as_ref().unwrap().position(), 3);
    assert!(matches!(palette_run(&mut app, "prev hunk"), Action::None));
    assert_eq!(app.review.as_ref().unwrap().position(), 0);
    assert!(matches!(
        palette_run(&mut app, "Explain diff"),
        Action::Ask { .. }
    ));
    assert!(matches!(app.dialog, Some(Dialog::Ask)));
    app.key(key(KeyCode::Esc)).unwrap();
    assert!(matches!(
        palette_run(&mut app, "review summary"),
        Action::None
    ));
    assert!(matches!(app.dialog, Some(Dialog::Submit { .. })));
    app.paste("A review summary");
    assert!(matches!(ctrl(&mut app, 's').unwrap(), Action::None));
    assert!(matches!(app.dialog, Some(Dialog::Confirm { .. })));
}

#[test]
fn palette_handles_disabled_actions_empty_results_paste_and_modal_text_without_binding_conflicts() {
    let root = tempfile::tempdir().unwrap();
    let mut app = palette_app(root.path());
    app.settings.ai.provider = Provider::None;
    palette_run(&mut app, "explain diff");
    assert!(matches!(app.dialog, Some(Dialog::Palette { .. })));
    assert!(screen(&mut app).contains("Choose an AI agent first"));
    ctrl(&mut app, 'u').unwrap();
    app.paste("no matching command 日本語");
    app.key(key(KeyCode::Enter)).unwrap();
    assert!(screen(&mut app).contains("No matching commands"));
    ctrl(&mut app, 'h').unwrap();
    assert!(
        matches!(&app.dialog, Some(Dialog::Palette { input, .. }) if input.text.ends_with("日本"))
    );
    ctrl(&mut app, 'k').unwrap();
    assert!(app.dialog.is_none());
    ctrl(&mut app, 'k').unwrap();
    app.key(key(KeyCode::Down)).unwrap();
    app.paste("NEXT\nHUNK");
    assert!(
        matches!(&app.dialog, Some(Dialog::Palette { input, selected: 0 }) if input.text == "NEXT HUNK")
    );
    app.key(key(KeyCode::Enter)).unwrap();
    assert_eq!(app.review.as_ref().unwrap().position(), 3);
    press(&mut app, 'x').unwrap();
    press(&mut app, 'C').unwrap();
    app.paste("Keep this text");
    ctrl(&mut app, 'b').unwrap();
    ctrl(&mut app, 'k').unwrap();
    assert!(app.file_sidebar);
    assert!(
        matches!(&app.dialog, Some(Dialog::Note { input, .. }) if input.text == "Keep this text")
    );
}

#[test]
fn palette_tracks_inbox_overview_local_and_busy_contexts() {
    let root = tempfile::tempdir().unwrap();
    let mut app = palette_app(root.path());
    app.tab = PageTab::Overview;
    assert!(app::commands::available(&app, "hunk").is_empty());
    assert!(matches!(palette_run(&mut app, "show diff"), Action::None));
    assert_eq!(app.tab, PageTab::Diff);
    app.busy = true;
    palette_run(&mut app, "helix");
    assert!(matches!(app.dialog, Some(Dialog::Palette { .. })));
    assert!(screen(&mut app).contains("Wait for the current operation"));
    app.key(key(KeyCode::Esc)).unwrap();
    app.busy = false;
    app.local_spec = Some(prpr::local::Spec::Jj {
        repository: root.path().into(),
        revision: None,
        from: None,
        to: None,
    });
    app.review.as_mut().unwrap().data.source = prpr::model::ReviewSource::Local {
        repository: root.path().into(),
        label: "local".into(),
    };
    assert!(app::commands::available(&app, "github").is_empty());
    assert!(app::commands::available(&app, "checkout").is_empty());
    assert!(matches!(
        palette_run(&mut app, "refresh"),
        Action::RefreshLocal
    ));
    let mut inbox = App::new(root.path().into(), false);
    inbox.inbox = vec![prpr::model::PrSummary {
        id: prpr::model::PrId {
            repo: "owner/repo".into(),
            number: 4,
        },
        title: "Fix behavior".into(),
        body: String::new(),
        author: "someone".into(),
        draft: false,
    }];
    inbox.filtering = true;
    inbox.filter = "fix".into();
    assert!(matches!(palette_run(&mut inbox, "selected pr"), Action::LoadPr(id) if id.number == 4));
    assert!(!inbox.filtering);
    assert_eq!(inbox.filter, "fix");
}

#[test]
fn hunk_navigation_crosses_files_skips_missing_patches_and_stays_with_active_selection() {
    let root = tempfile::tempdir().unwrap();
    let mut app = palette_app(root.path());
    press(&mut app, ']').unwrap();
    assert_eq!(app.review.as_ref().unwrap().position(), 3);
    press(&mut app, 'v').unwrap();
    press(&mut app, ']').unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 0);
    press(&mut app, ';').unwrap();
    press(&mut app, ']').unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 2);
    assert_eq!(app.review.as_ref().unwrap().position(), 0);
    press(&mut app, ']').unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 2);
    press(&mut app, '[').unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 0);
    assert_eq!(app.review.as_ref().unwrap().position(), 3);
    press(&mut app, '[').unwrap();
    press(&mut app, '[').unwrap();
    assert_eq!(app.review.as_ref().unwrap().position(), 0);
}

#[test]
fn original_hunks_remain_navigable_when_context_merges_them_or_shows_the_full_file() {
    let root = tempfile::tempdir().unwrap();
    let mut app = palette_app(root.path());
    let source = prpr::context::FileContext {
        base: (1..=10)
            .map(|line| match line {
                1 => "old\n".to_owned(),
                10 => "before\n".into(),
                _ => format!("same {line}\n"),
            })
            .collect(),
        head: (1..=10)
            .map(|line| match line {
                1 => "new\n".to_owned(),
                10 => "after\n".into(),
                _ => format!("same {line}\n"),
            })
            .collect(),
    };
    app.key(key(KeyCode::Char('F'))).unwrap();
    app.review
        .as_mut()
        .unwrap()
        .accept_context(0, source)
        .unwrap();
    let positions = app.review.as_ref().unwrap().contexts[0]
        .view
        .as_ref()
        .unwrap()
        .hunk_positions
        .clone();
    assert_eq!(positions.len(), 2);
    assert!(positions[0] < positions[1]);
    app.review.as_mut().unwrap().positions[0] = 0;
    press(&mut app, ']').unwrap();
    assert_eq!(app.review.as_ref().unwrap().position(), positions[0]);
    press(&mut app, ']').unwrap();
    assert_eq!(app.review.as_ref().unwrap().position(), positions[1]);
    press(&mut app, '-').unwrap();
    app.review.as_mut().unwrap().positions[0] = 0;
    press(&mut app, '+').unwrap();
    let review = app.review.as_ref().unwrap();
    assert_eq!(
        review
            .current_lines()
            .iter()
            .filter(|row| row.kind == prpr::diff::LineKind::Hunk)
            .count(),
        1
    );
    let second = review.contexts[0].view.as_ref().unwrap().hunk_positions[1];
    press(&mut app, ']').unwrap();
    assert_eq!(app.review.as_ref().unwrap().position(), second);
    assert_eq!(app.review.as_ref().unwrap().selected_file, 0);
}

#[test]
fn sidebar_and_palette_render_at_small_sizes_and_sidebar_focus_can_be_restored() {
    let root = tempfile::tempdir().unwrap();
    let mut app = palette_app(root.path());
    ctrl(&mut app, 'b').unwrap();
    press(&mut app, 'h').unwrap();
    assert!(app.file_sidebar);
    assert_eq!(app.review.as_ref().unwrap().focus, Focus::Files);
    ctrl(&mut app, 'b').unwrap();
    assert_eq!(app.review.as_ref().unwrap().focus, Focus::Diff);
    for (width, height) in [(140, 40), (80, 24), (45, 14), (20, 5)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        ctrl(&mut app, 'k').unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        for _ in 0..50 {
            app.key(key(KeyCode::Down)).unwrap();
        }
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        app.key(key(KeyCode::Esc)).unwrap();
    }
    ctrl(&mut app, 'k').unwrap();
    app.paste("sidebar");
    let text = screen(&mut app);
    assert!(text.contains("Toggle file sidebar"));
    assert!(text.contains("Ctrl-b"));
    app.key(key(KeyCode::Esc)).unwrap();
    let data = app.review.as_ref().unwrap().data.clone();
    app.loaded(data).unwrap();
    assert!(!app.file_sidebar);
}

#[test]
fn configured_normal_shortcuts_replace_defaults_and_preserve_multi_key_navigation() {
    let root = tempfile::tempdir().unwrap();
    let mut app = palette_app(root.path());
    app.settings.keybindings.0.extend([
        ("command_palette".into(), vec!["Ctrl-p".into(), "F2".into()]),
        ("toggle_sidebar".into(), vec!["Space b".into()]),
        ("next_hunk".into(), vec!["Alt-j".into()]),
        ("previous_hunk".into(), vec!["g p".into()]),
    ]);
    app.settings.keybindings.validate().unwrap();
    ctrl(&mut app, 'k').unwrap();
    assert!(app.dialog.is_none());
    ctrl(&mut app, 'p').unwrap();
    app.paste("sidebar");
    assert!(screen(&mut app).contains("Space b"));
    assert!(!screen(&mut app).contains("Ctrl-b"));
    app.key(key(KeyCode::Enter)).unwrap();
    assert!(!app.file_sidebar);
    ctrl(&mut app, 'b').unwrap();
    assert!(!app.file_sidebar);
    press(&mut app, ' ').unwrap();
    assert!(app.goto_pending);
    assert!(screen(&mut app).contains("Space …"));
    press(&mut app, 'b').unwrap();
    assert!(app.file_sidebar);
    press(&mut app, ']').unwrap();
    assert_eq!(app.review.as_ref().unwrap().position(), 0);
    app.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.review.as_ref().unwrap().position(), 3);
    press(&mut app, 'g').unwrap();
    press(&mut app, 'p').unwrap();
    assert_eq!(app.review.as_ref().unwrap().position(), 0);
    assert!(app.pending_keys.is_empty());
    app.key(key(KeyCode::F(2))).unwrap();
    assert!(matches!(app.dialog, Some(Dialog::Palette { .. })));
    app.key(key(KeyCode::Down)).unwrap();
    ctrl(&mut app, 'p').unwrap();
    assert!(matches!(
        app.dialog,
        Some(Dialog::Palette { selected: 0, .. })
    ));
    app.key(key(KeyCode::F(2))).unwrap();
    assert!(app.dialog.is_none());
}

#[test]
fn unbound_shortcuts_leave_palette_actions_available_and_canonical_keys_cannot_be_redirected() {
    let root = tempfile::tempdir().unwrap();
    let mut app = palette_app(root.path());
    app.settings
        .keybindings
        .0
        .insert("open_file".into(), vec![]);
    app.settings
        .keybindings
        .0
        .insert("previous_hunk".into(), vec!["g p".into()]);
    app.settings
        .keybindings
        .0
        .insert("full_file".into(), vec!["[".into()]);
    app.settings.keybindings.validate().unwrap();
    assert!(matches!(press(&mut app, 'o').unwrap(), Action::None));
    assert!(matches!(press(&mut app, 'e').unwrap(), Action::None));
    press(&mut app, 'g').unwrap();
    assert!(matches!(press(&mut app, 'f').unwrap(), Action::None));
    assert!(matches!(
        palette_run(&mut app, "helix"),
        Action::OpenEditor(_)
    ));
    app.review.as_mut().unwrap().positions[0] = 3;
    palette_run(&mut app, "previous hunk");
    assert_eq!(app.review.as_ref().unwrap().position(), 0);
    assert!(!app.review.as_ref().unwrap().contexts[0].expansion.full);
    app.settings
        .keybindings
        .0
        .insert("open_file".into(), vec!["o".into()]);
    app.settings.keybindings.validate().unwrap();
    assert!(matches!(
        press(&mut app, 'o').unwrap(),
        Action::OpenEditor(_)
    ));
}

#[test]
fn editor_controls_and_provider_saves_preserve_custom_bindings() {
    let root = tempfile::tempdir().unwrap();
    let mut app = palette_app(root.path());
    app.settings
        .keybindings
        .0
        .insert("choose_agent".into(), vec!["Ctrl-a".into()]);
    app.settings
        .keybindings
        .0
        .insert("next_hunk".into(), vec!["Alt-j".into(), "Ctrl-s".into()]);
    app.settings.keybindings.validate().unwrap();
    app.config_path = Some(root.path().join("config.toml"));
    ctrl(&mut app, 'a').unwrap();
    press(&mut app, 'j').unwrap();
    app.key(key(KeyCode::Enter)).unwrap();
    let saved = settings::load(app.config_path.as_ref().unwrap()).unwrap();
    assert_eq!(saved.ai.provider, Provider::Claude);
    assert_eq!(saved.keybindings, app.settings.keybindings);
    let mut externally_edited = saved;
    externally_edited
        .keybindings
        .0
        .insert("command_palette".into(), vec!["Ctrl-p".into()]);
    settings::save(app.config_path.as_ref().unwrap(), &externally_edited).unwrap();
    ctrl(&mut app, 'a').unwrap();
    app.key(key(KeyCode::Enter)).unwrap();
    assert_eq!(
        settings::load(app.config_path.as_ref().unwrap())
            .unwrap()
            .keybindings
            .keys("command_palette"),
        ["Ctrl-p"]
    );
    assert_eq!(app.settings.keybindings.keys("command_palette"), ["Ctrl-k"]);
    app.key(key(KeyCode::F(1))).unwrap();
    app.execute_binding("help").unwrap();
    assert!(screen(&mut app).contains("Ctrl-a"));
    app.key(key(KeyCode::Esc)).unwrap();
    press(&mut app, 'x').unwrap();
    press(&mut app, 'C').unwrap();
    app.paste("Keep text");
    app.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT))
        .unwrap();
    ctrl(&mut app, 'h').unwrap();
    assert!(matches!(&app.dialog, Some(Dialog::Note { input, .. }) if input.text == "Keep tex"));
    ctrl(&mut app, 's').unwrap();
    assert!(app.dialog.is_none());
    assert_eq!(
        app.review.as_ref().unwrap().notes[0].anchor.body,
        "Keep tex"
    );
}

#[test]
fn complete_chords_take_precedence_over_global_shortcut_suffixes() {
    let root = tempfile::tempdir().unwrap();
    let mut app = palette_app(root.path());
    app.settings
        .keybindings
        .0
        .insert("toggle_sidebar".into(), vec!["Space Ctrl-k".into()]);
    app.settings.keybindings.validate().unwrap();
    press(&mut app, ' ').unwrap();
    ctrl(&mut app, 'k').unwrap();
    assert!(!app.file_sidebar);
    assert!(app.dialog.is_none());
    press(&mut app, 'g').unwrap();
    ctrl(&mut app, 'k').unwrap();
    assert!(matches!(app.dialog, Some(Dialog::Palette { .. })));
    assert!(app.pending_keys.is_empty());
    app.key(key(KeyCode::Esc)).unwrap();
    press(&mut app, 'g').unwrap();
    assert!(matches!(press(&mut app, 'E').unwrap(), Action::Ask { .. }));
}

#[test]
fn ai_review_key_starts_once_reports_progress_and_opens_the_finished_review() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.settings.ai.provider = Provider::None;
    app.opened(overview());
    assert!(press(&mut app, 'R').is_err(), "the diff is not loaded yet");
    app.accept_diff(app::demo_data()).unwrap();
    assert!(press(&mut app, 'R').is_err(), "an agent must be chosen");
    app.settings.ai.provider = Provider::Claude;
    assert!(matches!(press(&mut app, 'R').unwrap(), Action::AiReview));
    let key = app::ai_review_key(&app.review.as_ref().unwrap().data.pr);
    app.ai_reviews
        .insert(key.clone(), app::AiReviewStatus::Running(Provider::Claude));
    assert!(matches!(press(&mut app, 'R').unwrap(), Action::None));
    assert!(app.dialog.is_none());
    app.ai_reviews.insert(
        key,
        app::AiReviewStatus::Ready {
            provider: Provider::Claude,
            text: "## Findings\n\nNo issues found.".into(),
            notes: 0,
            unanchored: 0,
            anchored: true,
        },
    );
    press(&mut app, 'R').unwrap();
    assert!(matches!(app.dialog, Some(Dialog::AiReview { .. })));
    assert!(screen(&mut app).contains("No issues found."));
    assert!(matches!(press(&mut app, 'r').unwrap(), Action::AiReview));
    app.dialog = None;
    app.local_spec = Some(prpr::local::Spec::Jj {
        repository: directory.path().into(),
        revision: None,
        from: None,
        to: None,
    });
    assert!(
        press(&mut app, 'R').is_err(),
        "local diffs have no provider PR review"
    );
}

#[test]
fn chat_panel_toggles_from_every_tab_and_attaches_the_cursor_when_sent() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.opened(overview());
    app.accept_diff(app::demo_data()).unwrap();
    app.settings.ai.provider = Provider::Codex;
    let toggle = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
    for tab in [PageTab::Overview, PageTab::Guide, PageTab::Diff] {
        app.tab = tab;
        app.key(toggle).unwrap();
        assert!(matches!(app.dialog, Some(Dialog::Ask)));
        assert!(screen(&mut app).contains("AI chat · codex"));
        app.key(toggle).unwrap();
        assert!(app.dialog.is_none() && !app.chat_open);
        assert!(!screen(&mut app).contains("AI chat · codex"));
    }

    app.key(toggle).unwrap();
    for c in "Why".chars() {
        press(&mut app, c).unwrap();
    }
    app.key(key(KeyCode::Esc)).unwrap();
    assert!(app.dialog.is_none() && app.chat_open);
    let docked = screen(&mut app);
    assert!(docked.contains("AI chat · codex") && docked.contains("Why"));
    let before = app.chat_focus().unwrap().unwrap().label;
    press(&mut app, 'x').unwrap();
    let after = app.chat_focus().unwrap().unwrap().label;
    assert_ne!(before, after);
    let docked = screen(&mut app);
    assert!(
        docked.contains("Attached: src/client.rs"),
        "{after}\n{docked}"
    );

    press(&mut app, 'Q').unwrap();
    let Action::Ask { question, focus } = app
        .key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        .unwrap()
    else {
        panic!("Expected an AI question")
    };
    assert_eq!(question, "Why");
    assert_eq!(focus.unwrap().label, after);
    assert!(app.chat_input.text.is_empty());

    app.key(key(KeyCode::Esc)).unwrap();
    app.tab = PageTab::Overview;
    assert!(screen(&mut app).contains("Attached: entire changeset"));
}

#[test]
fn checkout_runs_from_overview_and_ai_guide() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.opened(overview());
    for tab in [PageTab::Overview, PageTab::Guide] {
        app.tab = tab;
        assert!(matches!(press(&mut app, 'w').unwrap(), Action::None));
        assert!(app.status.contains("Wait for the diff"));
    }
    app.accept_diff(app::demo_data()).unwrap();
    let repo = app.review.as_ref().unwrap().data.pr.id.repo.clone();
    app.checkout = Some((directory.path().join("launch"), repo));
    for tab in [PageTab::Overview, PageTab::Guide] {
        app.tab = tab;
        assert!(matches!(
            press(&mut app, 'w').unwrap(),
            Action::CheckoutHere { .. }
        ));
    }
    app.tab = PageTab::Overview;
    app.dialog = Some(Dialog::Palette {
        input: app::TextInput::new("check out".into()),
        selected: 0,
    });
    assert!(matches!(
        app.key(key(KeyCode::Enter)).unwrap(),
        Action::CheckoutHere { .. }
    ));
}

#[test]
fn choosing_an_agent_generates_a_guide_only_when_asked_or_automatic() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().join("state"), false);
    app.opened(overview());
    assert!(!app.settings.ai.auto_explain);
    press(&mut app, 'A').unwrap();
    press(&mut app, 'k').unwrap();
    assert!(matches!(
        app.key(key(KeyCode::Enter)).unwrap(),
        Action::None
    ));
    assert_eq!(app.settings.ai.provider, Provider::Claude);
    app.settings.ai.auto_explain = true;
    press(&mut app, 'A').unwrap();
    press(&mut app, 'k').unwrap();
    assert!(matches!(
        app.key(key(KeyCode::Enter)).unwrap(),
        Action::Explain { force: false }
    ));
}
