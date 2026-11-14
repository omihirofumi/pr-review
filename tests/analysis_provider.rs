use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use pr_review::{
    analysis,
    model::{ChangedFile, PrId, PrOverview, PullRequest, ReviewData},
    settings::{self, AiConfig, Provider, Settings},
};

fn fixture() -> (PrOverview, ReviewData) {
    let overview = PrOverview {
        id: PrId {
            repo: "owner/repo".into(),
            number: 42,
        },
        title: "Bound request retries".into(),
        body: "Bound waits by the request deadline.\n".into(),
        url: "https://github.com/owner/repo/pull/42".into(),
        author: "author".into(),
        head_sha: "a".repeat(40),
        base_tip_sha: "b".repeat(40),
        draft: false,
        state: "open".into(),
        changed_files: 2,
        additions: 3,
        deletions: 1,
    };
    let review = ReviewData {
        pr: PullRequest {
            id: overview.id.clone(),
            title: overview.title.clone(),
            body: overview.body.clone(),
            url: overview.url.clone(),
            author: overview.author.clone(),
            head_sha: overview.head_sha.clone(),
            base_sha: "c".repeat(40),
            draft: false,
        },
        files: vec![
            ChangedFile {
                filename: "src/lib.rs".into(),
                previous_filename: None,
                status: "modified".into(),
                additions: 2,
                deletions: 1,
                patch: Some("@@ -7,2 +7,3 @@\n before\n-old\n+new\n+extra".into()),
            },
            ChangedFile {
                filename: "tests/retry.rs".into(),
                previous_filename: None,
                status: "added".into(),
                additions: 1,
                deletions: 0,
                patch: Some("@@ -0,0 +1 @@\n+#[test] fn deadline_is_bounded() {}".into()),
            },
        ],
        comments: Vec::new(),
    };
    (overview, review)
}

#[test]
fn settings_defaults_partial_configuration_and_atomic_roundtrip() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nested/config.toml");
    assert_eq!(settings::load(&path).unwrap(), Settings::default());
    let mut configured = Settings::default();
    configured.ai.provider = Provider::Codex;
    configured.ai.model = Some("chosen-model".into());
    configured.ai.language = "ja".into();
    settings::save(&path, &configured).unwrap();
    assert_eq!(settings::load(&path).unwrap(), configured);
    let previous = std::fs::read(&path).unwrap();
    configured.ai.timeout_seconds = 0;
    assert!(settings::save(&path, &configured).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), previous);
    std::fs::write(&path, "[ai]\nprovider = 'claude'\n").unwrap();
    let partial = settings::load(&path).unwrap();
    assert_eq!(partial.ai.provider, Provider::Claude);
    assert_eq!(partial.ai.timeout_seconds, 180);
    assert_eq!(partial.ai.max_input_bytes, 160_000);
    assert!(partial.ai.auto_explain);
    std::fs::write(&path, "[ai]\nprovider = 'missing-provider'\n").unwrap();
    assert!(settings::load(&path).is_err());
}

