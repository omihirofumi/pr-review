#[allow(dead_code)]
#[path = "../src/app.rs"]
mod app;
#[allow(dead_code)]
#[path = "../src/store.rs"]
mod store;
#[allow(dead_code)]
#[path = "../src/ui.rs"]
mod ui;

use app::{Action, App, Dialog, OracleStatus, PageTab};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use prpr::{
    model::{ChangedFile, PrId, PrOverview, PullRequest, ReviewData, ReviewSource},
    oracle::{TestProof, Verdict},
    settings::Provider,
    sieve::{self, RowClass},
};
use ratatui::{Terminal, backend::TestBackend};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
}

fn press(app: &mut App, character: char) -> anyhow::Result<Action> {
    app.key(key(KeyCode::Char(character)))
}

fn overview_for(data: &ReviewData) -> PrOverview {
    PrOverview {
        id: data.pr.id.clone(),
        title: data.pr.title.clone(),
        body: data.pr.body.clone(),
        url: data.pr.url.clone(),
        author: data.pr.author.clone(),
        head_sha: data.pr.head_sha.clone(),
        base_tip_sha: data.pr.base_sha.clone(),
        draft: data.pr.draft,
        state: "open".into(),
        changed_files: data.files.len() as u64,
        additions: data.files.iter().map(|file| file.additions).sum(),
        deletions: data.files.iter().map(|file| file.deletions).sum(),
    }
}

fn screen_sized(app: &mut App, width: u16, height: u16) -> String {
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

fn screen(app: &mut App) -> String {
    screen_sized(app, 130, 50)
}

#[test]
fn demo_oracle_enforces_blind_prediction_before_allowing_verdict() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), true);
    let data = app::demo_data();
    app.opened(overview_for(&data));
    app.accept_diff(data).unwrap();

    // Before opening Oracle, the tab bar looks identical to baseline.
    let before_open = screen(&mut app);
    assert!(!before_open.contains("Oracle 0/"));

    // Press O on Overview to open Behavioral Oracle.
    let action = press(&mut app, 'O').unwrap();
    assert!(matches!(action, Action::None));
    assert!(matches!(app.dialog, Some(Dialog::Oracle)));
    assert_eq!(app.oracle.witnesses().len(), 3);
    assert_eq!(app.oracle.witnesses()[0].proof, TestProof::Untested);

    // Blind prediction masks After and Mechanism so reviewers cannot anchor on LGTM code.
    let masked = screen(&mut app);
    assert!(masked.contains("BLIND PREDICTION"));
    assert!(masked.contains("self.retry_delay = 0ms, budget = 5s"));
    assert!(!masked.contains("without calling self.send()"));

    // Pressing 'y' while masked reveals After first instead of rubber-stamping!
    press(&mut app, 'y').unwrap();
    assert!(
        app.oracle.verdicts.is_empty(),
        "blind prediction blocks rubber-stamping before After is revealed"
    );
    let revealed = screen(&mut app);
    assert!(!revealed.contains("BLIND PREDICTION"));
    assert!(revealed.contains("without calling self.send()"));
    assert!(revealed.contains("self.retry_delay.min(budget)"));

    // Move to second case: also masked; pressing 'x' while masked reveals first instead of flagging.
    press(&mut app, 'j').unwrap();
    assert!(screen(&mut app).contains("BLIND PREDICTION"));
    press(&mut app, 'x').unwrap();
    assert!(app.oracle.verdicts.is_empty());
    assert!(matches!(app.dialog, Some(Dialog::Oracle)));
    assert!(!screen(&mut app).contains("BLIND PREDICTION"));

    // Move to third case and toggle blind mode off with 'b', then back on.
    press(&mut app, 'j').unwrap();
    assert!(screen(&mut app).contains("BLIND PREDICTION"));
    press(&mut app, 'b').unwrap();
    assert!(!app.oracle.blind);
    assert!(!screen(&mut app).contains("BLIND PREDICTION"));
    press(&mut app, 'b').unwrap();
    assert!(app.oracle.blind);
    assert!(screen(&mut app).contains("BLIND PREDICTION"));

    // Narrow layout (< 96 columns) also renders cleanly without panicking.
    let narrow = screen_sized(&mut app, 72, 32);
    assert!(narrow.contains("Behavioral Oracle"));
}

