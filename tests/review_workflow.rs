#[allow(dead_code)]
#[path = "../src/app.rs"]
mod app;
#[allow(dead_code)]
#[path = "../src/store.rs"]
mod store;

use app::{Action, App, Dialog};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn numeric_dialog_uses_current_repository_even_with_an_empty_inbox() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().into(), false);
    app.repo = Some("owner/current".into());
    app.key(key(KeyCode::Char('#'))).unwrap();
    for character in "123".chars() {
        app.key(key(KeyCode::Char(character))).unwrap();
    }
    match app.key(key(KeyCode::Enter)).unwrap() {
        Action::LoadPr(id) => {
            assert_eq!(id.repo, "owner/current");
            assert_eq!(id.number, 123);
        }
        _ => panic!("The number dialog must request a PR in the selected repository"),
    }
}

#[test]
fn invalid_pasted_number_keeps_the_dialog_and_does_not_switch_repository() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().into(), false);
    app.repo = Some("owner/current".into());
    for text in [
        "0",
        "-12",
        "12\n34",
        "https://github.com/other/repo/pull/123",
    ] {
        app.dialog = Some(Dialog::OpenPr {
            input: app::TextInput::new(text.into()),
        });
        assert!(app.key(key(KeyCode::Enter)).is_err());
        match &app.dialog {
            Some(Dialog::OpenPr { input }) => assert_eq!(input.text, text),
            _ => panic!("Invalid pasted input must remain editable"),
        }
        assert_eq!(app.repo.as_deref(), Some("owner/current"));
    }
    app.dialog = Some(Dialog::OpenPr {
        input: app::TextInput::new(" 123\n".into()),
    });
    assert!(
        matches!(app.key(key(KeyCode::Enter)).unwrap(), Action::LoadPr(id) if id.number == 123 && id.repo == "owner/current")
    );
}

#[test]
fn comment_visibility_never_changes_draft_or_editor_coordinates() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().into(), false);
    app.loaded(app::demo_data()).unwrap();
    for _ in 0..2 {
        app.key(key(KeyCode::Char('j'))).unwrap();
    }
    let review = app.review.as_ref().unwrap();
    let position = review.position();
    let anchor = review.anchor();
    assert_eq!(anchor, Some((prpr::model::Side::Left, 19)));
    let editor = review.editor_target().unwrap();
    for _ in 0..3 {
        app.key(key(KeyCode::Char('t'))).unwrap();
        let review = app.review.as_ref().unwrap();
        assert_eq!(review.position(), position);
        assert_eq!(review.anchor(), anchor);
        let toggled_editor = review.editor_target().unwrap();
        assert_eq!(toggled_editor.file.filename, editor.file.filename);
        assert_eq!(toggled_editor.line, editor.line);
        assert_eq!(toggled_editor.side, editor.side);
    }
    app.key(key(KeyCode::Char('c'))).unwrap();
    match &app.dialog {
        Some(Dialog::Compose { draft, .. }) => {
            assert_eq!(draft.path, editor.file.filename);
            assert_eq!(draft.line, editor.line);
            assert_eq!(draft.side, editor.side);
        }
        _ => panic!("Toggling inline comments must preserve the source comment target"),
    }
}

#[test]
fn failed_save_keeps_unsaved_comment_available_for_retry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("blocked");
    let mut app = App::new(root.clone(), false);
    app.loaded(app::demo_data()).unwrap();
    app.key(key(KeyCode::Char('j'))).unwrap();
    app.key(key(KeyCode::Char('c'))).unwrap();
    for character in "Please account for the deadline.".chars() {
        app.key(key(KeyCode::Char(character))).unwrap();
    }
    std::fs::write(&root, "a file prevents creation of the draft directory").unwrap();

    let result = app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(result.is_err());
    match &app.dialog {
        Some(Dialog::Compose { input, .. }) => {
            assert_eq!(input.text, "Please account for the deadline.");
        }
        _ => panic!("A failed save must preserve the comment editor and its text"),
    }
    assert!(app.review.as_ref().unwrap().drafts.is_empty());

    std::fs::remove_file(&root).unwrap();
    app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.review.as_ref().unwrap().drafts.len(), 1);
}

