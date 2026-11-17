#[allow(dead_code)]
#[path = "../src/app.rs"]
mod app;
#[allow(dead_code)]
#[path = "../src/store.rs"]
mod store;

use app::{Action, App, Dialog, Focus, Selection};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use prpr::{
    diff::validate_comment,
    model::{ChangedFile, DraftComment, Side},
};

fn key(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
}

fn fixture_file(path: &str, patch: Option<&str>) -> ChangedFile {
    ChangedFile {
        filename: path.into(),
        previous_filename: None,
        status: "modified".into(),
        additions: 1,
        deletions: 1,
        patch: patch.map(str::to_owned),
    }
}

fn load(app: &mut App, files: Vec<ChangedFile>) {
    let mut data = app::demo_data();
    data.files = files;
    data.comments.clear();
    app.loaded(data).unwrap();
}

fn range(app: &mut App, anchor: usize, cursor: usize) {
    let review = app.review.as_mut().unwrap();
    review.positions[review.selected_file] = cursor;
    review.selection = Some(Selection {
        anchor,
        extending: true,
    });
}

#[test]
fn line_navigation_stays_in_the_file_and_period_comma_switch_files() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![
            fixture_file("a.rs", Some("@@ -1 +1 @@\n-old\n+new")),
            fixture_file("b.bin", None),
            fixture_file("c.rs", Some("@@ -8 +8 @@\n tail")),
        ],
    );
    app.key(key('k')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 0);
    assert_eq!(app.review.as_ref().unwrap().position(), 0);
    for _ in 0..5 {
        app.key(key('j')).unwrap();
    }
    assert_eq!(app.review.as_ref().unwrap().selected_file, 0);
    assert_eq!(app.review.as_ref().unwrap().position(), 2);
    app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 0);
    app.key(key('.')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 1);
    app.key(key('.')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 2);
    for _ in 0..3 {
        app.key(key('j')).unwrap();
    }
    assert_eq!(
        app.review.as_ref().unwrap().anchor(),
        Some((Side::Right, 8))
    );
    for _ in 0..3 {
        app.key(key('k')).unwrap();
    }
    assert_eq!(app.review.as_ref().unwrap().selected_file, 2);
    app.key(key(',')).unwrap();
    app.key(key(',')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 0);
    assert_eq!(app.review.as_ref().unwrap().position(), 2);
}

#[test]
fn repeated_line_selection_extends_down_but_normal_movement_replaces_it() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![
            fixture_file("a.rs", Some("@@ -1,3 +1,3 @@\n one\n two\n three")),
            fixture_file("b.rs", Some("@@ -1 +1 @@\n next")),
        ],
    );
    app.key(key('j')).unwrap();
    for expected_end in 1..=3 {
        app.key(key('x')).unwrap();
        assert_eq!(
            app.review.as_ref().unwrap().selected_range(),
            Some((1, expected_end))
        );
    }
    app.key(key('x')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_range(), Some((1, 3)));
    app.key(key('X')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_range(), Some((1, 3)));
    app.key(key('j')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 0);
    assert!(app.review.as_ref().unwrap().selected_range().is_none());
}

#[test]
fn x_grows_a_reversed_completed_selection_from_its_bottom_edge() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![fixture_file(
            "a.rs",
            Some("@@ -1,4 +1,4 @@\n one\n two\n three\n four"),
        )],
    );
    app.review.as_mut().unwrap().positions[0] = 3;
    for command in ['v', 'k', 'k', 'v'] {
        app.key(key(command)).unwrap();
    }
    assert_eq!(app.review.as_ref().unwrap().selected_range(), Some((1, 3)));
    assert!(!app.review.as_ref().unwrap().selection.unwrap().extending);
    app.key(key('x')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_range(), Some((1, 4)));
    app.key(key('x')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_range(), Some((1, 4)));
}

#[test]
fn saving_a_selected_comment_resumes_normal_navigation_within_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![
            fixture_file("a.rs", Some("@@ -1,3 +1,3 @@\n one\n two\n three")),
            fixture_file("b.rs", Some("@@ -8 +8 @@\n next")),
        ],
    );
    for command in ['v', 'j', 'j', 'c'] {
        app.key(key(command)).unwrap();
    }
    for character in "Please verify this invariant.".chars() {
        app.key(key(character)).unwrap();
    }
    app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        .unwrap();
    let review = app.review.as_ref().unwrap();
    assert!(review.selection.is_none());
    assert_eq!(review.drafts.len(), 1);
    assert_eq!(review.drafts[0].start_line, Some(1));
    assert_eq!(review.drafts[0].line, 3);
    assert_eq!(
        store::load(directory.path(), &review.data.pr).unwrap(),
        review.drafts
    );
    app.key(key('k')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 0);
    assert_eq!(app.review.as_ref().unwrap().position(), 2);
    assert!(app.review.as_ref().unwrap().selection.is_none());
}