#[test]
fn flagging_counterexample_jumps_to_culprit_marks_lines_and_prefills_draft_or_note() {
    // 1. GitHub PR flow -> pre-fills Dialog::Compose and saves a GitHub review draft.
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), true);
    let data = app::demo_data();
    app.opened(overview_for(&data));
    app.accept_diff(data.clone()).unwrap();

    press(&mut app, 'O').unwrap();
    press(&mut app, ' ').unwrap(); // reveal case 0
    press(&mut app, 'x').unwrap(); // flag as unintended!

    assert_eq!(app.tab, PageTab::Diff);
    let review = app.review.as_ref().unwrap();
    assert_eq!(review.current_file().unwrap().filename, "src/client.rs");
    assert_eq!(review.marks.len(), 1);
    assert_eq!(review.marks[0].line, 19);
    assert_eq!(review.marks[0].end_line, 22);

    match &app.dialog {
        Some(Dialog::Compose { draft, input, .. }) => {
            assert_eq!(draft.path, "src/client.rs");
            assert_eq!(draft.start_line, Some(19));
            assert_eq!(draft.line, 22);
            assert!(
                input
                    .text
                    .contains("Counterexample (`self.retry_delay = 0ms, budget = 5s`):")
            );
            assert!(input.text.contains("Returns Err(Error::DeadlineExceeded)"));
        }
        _ => panic!("Expected pre-filled Dialog::Compose"),
    }
    app.key(ctrl('s')).unwrap();
    assert!(app.dialog.is_none());
    assert_eq!(app.review.as_ref().unwrap().drafts.len(), 1);
    assert!(
        app.review.as_ref().unwrap().drafts[0]
            .body
            .contains("Counterexample")
    );

    // 2. Local Jujutsu review flow -> pre-fills Dialog::Note and saves a local note.
    let local_dir = tempfile::tempdir().unwrap();
    let mut local_app = App::new(local_dir.path().into(), true);
    let mut local_data = app::demo_data();
    local_data.source = ReviewSource::Local {
        repository: local_dir.path().into(),
        label: "jj diff -r @".into(),
    };
    local_app.loaded(local_data).unwrap();
    press(&mut local_app, 'O').unwrap();
    press(&mut local_app, ' ').unwrap();
    press(&mut local_app, 'x').unwrap();
    assert!(matches!(local_app.dialog, Some(Dialog::Note { .. })));
    local_app.key(ctrl('s')).unwrap();
    assert!(local_app.dialog.is_none());
    assert_eq!(local_app.review.as_ref().unwrap().notes.len(), 1);
}

#[test]
fn marking_witness_intended_persists_across_restarts_and_shrinks_dark_diff_in_sieve() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), true);
    let data = app::demo_data();
    app.opened(overview_for(&data));
    app.accept_diff(data.clone()).unwrap();

    // Open Oracle, reveal & verify case 1 (lines 19..=22) and case 2 (lines 19..=23).
    press(&mut app, 'O').unwrap();
    press(&mut app, ' ').unwrap();
    press(&mut app, 'y').unwrap(); // marks case 0 Intended and advances to case 1
    assert_eq!(app.oracle.selected, 1);
    press(&mut app, ' ').unwrap();
    press(&mut app, 'y').unwrap(); // marks case 1 Intended (covers lines 19..=23!)
    assert_eq!(app.oracle.selected, 2);

    // Close Oracle and check tab badge + Sieve in Diff.
    app.key(key(KeyCode::Esc)).unwrap();
    assert!(app.dialog.is_none());
    let tabs_screen = screen(&mut app);
    assert!(tabs_screen.contains("Oracle · 2/3"));

    press(&mut app, '3').unwrap();
    assert_eq!(app.tab, PageTab::Diff);
    press(&mut app, 'z').unwrap();
    assert!(app.review.as_ref().unwrap().sieve);
    assert!(app.status.contains("Sieve ON: 0 unaccounted lines"));

    let sieved = screen(&mut app);
    assert!(sieved.contains("SIEVE ◆0 unaccounted"));
    assert!(sieved.contains("✓7"));

    // Undo a verdict inside Oracle ('u') restores unaccounted lines when needed.
    press(&mut app, 'O').unwrap();
    press(&mut app, 'k').unwrap(); // back to case 1 (lines 19..=23, including sleep(delay) on 23)
    press(&mut app, 'u').unwrap(); // clear verdict on case 1 -> line 23 + paired deletion become unaccounted again!
    app.key(key(KeyCode::Esc)).unwrap();
    let after_undo = screen(&mut app);
    assert!(!after_undo.contains("SIEVE ◆0 unaccounted"));

    // Re-verify case 1 and confirm verdicts survive a full App restart on the same revision,
    // while a new head_sha isolates its verdicts cleanly.
    press(&mut app, 'O').unwrap();
    press(&mut app, 'y').unwrap(); // case 1 was already revealed on undo, so 'y' records immediately
    app.key(key(KeyCode::Esc)).unwrap();

    let mut restarted = App::new(directory.path().into(), true);
    restarted.loaded(data.clone()).unwrap();
    assert_eq!(restarted.oracle.verdicts.len(), 2);
    assert!(
        restarted
            .oracle
            .verdicts
            .values()
            .all(|&v| v == Verdict::Intended)
    );

    let mut next_rev = data;
    next_rev.pr.head_sha = "f".repeat(40);
    let mut fresh_rev = App::new(directory.path().into(), true);
    fresh_rev.loaded(next_rev).unwrap();
    assert!(fresh_rev.oracle.verdicts.is_empty());
}

