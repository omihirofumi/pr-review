#[cfg(unix)]
mod unix {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    use prpr::{github, model::PrId};

    #[test]
    fn staged_fixture_child() {
        let Ok(scenario) = std::env::var("PRPR_STAGED_SCENARIO") else {
            return;
        };
        let id = PrId {
            repo: "owner/repo".into(),
            number: 42,
        };
        let mut overview = github::load_overview(&id).unwrap();
        assert_eq!(overview.title, "Explain the invariant");
        assert_eq!(overview.body, "Description is available before the patch.");
        assert_eq!(overview.author, "author");
        assert_eq!(overview.head_sha, "a".repeat(40));
        assert_eq!(overview.base_tip_sha, "b".repeat(40));
        assert_eq!(overview.additions, 7);
        assert_eq!(overview.deletions, 3);
        match scenario.as_str() {
            "overview_only" => {}
            "diff_independent" => {
                let data = github::load_diff(&overview).unwrap();
                assert_eq!(data.pr.base_sha, "c".repeat(40));
                assert_eq!(data.pr.head_sha, overview.head_sha);
                assert_eq!(data.files.len(), 2);
                assert!(data.comments.is_empty());
            }
            "comments_independent" => {
                let comments = github::load_comments(&overview).unwrap();
                assert_eq!(comments.len(), 2);
                assert_eq!(comments[0].line_label(), "1–2");
                assert_eq!(comments[0].start_side.as_deref(), Some("RIGHT"));
                assert_eq!(comments[1].in_reply_to_id, Some(1));
            }
            "diff_head_moved" | "diff_base_moved" => {
                let error = github::load_diff(&overview).unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("PR changed since the overview loaded")
                );
            }
            "comments_head_moved" | "comments_base_moved" => {
                let error = github::load_comments(&overview).unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("PR changed since the overview loaded")
                );
            }
            "incomplete_files" => {
                let error = github::load_diff(&overview).unwrap_err();
                assert!(error.to_string().contains("2 of 3 changed files"));
            }
            "large_overview" => {
                assert_eq!(overview.changed_files, 3001);
                assert!(
                    github::load_diff(&overview)
                        .unwrap_err()
                        .to_string()
                        .contains("limited to 3000")
                );
            }
            "comments_failure" => {
                assert!(github::load_comments(&overview).is_err());
                assert_eq!(github::load_diff(&overview).unwrap().files.len(), 2);
            }
            "diff_failure" => {
                assert!(github::load_diff(&overview).is_err());
                assert_eq!(github::load_comments(&overview).unwrap().len(), 2);
            }
            "invalid_overview" => {
                overview.head_sha = "bad-sha".into();
                assert!(github::load_diff(&overview).is_err());
                assert!(github::load_comments(&overview).is_err());
            }
            other => panic!("Unknown scenario: {other}"),
        }
    }

    #[test]
    fn overview_diff_and_comments_have_independent_revision_checked_requests() {
        for scenario in [
            "overview_only",
            "diff_independent",
            "comments_independent",
            "diff_head_moved",
            "diff_base_moved",
            "comments_head_moved",
            "comments_base_moved",
            "incomplete_files",
            "large_overview",
            "comments_failure",
            "diff_failure",
            "invalid_overview",
        ] {
            let fixture = tempfile::tempdir().unwrap();
            let executable = fixture.path().join("gh");
            fs::write(&executable, SCRIPT).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            let mut paths = vec![fixture.path().to_path_buf()];
            paths.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "unix::staged_fixture_child", "--nocapture"])
                .env("PATH", std::env::join_paths(paths).unwrap())
                .env("PRPR_STAGED_FIXTURE", fixture.path())
                .env("PRPR_STAGED_SCENARIO", scenario)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{scenario}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let log = fs::read_to_string(fixture.path().join("calls.jsonl")).unwrap();
            let calls: Vec<Vec<String>> = log
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert!(calls.iter().all(|args| args.iter().any(|arg| arg == "GET")));
            if matches!(
                scenario,
                "overview_only" | "large_overview" | "invalid_overview"
            ) {
                assert_eq!(
                    calls.len(),
                    1,
                    "{scenario} must not fetch a diff or comments"
                );
                assert_eq!(
                    calls[0],
                    vec!["api", "--method", "GET", "repos/owner/repo/pulls/42"]
                );
            }
            if (scenario.starts_with("diff_") || scenario == "incomplete_files")
                && scenario != "diff_failure"
            {
                assert!(!calls.iter().flatten().any(|arg| arg.contains("/comments?")));
            }
            if scenario.starts_with("comments_") && scenario != "comments_failure" {
                assert!(
                    !calls
                        .iter()
                        .flatten()
                        .any(|arg| arg.contains("/files?") || arg.contains("/compare/"))
                );
            }
            for args in calls.iter().filter(|args| {
                args.iter()
                    .any(|arg| arg.contains("/files?") || arg.contains("/comments?"))
            }) {
                assert!(args.iter().any(|arg| arg == "--paginate"));
                assert!(args.iter().any(|arg| arg == "--slurp"));
            }
        }
    }

    const SCRIPT: &str = r#"#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