#[test]
fn busy_review_cannot_switch_revision_or_start_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().into(), false);
    app.loaded(app::demo_data()).unwrap();
    let initial_id = app.review.as_ref().unwrap().data.pr.id.clone();
    app.busy = true;

    for code in [
        KeyCode::Esc,
        KeyCode::Char('r'),
        KeyCode::Char('w'),
        KeyCode::Char('e'),
        KeyCode::Char('s'),
        KeyCode::Char('c'),
        KeyCode::Char('q'),
    ] {
        assert!(matches!(app.key(key(code)).unwrap(), Action::None));
        assert_eq!(app.review.as_ref().unwrap().data.pr.id, initial_id);
        assert!(app.dialog.is_none());
    }
}

#[test]
fn asynchronous_editor_request_keeps_the_original_file_and_line() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().into(), false);
    app.settings.editor.open_in = prpr::settings::OpenIn::Workspace;
    app.loaded(app::demo_data()).unwrap();
    let target = match app.key(key(KeyCode::Char('e'))).unwrap() {
        Action::Prepare {
            open_editor: Some(target),
            ..
        } => target,
        _ => panic!("Opening Helix must capture the selected diff location"),
    };
    app.busy = true;
    app.key(key(KeyCode::Char('n'))).unwrap();
    app.key(key(KeyCode::Char('j'))).unwrap();
    assert_eq!(target.file.filename, "src/client.rs");
    assert_eq!(target.line, 18);
    assert_eq!(target.side, prpr::model::Side::Right);
    assert_eq!(
        app.review
            .as_ref()
            .unwrap()
            .current_file()
            .unwrap()
            .filename,
        "tests/deadline.rs"
    );
}

#[test]
fn removed_file_without_a_patch_opens_the_base_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().into(), false);
    let mut data = app::demo_data();
    data.files[0].status = "removed".into();
    data.files[0].patch = None;
    app.loaded(data).unwrap();
    let target = app.review.as_ref().unwrap().editor_target().unwrap();
    assert_eq!(target.side, prpr::model::Side::Left);
    assert_eq!(target.line, 1);
}

#[test]
fn drafts_from_a_different_diff_base_are_not_reused() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = App::new(dir.path().into(), false);
    first.loaded(app::demo_data()).unwrap();
    first.key(key(KeyCode::Char('j'))).unwrap();
    first.key(key(KeyCode::Char('c'))).unwrap();
    first.key(key(KeyCode::Char('A'))).unwrap();
    first
        .key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        .unwrap();
    let old_pr = first.review.as_ref().unwrap().data.pr.clone();

    let mut updated = app::demo_data();
    updated.pr.base_sha = "9".repeat(40);
    let mut reopened = App::new(dir.path().into(), false);
    if reopened.loaded(updated).is_ok() {
        assert!(reopened.review.as_ref().unwrap().drafts.is_empty());
    }
    assert_eq!(store::load(dir.path(), &old_pr).unwrap().len(), 1);
}

#[test]
fn unresolved_submission_blocks_retry_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let data = app::demo_data();
    store::begin_submission(dir.path(), &data.pr, &[], "First attempt", "COMMENT").unwrap();
    assert!(store::begin_submission(dir.path(), &data.pr, &[], "Retry", "COMMENT").is_err());
    assert_eq!(
        store::attempt(dir.path(), &data.pr).unwrap().unwrap().body,
        "First attempt"
    );

    let mut restarted = App::new(dir.path().into(), false);
    restarted.loaded(data.clone()).unwrap();
    assert!(matches!(
        restarted.key(key(KeyCode::Char('s'))).unwrap(),
        Action::None
    ));
    assert!(restarted.dialog.is_none());
    restarted.key(key(KeyCode::Char('u'))).unwrap();
    assert!(matches!(restarted.dialog, Some(Dialog::Recovery { .. })));
    restarted.key(key(KeyCode::Char('n'))).unwrap();
    assert!(store::attempt(dir.path(), &data.pr).unwrap().is_none());
    restarted.key(key(KeyCode::Char('s'))).unwrap();
    assert!(matches!(restarted.dialog, Some(Dialog::Submit { .. })));
}