#[test]
fn selection_started_on_a_header_uses_source_and_missing_patches_cannot_be_selected() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    for command in ['v', 'x', 'X'] {
        load(
            &mut app,
            vec![fixture_file("a.rs", Some("@@ -1 +1 @@\n line"))],
        );
        app.key(key(command)).unwrap();
        assert_eq!(app.review.as_ref().unwrap().selected_range(), Some((1, 1)));
        assert_eq!(
            app.review.as_ref().unwrap().anchor(),
            Some((Side::Right, 1))
        );
    }
    load(&mut app, vec![fixture_file("a.bin", None)]);
    assert!(app.key(key('v')).is_err());
    assert!(app.review.as_ref().unwrap().selection.is_none());
}

#[test]
fn extend_mode_never_crosses_files_and_escape_collapses_without_leaving_review() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![
            fixture_file("a.rs", Some("@@ -1,2 +1,2 @@\n one\n two")),
            fixture_file("b.rs", Some("@@ -1 +1 @@\n next")),
        ],
    );
    app.key(key('j')).unwrap();
    app.key(key('v')).unwrap();
    for _ in 0..6 {
        app.key(key('j')).unwrap();
    }
    assert_eq!(app.review.as_ref().unwrap().selected_file, 0);
    assert_eq!(app.review.as_ref().unwrap().selected_range(), Some((1, 2)));
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.review.is_some());
    assert!(app.review.as_ref().unwrap().selected_range().is_none());
    app.key(key('j')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 0);
}

#[test]
fn reversed_context_and_deletion_range_uses_old_side_coordinates() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![fixture_file(
            "a.rs",
            Some("@@ -10,5 +20,4 @@\n before\n-old one\n-old two\n+new one\n after\n end"),
        )],
    );
    range(&mut app, 3, 1);
    assert_eq!(app.review.as_ref().unwrap().selected_range(), Some((1, 3)));
    app.key(key('c')).unwrap();
    match &app.dialog {
        Some(Dialog::Compose { draft, .. }) => {
            assert_eq!(draft.side, Side::Left);
            assert_eq!(draft.line, 12);
            assert_eq!(draft.start_line, Some(10));
            assert_eq!(draft.start_side, Some(Side::Left));
            assert!(validate_comment(
                app.review.as_ref().unwrap().current_file().unwrap(),
                draft
            ));
        }
        _ => panic!("A reversed same-side range must create a multiline comment"),
    }
}

#[test]
fn addition_and_context_range_uses_new_side_coordinates() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![fixture_file(
            "a.rs",
            Some("@@ -10,5 +20,4 @@\n before\n-old one\n-old two\n+new one\n after\n end"),
        )],
    );
    range(&mut app, 6, 4);
    app.key(key('c')).unwrap();
    match &app.dialog {
        Some(Dialog::Compose { draft, .. }) => {
            assert_eq!(draft.side, Side::Right);
            assert_eq!(draft.line, 23);
            assert_eq!(draft.start_line, Some(21));
            assert_eq!(draft.start_side, Some(Side::Right));
            assert!(validate_comment(
                app.review.as_ref().unwrap().current_file().unwrap(),
                draft
            ));
        }
        _ => panic!("Addition and context lines must compose against the new side"),
    }
}

#[test]
fn context_only_reversed_selection_uses_the_new_coordinates() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![fixture_file(
            "a.rs",
            Some("@@ -10,5 +20,4 @@\n before\n-old one\n-old two\n+new one\n after\n end"),
        )],
    );
    range(&mut app, 6, 5);
    app.key(key('c')).unwrap();
    match &app.dialog {
        Some(Dialog::Compose { draft, .. }) => {
            assert_eq!(draft.side, Side::Right);
            assert_eq!(draft.line, 23);
            assert_eq!(draft.start_line, Some(22));
            assert_eq!(draft.start_side, Some(Side::Right));
            assert!(validate_comment(
                app.review.as_ref().unwrap().current_file().unwrap(),
                draft
            ));
        }
        _ => panic!("Context-only selections must map to the new source coordinates"),
    }
}