fn multi_file_sieve_fixture() -> ReviewData {
    ReviewData {
        source: ReviewSource::GitHub,
        pr: PullRequest {
            id: PrId {
                repo: "acme/gateway".into(),
                number: 99,
            },
            title: "Move retry policy and tweak limit".into(),
            body: "Extract retry helper into its own module.".into(),
            url: "https://github.com/acme/gateway/pull/99".into(),
            author: "bot".into(),
            head_sha: "1".repeat(40),
            base_sha: "2".repeat(40),
            draft: false,
        },
        files: vec![
            ChangedFile {
                filename: "src/a_format.rs".into(),
                previous_filename: None,
                status: "modified".into(),
                additions: 2,
                deletions: 1,
                patch: Some(
                    "@@ -1,2 +1,3 @@\n fn keep() {\n-    run(1, 2);\n+    // Formatting cleanup\n+        run(1, 2);\n }"
                        .into(),
                ),
            },
            ChangedFile {
                filename: "src/b_old.rs".into(),
                previous_filename: None,
                status: "modified".into(),
                additions: 0,
                deletions: 4,
                patch: Some(
                    "@@ -1,5 +1,1 @@\n fn caller() {}\n-fn compute_backoff(step: u32) -> u64 {\n-    let max_attempts = 3;\n-    u64::from(step.min(max_attempts)) * 50\n-}"
                        .into(),
                ),
            },
            ChangedFile {
                filename: "src/c_new.rs".into(),
                previous_filename: None,
                status: "added".into(),
                additions: 4,
                deletions: 0,
                patch: Some(
                    "@@ -0,0 +1,4 @@\n+fn compute_backoff(step: u32) -> u64 {\n+    let max_attempts = 30;\n+    u64::from(step.min(max_attempts)) * 50\n+}"
                        .into(),
                ),
            },
            ChangedFile {
                filename: "tests/d_backoff.rs".into(),
                previous_filename: None,
                status: "added".into(),
                additions: 2,
                deletions: 0,
                patch: Some(
                    "@@ -0,0 +1,2 @@\n+#[test]\n+fn smoke_backoff() { assert_eq!(1, 1); }".into(),
                ),
            },
        ],
        comments: Vec::new(),
    }
}