#[test]
fn marking_an_uncertain_review_sent_preserves_new_local_drafts() {
    use prpr::model::{DraftComment, Side};

    let dir = tempfile::tempdir().unwrap();
    let data = app::demo_data();
    let submitted = DraftComment {
        start_line: None,
        start_side: None,
        path: "src/client.rs".into(),
        line: 18,
        side: Side::Right,
        body: "Already submitted comment".into(),
    };
    let added_later = DraftComment {
        start_line: None,
        start_side: None,
        path: "src/client.rs".into(),
        line: 19,
        side: Side::Right,
        body: "New comment written after the uncertain response".into(),
    };
    store::begin_submission(
        dir.path(),
        &data.pr,
        std::slice::from_ref(&submitted),
        "Summary",
        "COMMENT",
    )
    .unwrap();
    store::save(dir.path(), &data.pr, &[submitted, added_later.clone()]).unwrap();

    let mut app = App::new(dir.path().into(), false);
    app.loaded(data.clone()).unwrap();
    app.key(key(KeyCode::Char('u'))).unwrap();
    app.key(key(KeyCode::Char('y'))).unwrap();
    assert_eq!(
        app.review.as_ref().unwrap().drafts,
        vec![added_later.clone()]
    );
    assert_eq!(
        store::load(dir.path(), &data.pr).unwrap(),
        vec![added_later]
    );
    assert!(store::attempt(dir.path(), &data.pr).unwrap().is_none());
}

#[test]
fn repository_names_cannot_collide_in_local_state_paths() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = app::demo_data().pr;
    first.id.repo = "a--b/c".into();
    let mut second = first.clone();
    second.id.repo = "a/b--c".into();
    assert_ne!(
        store::draft_path(dir.path(), &first),
        store::draft_path(dir.path(), &second)
    );
    assert_ne!(
        store::attempt_path(dir.path(), &first),
        store::attempt_path(dir.path(), &second)
    );
}

#[cfg(unix)]
#[test]
fn github_preflight_fixture_child() {
    let Ok(scenario) = std::env::var("PRPR_GH_SCENARIO") else {
        return;
    };
    let id = prpr::model::PrId {
        repo: "owner/repo".into(),
        number: 42,
    };
    match scenario.as_str() {
        "changed_during_load" => {
            let error = prpr::github::load_review(&id).unwrap_err();
            assert!(error.to_string().contains("changed while loading"));
        }
        "changed_before_submit" => {
            let data = prpr::github::load_review(&id).unwrap();
            let error = prpr::github::submit_review(
                &data,
                &[],
                "Review summary",
                prpr::model::ReviewEvent::Comment,
            )
            .unwrap_err();
            assert!(error.to_string().contains("head changed since loading"));
        }
        "post_success" | "post_uncertain" => {
            use prpr::model::{DraftComment, ReviewEvent, Side};
            let data = prpr::github::load_review(&id).unwrap();
            let drafts = vec![
                DraftComment {
                    start_line: None,
                    start_side: None,
                    path: "file.rs".into(),
                    line: 1,
                    side: Side::Left,
                    body: "Why remove this invariant?".into(),
                },
                DraftComment {
                    start_line: None,
                    start_side: None,
                    path: "file.rs".into(),
                    line: 1,
                    side: Side::Right,
                    body: "Please add a regression test.".into(),
                },
            ];
            let result = prpr::github::submit_review(
                &data,
                &drafts,
                "Please address these two points.",
                ReviewEvent::RequestChanges,
            );
            if scenario == "post_success" {
                assert_eq!(
                    result.unwrap(),
                    "https://github.com/owner/repo/pull/42#pullrequestreview-123"
                );
            } else {
                assert!(prpr::github::is_submission_uncertain(&result.unwrap_err()));
            }
        }
        other => panic!("Unknown fixture scenario: {other}"),
    }
}