#[test]
fn deletions_inside_context_endpoints_still_choose_the_old_side() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![fixture_file(
            "a.rs",
            Some("@@ -10,4 +20,2 @@\n before\n-old one\n-old two\n after"),
        )],
    );
    range(&mut app, 1, 4);
    app.key(key('c')).unwrap();
    match &app.dialog {
        Some(Dialog::Compose { draft, .. }) => {
            assert_eq!(draft.side, Side::Left);
            assert_eq!(draft.line, 13);
            assert_eq!(draft.start_line, Some(10));
            assert_eq!(draft.start_side, Some(Side::Left));
        }
        _ => panic!("Interior deleted lines must participate in side inference"),
    }
}

#[test]
fn mixed_sides_hunk_headers_and_multiple_hunks_cannot_be_submitted_as_one_range() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    for (patch, anchor, cursor) in [
        ("@@ -1 +1 @@\n-old\n+new", 1, 2),
        ("@@ -1,3 +1,3 @@\n before\n-old\n+new\n after", 1, 4),
        ("@@ -1 +1 @@\n same", 0, 1),
        ("@@ -1 +1 @@\n one\n@@ -9 +9 @@\n nine", 1, 3),
    ] {
        load(&mut app, vec![fixture_file("a.rs", Some(patch))]);
        range(&mut app, anchor, cursor);
        let result = app.key(key('c'));
        assert!(result.is_err() || app.error);
        assert!(!matches!(app.dialog, Some(Dialog::Compose { .. })));
        assert!(app.review.as_ref().unwrap().drafts.is_empty());
    }
}

#[test]
fn goto_prefix_cancellation_consumes_the_key_without_mutating_review() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![fixture_file("a.rs", Some("@@ -1,2 +1,2 @@\n one\n two"))],
    );
    app.key(key('j')).unwrap();
    for continuation in [
        key('j'),
        key('q'),
        key('c'),
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    ] {
        app.key(key('g')).unwrap();
        assert!(app.goto_pending);
        assert!(matches!(app.key(continuation).unwrap(), Action::None));
        assert!(!app.goto_pending);
        assert_eq!(app.review.as_ref().unwrap().position(), 1);
        assert!(app.dialog.is_none());
    }
}

#[test]
fn goto_bounds_extend_only_within_current_file_and_busy_gf_never_opens_editor() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![fixture_file(
            "a.rs",
            Some("@@ -1,3 +1,3 @@\n one\n two\n three"),
        )],
    );
    app.key(key('j')).unwrap();
    app.key(key('v')).unwrap();
    app.key(key('g')).unwrap();
    app.key(key('e')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_range(), Some((1, 3)));
    app.key(key('g')).unwrap();
    app.key(key('g')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_range(), Some((0, 1)));
    app.key(key(';')).unwrap();
    assert!(app.review.as_ref().unwrap().selection.is_none());
    app.busy = true;
    app.key(key('g')).unwrap();
    assert!(matches!(app.key(key('f')).unwrap(), Action::None));
    assert!(!app.goto_pending);
    assert!(app.dialog.is_none());
}

#[test]
fn gf_captures_the_active_deleted_source_line() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![fixture_file("a.rs", Some("@@ -10,2 +20 @@\n before\n-old"))],
    );
    app.review.as_mut().unwrap().positions[0] = 2;
    app.key(key('g')).unwrap();
    match app.key(key('f')).unwrap() {
        Action::OpenEditor(target) => {
            assert_eq!(target.file.filename, "a.rs");
            assert_eq!(target.side, Side::Left);
            assert_eq!(target.line, 11);
        }
        _ => panic!("gf must open an editor target for the active source line"),
    }
    assert!(!app.goto_pending);
}

#[test]
fn published_reviews_move_to_uppercase_v_without_changing_selection() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![fixture_file("a.rs", Some("@@ -1,2 +1,2 @@\n one\n two"))],
    );
    assert!(matches!(app.key(key('v')).unwrap(), Action::None));
    app.key(key('j')).unwrap();
    let selection = app.review.as_ref().unwrap().selected_range();
    assert!(
        matches!(app.key(key('V')).unwrap(), Action::Activity(id) if id.repo == "example/service" && id.number == 42)
    );
    assert_eq!(app.review.as_ref().unwrap().selected_range(), selection);
    app.busy = true;
    assert!(matches!(app.key(key('V')).unwrap(), Action::None));
}