#[test]
fn sieve_vaporizes_moves_comments_and_tests_while_isolating_sneaky_edits_in_both_layouts() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    let data = multi_file_sieve_fixture();
    app.loaded(data).unwrap();

    // Check deterministic sieve report directly (row 0 is the @@ hunk header).
    let report = app.review.as_ref().unwrap().sieve_report();
    // Only `let max_attempts = 3;` (in b_old.rs) and `let max_attempts = 30;` (in c_new.rs)
    // are Unaccounted; the surrounding moved function lines, formatting file, and test file are accounted!
    assert_eq!(report.unaccounted, 2);
    assert_eq!(
        report.files[0].unaccounted, 0,
        "a_format.rs is purely mechanical"
    );
    assert_eq!(
        report.files[1].unaccounted, 1,
        "b_old.rs has the removed `3` line"
    );
    assert_eq!(
        report.files[2].unaccounted, 1,
        "c_new.rs has the sneaky `30` line"
    );
    assert_eq!(
        report.files[3].unaccounted, 0,
        "tests/d_backoff.rs is test code"
    );
    assert_eq!(report.files[2].rows[0], RowClass::Unchanged);
    assert!(matches!(report.files[2].rows[1], RowClass::Moved { .. }));
    assert_eq!(report.files[2].rows[2], RowClass::Unaccounted);
    assert!(matches!(report.files[2].rows[3], RowClass::Moved { .. }));
    assert!(matches!(report.files[2].rows[4], RowClass::Moved { .. }));

    // Enable Sieve ('z') on file 0 (a_format.rs, which has 0 unaccounted lines).
    press(&mut app, 'z').unwrap();
    assert!(app.review.as_ref().unwrap().sieve);

    // Pressing ']' skips a_format.rs completely and lands directly on b_old.rs's unaccounted line (`max_attempts = 3`)!
    press(&mut app, ']').unwrap();
    {
        let review = app.review.as_ref().unwrap();
        assert_eq!(review.selected_file, 1);
        let row = &review.current_lines()[review.position()];
        assert!(
            row.text.contains("let max_attempts = 3;"),
            "landed on {row:?}"
        );
    }

    // Pressing ']' again jumps straight to c_new.rs's sneaky `let max_attempts = 30;` (skipping moved header line 1)!
    press(&mut app, ']').unwrap();
    {
        let review = app.review.as_ref().unwrap();
        assert_eq!(review.selected_file, 2);
        let row = &review.current_lines()[review.position()];
        assert!(
            row.text.contains("let max_attempts = 30;"),
            "landed on {row:?}"
        );
    }

    // Pressing ']' again skips tests/d_backoff.rs cleanly since it has 0 unaccounted lines!
    press(&mut app, ']').unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 2);
    assert_eq!(app.review.as_ref().unwrap().position(), 2);

    // Unified Sieve screen shows move provenance (`⇄ from src/b_old.rs`) and `◆1` sidebar badges.
    let unified = screen(&mut app);
    assert!(unified.contains("SIEVE ◆2 unaccounted"));
    assert!(unified.contains("⇄ from src/b_old.rs"));
    assert!(unified.contains("◆1"));

    // Switch to Split layout ('|') and verify rendering + '[' backwards jump across unaccounted lines.
    press(&mut app, '|').unwrap();
    let split = screen_sized(&mut app, 160, 50);
    assert!(split.contains("SIEVE ◆2 unaccounted"));
    press(&mut app, '[').unwrap();
    assert_eq!(app.review.as_ref().unwrap().selected_file, 1);
}

#[test]
fn agent_picker_from_oracle_returns_to_oracle_and_requests_generation() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), false);
    app.config_path = Some(directory.path().join("config.toml"));
    let data = app::demo_data();
    app.opened(overview_for(&data));
    app.accept_diff(data).unwrap();
    assert_eq!(app.settings.ai.provider, Provider::None);

    // Open Oracle with Provider::None -> Idle prompt, then press 'A' and move up to Codex.
    assert!(matches!(press(&mut app, 'O').unwrap(), Action::None));
    assert!(matches!(app.oracle.status, OracleStatus::Idle));
    assert!(screen(&mut app).contains("Judge Concrete Cases Instead of Simulating Syntax"));

    press(&mut app, 'A').unwrap();
    assert!(matches!(app.dialog, Some(Dialog::AgentPicker { .. })));
    press(&mut app, 'k').unwrap();
    press(&mut app, 'k').unwrap();
    press(&mut app, 'k').unwrap();
    let action = app.key(key(KeyCode::Enter)).unwrap();
    assert_eq!(app.settings.ai.provider, Provider::Codex);
    assert!(matches!(app.dialog, Some(Dialog::Oracle)));
    assert!(matches!(action, Action::Oracle { force: false }));
}

#[test]
fn command_palette_lists_behavioral_oracle_and_sieve_actions() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(directory.path().into(), true);
    app.loaded(app::demo_data()).unwrap();

    app.key(ctrl('k')).unwrap();
    app.paste("sieve");
    assert!(screen(&mut app).contains("Sieve diff to unaccounted logic"));
    app.key(key(KeyCode::Enter)).unwrap();
    assert!(app.review.as_ref().unwrap().sieve);

    app.key(ctrl('k')).unwrap();
    app.paste("oracle");
    assert!(screen(&mut app).contains("Behavioral oracle"));
    app.key(key(KeyCode::Enter)).unwrap();
    assert!(matches!(app.dialog, Some(Dialog::Oracle)));

    // Ensure sieve module re-export works cleanly from public crate API too.
    let review = app.review.as_ref().unwrap();
    let report = sieve::analyze(&review.data.files, &review.lines, &[], &Default::default());
    assert_eq!(report.unaccounted, 5);
}