#[cfg(unix)]
#[test]
fn github_preflight_and_submission_transport_are_verified_without_network() {
    use std::{os::unix::fs::PermissionsExt, process::Command};

    for scenario in [
        "changed_during_load",
        "changed_before_submit",
        "post_success",
        "post_uncertain",
    ] {
        let fixture = tempfile::tempdir().unwrap();
        let executable = fixture.path().join("gh");
        std::fs::write(&executable, GH_FIXTURE).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![fixture.path().to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "github_preflight_fixture_child", "--nocapture"])
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("PRPR_GH_FIXTURE", fixture.path())
            .env("PRPR_GH_SCENARIO", scenario)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{scenario}: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let log = std::fs::read_to_string(fixture.path().join("calls.jsonl")).unwrap();
        let calls: Vec<Vec<String>> = log
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let posts = calls
            .iter()
            .filter(|args| args.iter().any(|arg| arg == "POST"))
            .count();
        if scenario.starts_with("changed_") {
            assert_eq!(posts, 0);
        } else {
            assert_eq!(posts, 1);
            let payload: serde_json::Value = serde_json::from_slice(
                &std::fs::read(fixture.path().join("payload.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                payload,
                serde_json::json!({
                    "commit_id": "a".repeat(40),
                    "body": "Please address these two points.",
                    "event": "REQUEST_CHANGES",
                    "comments": [
                        {"path": "file.rs", "line": 1, "side": "LEFT", "body": "Why remove this invariant?"},
                        {"path": "file.rs", "line": 1, "side": "RIGHT", "body": "Please add a regression test."}
                    ]
                })
            );
        }
        assert!(
            calls
                .iter()
                .any(|args| args.iter().any(|arg| arg == "--paginate"))
        );
    }
}

#[cfg(unix)]
const GH_FIXTURE: &str = r#"#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

root = Path(os.environ["PRPR_GH_FIXTURE"])
scenario = os.environ["PRPR_GH_SCENARIO"]
args = sys.argv[1:]
with (root / "calls.jsonl").open("a") as log:
    log.write(json.dumps(args) + "\n")
endpoint = next((arg for arg in args if arg.startswith("repos/")), "")
if endpoint == "repos/owner/repo/pulls/42":
    counter = root / "fetch_count"
    count = int(counter.read_text()) + 1 if counter.exists() else 1
    counter.write_text(str(count))
    threshold = {"changed_during_load": 2, "changed_before_submit": 3}.get(scenario, 1000)
    response = {
        "title": "Fixture", "body": "Fixture description",
        "html_url": "https://github.com/owner/repo/pull/42",
        "user": {"login": "author"},
        "head": {"sha": ("d" if count >= threshold else "a") * 40},
        "base": {"sha": "b" * 40},
        "draft": False, "changed_files": 1, "state": "open"
    }
elif "/compare/" in endpoint:
    response = {"merge_base_commit": {"sha": "c" * 40}}
elif "/files?" in endpoint:
    response = [[{
        "filename": "file.rs", "previous_filename": None, "status": "modified",
        "additions": 1, "deletions": 1,
        "patch": "@@ -1 +1 @@\n-old\n+new"
    }]]
elif "/comments?" in endpoint:
    response = [[]]
elif endpoint == "repos/owner/repo/pulls/42/reviews" and "POST" in args:
    payload = json.load(sys.stdin)
    (root / "payload.json").write_text(json.dumps(payload))
    if scenario == "post_uncertain":
        sys.stderr.write("Connection closed after request transmission")
        sys.exit(1)
    response = {"html_url": "https://github.com/owner/repo/pull/42#pullrequestreview-123"}
else:
    sys.stderr.write("Unexpected fixture request: " + repr(args))
    sys.exit(1)
print(json.dumps(response))
"#;

#[test]
fn checkout_targets_the_launch_repository_when_it_holds_the_pr() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().into(), false);
    app.loaded(app::demo_data()).unwrap();
    let repo = app.review.as_ref().unwrap().data.pr.id.repo.clone();
    app.checkout = Some((dir.path().join("launch"), repo));
    match app.key(key(KeyCode::Char('w'))).unwrap() {
        Action::CheckoutHere { pr, repository } => {
            assert_eq!(repository, dir.path().join("launch"));
            assert_eq!(pr.id, app.review.as_ref().unwrap().data.pr.id);
        }
        _ => panic!("w should check out in the launch repository"),
    }
    app.checkout = Some((dir.path().join("launch"), "someone/else".into()));
    assert!(matches!(
        app.key(key(KeyCode::Char('w'))).unwrap(),
        Action::Prepare {
            open_editor: None,
            ..
        }
    ));
}