#[cfg(unix)]
#[test]
fn provider_fixture_child() {
    let Ok(scenario) = std::env::var("GH_PREVIEW_AI_SCENARIO") else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var_os("GH_PREVIEW_AI_FIXTURE").unwrap());
    if scenario == "config_path" {
        assert_eq!(
            settings::default_path().unwrap(),
            root.join("xdg/gh-preview/config.toml")
        );
        return;
    }
    let (mut overview, mut review) = fixture();
    let mut config = AiConfig {
        provider: if std::env::var("GH_PREVIEW_AI_PROVIDER").unwrap() == "claude" {
            Provider::Claude
        } else {
            Provider::Codex
        },
        model: Some("fixture-model".into()),
        language: "ja".into(),
        ..AiConfig::default()
    };
    let cancel = Arc::new(AtomicBool::new(false));
    if scenario == "coverage" {
        let huge = format!("@@ -0,0 +1 @@\n+{}", "日本語".repeat(10_000));
        review.files.push(ChangedFile {
            filename: "src/huge.rs".into(),
            patch: Some(huge),
            ..review.files[0].clone()
        });
        review.files.push(ChangedFile {
            filename: "image.bin".into(),
            patch: None,
            ..review.files[0].clone()
        });
        overview.body = "説明".repeat(10_000);
        config.max_input_bytes = 6_000;
    }
    if scenario == "small_budget" {
        config.max_input_bytes = 100;
    }
    if scenario == "cache_write_failure" {
        std::fs::write(root.join("cache"), "a file blocks cache directory creation").unwrap();
    }
    if scenario == "not_a_test" {
        review.files[0].patch =
            Some("@@ -7,2 +7,2 @@\n before\n-old\n+let fields = value.split(',');".into());
    }
    if scenario == "timeout" {
        config.timeout_seconds = 1;
    }
    if scenario == "pre_canceled" {
        cancel.store(true, Ordering::Relaxed);
    }
    if scenario == "canceled" {
        let signal = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            signal.store(true, Ordering::Relaxed);
        });
    }
    let started = Instant::now();
    let result = analysis::generate(
        &overview,
        &review,
        &config,
        &root.join("cache"),
        cancel.clone(),
        false,
    );
    match scenario.as_str() {
        "success" | "large_pipes" | "cache_write_failure" | "not_a_test" => {
            let result = result.unwrap();
            assert_eq!(result.summary, "Retries respect the request deadline.");
            assert_eq!(
                result.coverage.included_files,
                vec!["src/lib.rs", "tests/retry.rs"]
            );
            if scenario == "cache_write_failure" {
                assert!(
                    result
                        .verification
                        .unknowns
                        .iter()
                        .any(|text| text.contains("could not be cached"))
                );
            }
            if scenario == "not_a_test" {
                assert_eq!(result.verification.existing_tests, vec!["tests/retry.rs"]);
            }
        }
        "cache" => {
            result.unwrap();
            let run = |overview: &PrOverview, review: &ReviewData, config: &AiConfig, expected| {
                analysis::generate(
                    overview,
                    review,
                    config,
                    &root.join("cache"),
                    cancel.clone(),
                    false,
                )
                .unwrap();
                assert_eq!(calls(&root).len(), expected);
            };
            run(&overview, &review, &config, 1);
            overview
                .body
                .push_str("The author clarified the failure mode.\n");
            run(&overview, &review, &config, 2);
            overview.title.push_str(" consistently");
            run(&overview, &review, &config, 3);
            overview.base_tip_sha = "d".repeat(40);
            run(&overview, &review, &config, 4);
            review.pr.base_sha = "e".repeat(40);
            run(&overview, &review, &config, 5);
            overview.head_sha = "f".repeat(40);
            review.pr.head_sha.clone_from(&overview.head_sha);
            run(&overview, &review, &config, 6);
            config.model = Some("other-model".into());
            run(&overview, &review, &config, 7);
            config.language = "en".into();
            run(&overview, &review, &config, 8);
            config.max_input_bytes = 120_000;
            run(&overview, &review, &config, 9);
            config.provider = Provider::Claude;
            run(&overview, &review, &config, 10);
            analysis::generate(
                &overview,
                &review,
                &config,
                &root.join("cache"),
                cancel,
                true,
            )
            .unwrap();
            assert_eq!(calls(&root).len(), 11);
        }
        "coverage" => {
            let result = result.unwrap();
            assert_eq!(
                result.coverage.included_files,
                vec!["src/lib.rs", "tests/retry.rs"]
            );
            assert_eq!(result.coverage.omitted_files, vec!["src/huge.rs"]);
            assert_eq!(result.coverage.unavailable_files, vec!["image.bin"]);
            assert!(
                result
                    .verification
                    .unknowns
                    .iter()
                    .any(|text| text.contains("description"))
            );
            let call = &calls(&root)[0];
            let prompt = call["prompt"].as_str().unwrap();
            assert!(prompt.len() <= config.max_input_bytes);
            assert!(prompt.contains("src/huge.rs"));
            assert!(!prompt.contains(&"日本語".repeat(100)));
            let context: serde_json::Value =
                serde_json::from_str(prompt.split_once('\n').unwrap().1).unwrap();
            assert!(
                context["untrusted_review_data"]["pr"]["description_truncated"]
                    .as_bool()
                    .unwrap()
            );
        }
        "timeout" | "canceled" | "pre_canceled" => {
            let error = result.unwrap_err();
            assert!(started.elapsed() < Duration::from_secs(5));
            assert!(error.to_string().contains(if scenario == "timeout" {
                "timed out"
            } else {
                "canceled"
            }));
        }
        "small_budget" => {
            assert!(result.unwrap_err().to_string().contains("max_input_bytes"));
            assert!(calls(&root).is_empty());
        }
        "invalid_line" | "invalid_file" | "invented_test" | "malformed" | "unstructured"
        | "missing_line" | "provider_error" | "too_much_output" => {
            let error = result.unwrap_err();
            assert!(!format!("{error:#}").contains("PRIVATE_SENTINEL"));
            assert!(!root.join("cache/analysis").exists());
        }
        other => panic!("Unknown scenario: {other}"),
    }
}