import time

root = Path(os.environ['PRPR_STAGED_FIXTURE'])
scenario = os.environ['PRPR_STAGED_SCENARIO']
args = sys.argv[1:]
with (root / 'calls.jsonl').open('a') as log:
    log.write(json.dumps(args) + '\n')
endpoint = next((arg for arg in args if arg.startswith('repos/')), '')
if endpoint == 'repos/owner/repo/pulls/42':
    counter = root / 'fetch_count'
    count = int(counter.read_text()) + 1 if counter.exists() else 1
    counter.write_text(str(count))
    response = {
        'title': 'Explain the invariant', 'body': 'Description is available before the patch.',
        'html_url': 'https://github.com/owner/repo/pull/42', 'user': {'login': 'author'},
        'head': {'sha': ('d' if count > 1 and scenario.endswith('head_moved') else 'a') * 40},
        'base': {'sha': ('d' if count > 1 and scenario.endswith('base_moved') else 'b') * 40},
        'draft': False, 'state': 'open', 'additions': 7, 'deletions': 3,
        'changed_files': 3001 if scenario == 'large_overview' else (3 if scenario == 'incomplete_files' else 2)
    }
elif '/compare/' in endpoint or '/files?' in endpoint:
    if scenario == 'overview_only':
        sys.stderr.write('Overview must not wait for diff endpoints')
        sys.exit(1)
    if scenario == 'diff_independent':
        mine = 'compare_ready' if '/compare/' in endpoint else 'files_ready'
        other = 'files_ready' if mine == 'compare_ready' else 'compare_ready'
        (root / mine).touch()
        deadline = time.monotonic() + 5
        while not (root / other).exists():
            if time.monotonic() >= deadline:
                sys.stderr.write('Diff base and files were not fetched concurrently')
                sys.exit(1)
            time.sleep(0.01)
    if '/compare/' in endpoint:
        response = {'merge_base_commit': {'sha': 'c' * 40}}
    else:
        if scenario == 'diff_failure':
            sys.stderr.write('Changed files unavailable')
            sys.exit(1)
        response = [[{
            'filename': name, 'previous_filename': None, 'status': 'modified',
            'additions': 1, 'deletions': 1, 'patch': '@@ -1 +1 @@\n-old\n+new'
        }] for name in ['first.rs', 'second.rs']]
elif '/comments?' in endpoint:
    if scenario == 'comments_failure':
        sys.stderr.write('Comments unavailable')
        sys.exit(1)
    response = [[{
        'id': 1, 'path': 'first.rs', 'body': 'Review the entire block', 'user': {'login': 'reviewer'},
        'line': 2, 'original_line': 2, 'side': 'RIGHT', 'start_line': 1, 'start_side': 'RIGHT',
        'in_reply_to_id': None
    }], [{
        'id': 2, 'path': 'first.rs', 'body': 'Reply', 'user': {'login': 'author'},
        'line': 2, 'original_line': 2, 'side': 'RIGHT', 'in_reply_to_id': 1
    }]]
else:
    sys.stderr.write('Unexpected fixture request: ' + repr(args))
    sys.exit(1)
print(json.dumps(response))
"#;
}