#[test]
fn thread_reply_targets_the_root_keeps_its_draft_and_posts_after_preview() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().into(), false);
    app.loaded(app::demo_data()).unwrap();
    let id = app.review.as_ref().unwrap().data.pr.id.clone();
    let type_text = |app: &mut App, text: &str| {
        for character in text.chars() {
            app.key(key(KeyCode::Char(character))).unwrap();
        }
    };
    let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);

    app.navigate("tests/deadline.rs", &prpr::model::Side::Right, 1)
        .unwrap();
    type_text(&mut app, "gr");
    assert!(
        app.dialog.is_none(),
        "a line without a thread has nothing to reply to"
    );
    assert!(app.status.contains("No review thread"), "{}", app.status);

    app.navigate("src/client.rs", &prpr::model::Side::Right, 25)
        .unwrap();
    type_text(&mut app, "gr");
    assert!(
        matches!(&app.dialog, Some(Dialog::Reply { comment: 1, .. })),
        "the reply goes to the thread's first comment"
    );
    type_text(&mut app, "Done in");
    app.key(key(KeyCode::Esc)).unwrap();
    assert!(app.dialog.is_none());
    assert_eq!(store::reply_draft(dir.path(), &id, 1).unwrap(), "Done in");

    type_text(&mut app, "gr");
    type_text(&mut app, " abc123");
    app.key(ctrl_s).unwrap();
    assert!(matches!(app.dialog, Some(Dialog::ReplyPreview { .. })));
    app.key(key(KeyCode::Esc)).unwrap();
    assert!(
        matches!(&app.dialog, Some(Dialog::Reply { input, .. }) if input.text == "Done in abc123")
    );
    app.key(ctrl_s).unwrap();
    match app.key(key(KeyCode::Enter)).unwrap() {
        Action::PostReply {
            id: posted,
            comment,
            body,
        } => {
            assert_eq!(posted, id);
            assert_eq!(comment, 1);
            assert_eq!(body, "Done in abc123");
        }
        _ => panic!("Enter in the preview must post the reply"),
    }
}

#[test]
fn conversation_comment_keeps_its_draft_across_escape_and_posts_after_preview() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().into(), false);
    app.loaded(app::demo_data()).unwrap();
    let id = app.review.as_ref().unwrap().data.pr.id.clone();
    let type_text = |app: &mut App, text: &str| {
        for character in text.chars() {
            app.key(key(KeyCode::Char(character))).unwrap();
        }
    };
    let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);

    app.key(key(KeyCode::Char('M'))).unwrap();
    type_text(&mut app, "Looks good");
    app.key(key(KeyCode::Esc)).unwrap();
    assert!(app.dialog.is_none());
    assert_eq!(
        store::conversation_draft(dir.path(), &id).unwrap(),
        "Looks good"
    );

    app.key(key(KeyCode::Char('n'))).unwrap();
    app.key(key(KeyCode::Char('M'))).unwrap();
    type_text(&mut app, " overall");
    app.key(ctrl_s).unwrap();
    assert!(matches!(
        app.dialog,
        Some(Dialog::ConversationPreview { .. })
    ));
    app.key(key(KeyCode::Esc)).unwrap();
    assert!(
        matches!(&app.dialog, Some(Dialog::Conversation { input, .. }) if input.text == "Looks good overall")
    );
    app.key(ctrl_s).unwrap();
    match app.key(key(KeyCode::Enter)).unwrap() {
        Action::PostComment { id: posted, body } => {
            assert_eq!(posted, id);
            assert_eq!(body, "Looks good overall");
        }
        _ => panic!("Enter in the preview must post the comment"),
    }

    app.key(key(KeyCode::Char('M'))).unwrap();
    app.key(key(KeyCode::Esc)).unwrap();
    store::save_conversation_draft(dir.path(), &id, "  ").unwrap();
    assert_eq!(store::conversation_draft(dir.path(), &id).unwrap(), "");
}