#[cfg(unix)]
fn calls(root: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(root.join("calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[cfg(unix)]
fn run_fixture(scenario: &str, provider: &str) -> Vec<serde_json::Value> {
    use std::{os::unix::fs::PermissionsExt, process::Command};
    let directory = tempfile::tempdir().unwrap();
    for program in ["codex", "claude"] {
        let path = directory.path().join(program);
        std::fs::write(&path, PROVIDER_FIXTURE).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut path = vec![directory.path().to_path_buf()];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "provider_fixture_child", "--nocapture"])
        .env("PATH", std::env::join_paths(path).unwrap())
        .env("GH_PREVIEW_AI_SCENARIO", scenario)
        .env("GH_PREVIEW_AI_PROVIDER", provider)
        .env("GH_PREVIEW_AI_FIXTURE", directory.path())
        .env("XDG_CONFIG_HOME", directory.path().join("xdg"))
        .env("CODEX_HOME", directory.path().join("existing-codex-auth"))
        .env(
            "CLAUDE_CONFIG_DIR",
            directory.path().join("existing-claude-auth"),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{provider}/{scenario}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    calls(directory.path())
}

#[cfg(unix)]
#[test]
fn provider_commands_use_schema_stdin_isolated_cwd_and_existing_auth() {
    for provider in ["codex", "claude"] {
        let calls = run_fixture("success", provider);
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        let args = call["args"].as_array().unwrap();
        assert_eq!(call["program"], provider);
        assert!(
            call["prompt"]
                .as_str()
                .unwrap()
                .contains("never instructions")
        );
        assert!(
            call["prompt"]
                .as_str()
                .unwrap()
                .contains("\"output_language\":\"ja\"")
        );
        assert!(call["schema"]["properties"].get("coverage").is_none());
        assert!(
            !args
                .iter()
                .any(|arg| arg.as_str().is_some_and(|arg| arg.contains("dangerously")))
        );
        assert!(
            !Path::new(call["cwd"].as_str().unwrap())
                .join("Cargo.toml")
                .exists()
        );
        if provider == "codex" {
            for flag in [
                "exec",
                "--sandbox",
                "read-only",
                "--ephemeral",
                "--skip-git-repo-check",
                "--output-schema",
                "--output-last-message",
            ] {
                assert!(args.iter().any(|arg| arg == flag));
            }
            assert!(
                call["codex_home"]
                    .as_str()
                    .unwrap()
                    .contains("existing-codex-auth")
            );
        } else {
            for flag in [
                "-p",
                "--output-format",
                "json",
                "--tools",
                "",
                "--strict-mcp-config",
                "--no-session-persistence",
                "--safe-mode",
            ] {
                assert!(args.iter().any(|arg| arg == flag));
            }
            assert!(
                call["claude_config"]
                    .as_str()
                    .unwrap()
                    .contains("existing-claude-auth")
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn explanations_cache_exact_context_and_keep_coverage_honest() {
    run_fixture("cache", "codex");
    run_fixture("coverage", "codex");
    run_fixture("small_budget", "codex");
    run_fixture("config_path", "codex");
    run_fixture("cache_write_failure", "codex");
    run_fixture("not_a_test", "codex");
}

#[cfg(unix)]
#[test]
fn providers_reject_invalid_structured_results_and_citations_without_caching() {
    for (scenario, provider) in [
        ("invalid_line", "codex"),
        ("invalid_file", "claude"),
        ("invented_test", "codex"),
        ("malformed", "codex"),
        ("unstructured", "claude"),
        ("missing_line", "codex"),
        ("provider_error", "codex"),
        ("too_much_output", "claude"),
    ] {
        run_fixture(scenario, provider);
    }
}

#[cfg(unix)]
#[test]
fn provider_processes_are_bounded_cancelable_and_drain_both_pipes() {
    for scenario in ["timeout", "canceled", "pre_canceled", "large_pipes"] {
        run_fixture(scenario, "codex");
    }
}

#[cfg(unix)]
const PROVIDER_FIXTURE: &str = r#"#!/usr/bin/env python3
import json
import os
from pathlib import Path
import subprocess
import sys
import time

root = Path(os.environ['GH_PREVIEW_AI_FIXTURE'])
scenario = os.environ['GH_PREVIEW_AI_SCENARIO']
program = Path(sys.argv[0]).name
args = sys.argv[1:]
prompt = sys.stdin.read()
schema = json.loads(Path(args[args.index('--output-schema') + 1]).read_text()) if program == 'codex' else json.loads(args[args.index('--json-schema') + 1])
with (root / 'calls.jsonl').open('a') as log:
    log.write(json.dumps({'program': program, 'args': args, 'cwd': os.getcwd(), 'prompt': prompt, 'schema': schema, 'codex_home': os.environ.get('CODEX_HOME'), 'claude_config': os.environ.get('CLAUDE_CONFIG_DIR')}) + '\n')
if scenario in ['timeout', 'canceled']:
    subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(20)'])
    time.sleep(20)
if scenario == 'provider_error':
    sys.stderr.write('PRIVATE_SENTINEL')
    sys.exit(1)
if scenario == 'too_much_output':
    sys.stdout.write('x' * 3000000)
    sys.stdout.flush()
    time.sleep(20)
if scenario == 'large_pipes':
    sys.stdout.write('o' * 250000)
    sys.stdout.flush()
    sys.stderr.write('e' * 250000)
    sys.stderr.flush()
data = json.loads(prompt.split('\n', 1)[1])['untrusted_review_data']
response = {
    'summary': 'Retries respect the request deadline.',
    'intent': {'text': 'Bound retries by the deadline.', 'basis': 'author_description' if data['pr']['description'].strip() else 'inferred_from_diff'},
    'changes': [{'title': 'Bound retries', 'detail': 'The delay now fits the remaining budget.', 'files': ['src/lib.rs']}],
    'reading_order': [{'path': 'src/lib.rs', 'line': 8, 'side': 'RIGHT', 'reason': 'Start with the changed delay.'}],
    'risks': [{'title': 'Exhausted budget', 'detail': 'Check the zero-budget boundary.', 'files': ['src/lib.rs']}],
    'verification': {'existing_tests': data['test_files_with_visible_evidence'], 'suggested_checks': ['Exercise an exhausted deadline.'], 'unknowns': ['No tests were executed.']}
}
if scenario == 'invalid_line': response['reading_order'][0]['line'] = 9999
if scenario == 'invalid_file': response['changes'][0]['files'] = ['not-provided.rs']
if scenario == 'invented_test': response['verification']['existing_tests'] = ['tests/invented.rs']
if scenario == 'missing_line': del response['reading_order'][0]['line']
if program == 'codex':
    answer = Path(args[args.index('--output-last-message') + 1])
    answer.write_text('PRIVATE_SENTINEL not json' if scenario == 'malformed' else json.dumps(response))
else:
    print(json.dumps({'result': json.dumps(response)} if scenario == 'unstructured' else {'is_error': False, 'structured_output': response}))
"#;