#[test]
fn sidebar_boundary_jumps_cannot_carry_a_selection_into_another_file() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    let files = vec![
        fixture_file("a.rs", Some("@@ -1,2 +1,2 @@\n one\n two")),
        fixture_file("b.rs", Some("@@ -10,2 +10,2 @@\n ten\n eleven")),
    ];
    for boundary in [KeyCode::Home, KeyCode::Char('G')] {
        load(&mut app, files.clone());
        app.review.as_mut().unwrap().selected_file = if boundary == KeyCode::Home { 1 } else { 0 };
        range(&mut app, 1, 2);
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();
        app.key(KeyEvent::new(boundary, KeyModifiers::NONE))
            .unwrap();
        assert!(app.review.as_ref().unwrap().selection.is_none());
        assert_eq!(
            app.review.as_ref().unwrap().selected_file,
            if boundary == KeyCode::Home { 0 } else { 1 }
        );
    }
}

#[test]
fn legacy_single_line_draft_json_remains_compatible_and_omits_range_fields() {
    let draft: DraftComment = serde_json::from_value(serde_json::json!({
        "path": "a.rs", "line": 7, "side": "RIGHT", "body": "Legacy draft"
    }))
    .unwrap();
    assert!(draft.start_line.is_none());
    assert!(draft.start_side.is_none());
    let encoded = serde_json::to_value(&draft).unwrap();
    assert!(encoded.get("start_line").is_none());
    assert!(encoded.get("start_side").is_none());
}

#[test]
fn returning_from_the_sidebar_starts_other_files_at_the_top_and_keeps_the_same_file() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![
            fixture_file("a.rs", Some("@@ -1,3 +1,3 @@\n one\n two\n three")),
            fixture_file("b.rs", Some("@@ -1,2 +1,2 @@\n four\n five")),
        ],
    );
    let review = app.review.as_mut().unwrap();
    review.positions = vec![2, 1];
    app.key(key('h')).unwrap();
    app.key(key('l')).unwrap();
    assert_eq!(app.review.as_ref().unwrap().position(), 2);
    app.key(key('h')).unwrap();
    app.key(key('j')).unwrap();
    app.key(key('l')).unwrap();
    let review = app.review.as_ref().unwrap();
    assert_eq!((review.selected_file, review.position()), (1, 0));
    app.review.as_mut().unwrap().positions[0] = 2;
    app.key(key('h')).unwrap();
    app.key(key('k')).unwrap();
    app.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    let review = app.review.as_ref().unwrap();
    assert_eq!(review.focus, Focus::Diff);
    assert_eq!((review.selected_file, review.position()), (0, 0));
    assert_eq!(review.horizontal, 0);
}

#[test]
fn copy_formats_markdown_and_permalinks_pin_the_reviewed_revision() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![fixture_file(
            "src/a b.rs",
            Some("@@ -10,3 +20,3 @@\n keep\n-old\n+new\n tail"),
        )],
    );
    app.review.as_mut().unwrap().positions[0] = 1;
    range(&mut app, 1, 3);
    let review = app.review.as_ref().unwrap();
    assert_eq!(
        review.copy_text().unwrap(),
        "`src/a b.rs:20-21`\n\n```diff\n keep\n-old\n+new\n```\n"
    );
    let head = &review.data.pr.head_sha;
    assert_eq!(
        review.permalink().unwrap(),
        format!("https://github.com/example/service/blob/{head}/src/a%20b.rs#L20-L21")
    );
    range(&mut app, 2, 2);
    let review = app.review.as_ref().unwrap();
    let base = &review.data.pr.base_sha;
    assert_eq!(
        review.permalink().unwrap(),
        format!("https://github.com/example/service/blob/{base}/src/a%20b.rs#L11")
    );
    assert!(matches!(
        app.key(key('y')).unwrap(),
        Action::Copy {
            label: "selected lines",
            ..
        }
    ));
    app.key(key('g')).unwrap();
    assert!(matches!(
        app.key(key('y')).unwrap(),
        Action::Copy {
            label: "GitHub permalink",
            ..
        }
    ));
}

#[test]
fn pipe_toggles_the_layout_shown_on_screen() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    load(
        &mut app,
        vec![fixture_file("a.rs", Some("@@ -1 +1 @@\n-old\n+new"))],
    );
    assert_eq!(app.diff_split, None);
    app.key(key('|')).unwrap();
    assert_eq!(app.diff_split, Some(true));
    app.split_shown.set(true);
    app.key(key('|')).unwrap();
    assert_eq!(app.diff_split, Some(false));
}
