#[allow(dead_code)]
#[path = "../src/app.rs"]
mod app;
#[allow(dead_code)]
#[path = "../src/store.rs"]
mod store;
#[allow(dead_code)]
#[path = "../src/ui.rs"]
mod ui;

use app::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use prpr::model::{DraftComment, ReviewComment, Side};
use ratatui::{Terminal, backend::TestBackend};

fn key(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
}

fn screen(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| ui::draw(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn comment(id: u64, line: Option<u32>, side: &str, body: &str) -> ReviewComment {
    ReviewComment {
        start_line: None,
        start_side: None,
        id,
        path: "src/client.rs".into(),
        body: body.into(),
        author: format!("reviewer-{id}"),
        line,
        original_line: Some(999),
        side: side.into(),
        in_reply_to_id: None,
    }
}

#[test]
fn inline_published_reply_and_draft_visibility_preserves_left_source_target() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    let mut data = app::demo_data();
    data.files.truncate(1);
    data.files[0].patch = Some("@@ -1,2 +1,2 @@\n-old\n+new\n context".into());
    let mut reply = comment(11, None, "LEFT", "REPLY-BODY");
    reply.in_reply_to_id = Some(10);
    data.comments = vec![
        reply,
        comment(10, Some(1), "LEFT", "PUBLISHED-BODY"),
        comment(12, Some(2), "LEFT", "LEFT-CONTEXT-BODY"),
    ];
    app.loaded(data).unwrap();
    let review = app.review.as_mut().unwrap();
    review.positions[0] = 1;
    review.drafts.push(DraftComment {
        start_line: None,
        start_side: None,
        path: "src/client.rs".into(),
        line: 1,
        side: Side::Right,
        body: "LOCAL-DRAFT-BODY".into(),
    });
    let anchor = review.anchor();
    let before = screen(&mut app, 120, 60);
    for text in [
        "PUBLISHED-BODY",
        "REPLY-BODY",
        "LEFT-CONTEXT-BODY",
        "LOCAL-DRAFT-BODY",
    ] {
        assert!(before.contains(text), "Missing visible inline text: {text}");
    }
    app.key(key('t')).unwrap();
    let hidden = screen(&mut app, 120, 60);
    for text in [
        "PUBLISHED-BODY",
        "REPLY-BODY",
        "LEFT-CONTEXT-BODY",
        "LOCAL-DRAFT-BODY",
    ] {
        assert!(
            !hidden.contains(text),
            "Hidden inline text still rendered: {text}"
        );
    }
    assert_eq!(app.review.as_ref().unwrap().anchor(), anchor);
    assert_eq!(anchor, Some((Side::Left, 1)));
    app.key(key('t')).unwrap();
    assert_eq!(screen(&mut app, 120, 60), before);
}

#[test]
fn long_published_comment_tail_is_reachable_in_full_thread_view() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    let mut data = app::demo_data();
    let body = (0..30)
        .map(|index| format!("Published comment line {index}"))
        .chain(std::iter::once("TAIL-OF-PUBLISHED-COMMENT".into()))
        .collect::<Vec<_>>()
        .join("\n");
    data.comments = vec![comment(1, Some(18), "RIGHT", &body)];
    app.loaded(data).unwrap();
    app.key(key('j')).unwrap();
    let inline = screen(&mut app, 100, 32);
    assert!(inline.contains("T opens the full thread"));
    assert!(!inline.contains("TAIL-OF-PUBLISHED-COMMENT"));
    app.key(key('T')).unwrap();
    let mut tail_visible = false;
    for _ in 0..40 {
        if screen(&mut app, 100, 32).contains("TAIL-OF-PUBLISHED-COMMENT") {
            tail_visible = true;
            break;
        }
        app.key(key('j')).unwrap();
    }
    assert!(
        tail_visible,
        "Full-thread scrolling must expose the complete published body"
    );
}

#[test]
fn outdated_comment_on_a_missing_patch_stays_unanchored_and_can_be_hidden() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    let mut data = app::demo_data();
    data.files[0].patch = None;
    data.comments = vec![comment(1, None, "LEFT", "OUTDATED-UNANCHORED-BODY")];
    app.loaded(data).unwrap();
    assert!(app.review.as_ref().unwrap().anchor().is_none());
    let visible = screen(&mut app, 110, 32);
    assert!(visible.contains("No text diff available"));
    assert!(visible.contains("OUTDATED-UNANCHORED-BODY"));
    assert!(visible.contains("outdated"));
    app.key(key('t')).unwrap();
    assert!(!screen(&mut app, 110, 32).contains("OUTDATED-UNANCHORED-BODY"));
    assert!(app.review.as_ref().unwrap().anchor().is_none());
}

#[test]
fn unsorted_api_files_form_contiguous_groups_with_matching_source_navigation() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    let mut data = app::demo_data();
    let mut later_source = data.files[0].clone();
    later_source.filename = "src/retry.rs".into();
    data.files.push(later_source);
    app.loaded(data).unwrap();
    let review = app.review.as_ref().unwrap();
    assert_eq!(
        review
            .data
            .files
            .iter()
            .map(|file| file.filename.as_str())
            .collect::<Vec<_>>(),
        vec!["src/client.rs", "src/retry.rs", "tests/deadline.rs"]
    );
    let rendered = screen(&mut app, 120, 36);
    let retry = rendered.find("retry.rs").unwrap();
    let tests_heading = rendered.find("▾ tests").unwrap();
    assert!(retry < tests_heading);

    app.key(key('n')).unwrap();
    app.key(key('j')).unwrap();
    let review = app.review.as_ref().unwrap();
    assert_eq!(review.current_file().unwrap().filename, "src/retry.rs");
    assert_eq!(review.anchor(), Some((Side::Right, 18)));
    app.key(key('n')).unwrap();
    app.key(key('j')).unwrap();
    let review = app.review.as_ref().unwrap();
    assert_eq!(review.current_file().unwrap().filename, "tests/deadline.rs");
    assert_eq!(review.anchor(), Some((Side::Right, 1)));
    app.key(key('p')).unwrap();
    assert_eq!(
        app.review.as_ref().unwrap().anchor(),
        Some((Side::Right, 18))
    );
}
