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
use pr_review::{
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
    assert_eq!(app.tab, PageTab::Diff);
    assert!(app.review.is_none());
    app.key(key(KeyCode::Tab)).unwrap();
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
    let review = app.review.as_ref().unwrap();
    assert_eq!(review.current_file().unwrap().filename, "src/client.rs");
    assert_eq!(review.current_lines()[review.position()].old_line, Some(20));
    assert_eq!(review.focus, Focus::Diff);
    assert_eq!(app.tab, PageTab::Diff);

    press(&mut app, '1').unwrap();
    press(&mut app, 'n').unwrap();
    app.key(key(KeyCode::Enter)).unwrap();
    let review = app.review.as_ref().unwrap();
    assert_eq!(review.current_lines()[review.position()].new_line, Some(24));

    press(&mut app, '1').unwrap();
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
    assert!(app.key(key(KeyCode::Enter)).is_err());
    assert_eq!(app.tab, PageTab::Overview);
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
        Some(Dialog::AgentPicker { selected: 0 })
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
        Some(Dialog::AgentPicker { selected: 1 })
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
                generation: 7,
                ai_request: 9,
                pending_comments: None,
                pending_ai: None,
                ai_cancel: None,
                ai_handle: None,
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
                generation: 7,
                ai_request: 9,
                pending_comments: None,
                pending_ai: None,
                ai_cancel: None,
                ai_handle: None,
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
                generation: 7,
                pending_comments: Some(app::demo_data().comments),
                pending_ai: Some(true),
                ai_request: 9,
                ai_cancel: Some(cancel.clone()),
                ai_handle: Some(handle),
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
                    generation: 0,
                    pending_comments: None,
                    pending_ai: None,
                    ai_request: 0,
                    ai_cancel: Some(cancel.clone()),
                    ai_handle: Some(handle),
                };
            }
            assert!(cancel.load(Ordering::Relaxed));
            receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        }
    }
}
