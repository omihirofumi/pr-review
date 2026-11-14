#[cfg(unix)]
mod unix {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    use pr_review::github;

    #[test]
    fn repository_fixture_child() {
        let Ok(scenario) = std::env::var("PR_REVIEW_CLI_SCENARIO") else {
            return;
        };
        match scenario.as_str() {
            "cwd" | "no_colocate" => {
                let repo = github::resolve_repo(None).unwrap();
                assert_eq!(repo, "scope/repo");
                let prs = github::list_prs(Some(&repo), None).unwrap();
                assert_eq!(prs.len(), 1);
                assert_eq!(prs[0].id.repo, "scope/repo");
            }
            "numeric" => {
                let target = github::parse_target("123", None).unwrap();
                assert_eq!(target.repo, "scope/repo");
                assert_eq!(target.number, 123);
                github::list_prs(Some(&target.repo), None).unwrap();
            }
            "query" => {
                let prs = github::list_prs(Some("scope/repo"), Some("repo:other/repo author:@me"))
                    .unwrap();
                assert!(prs.is_empty());
            }
            "hostile_results" => {
                let error =
                    github::list_prs(Some("scope/repo"), Some("repo:other/repo")).unwrap_err();
                assert!(error.to_string().contains("does not match"));
            }
            "url" => {
                let target =
                    github::parse_target("https://github.com/url/repo/pull/42", None).unwrap();
                assert_eq!(target.repo, "url/repo");
            }
            "missing_repo" => {
                let error = github::resolve_repo(None).unwrap_err();
                assert!(format!("{error:#}").contains("--repo owner/repo"));
            }
            "malformed" => {
                assert!(github::parse_target("0", None).is_err());
                assert!(github::parse_target("https://github.com/a/b/issues/1", None).is_err());
                assert!(github::resolve_repo(Some("a/../b")).is_err());
            }
            other => panic!("Unknown fixture scenario: {other}"),
        }
    }

    #[test]
    fn cwd_repository_resolution_and_search_scope_are_process_isolated() {
        for scenario in [
            "cwd",
            "numeric",
            "query",
            "hostile_results",
            "url",
            "missing_repo",
            "malformed",
            "no_colocate",
        ] {
            let fixture = tempfile::tempdir().unwrap();
            let commands = fixture.path().join("bin");
            let cwd = fixture.path().join("repo/nested");
            let git_store = fixture.path().join("git-store");
            fs::create_dir_all(&commands).unwrap();
            fs::create_dir_all(&cwd).unwrap();
            fs::create_dir_all(&git_store).unwrap();
            for name in ["gh", "jj"] {
                let executable = commands.join(name);
                fs::write(&executable, SCRIPT).unwrap();
                fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let mut path = vec![commands];
            path.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            let result = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "unix::repository_fixture_child", "--nocapture"])
                .current_dir(&cwd)
                .env("PATH", std::env::join_paths(path).unwrap())
                .env("GH_REPO", "environment/wrong-repo")
                .env("PR_REVIEW_CLI_FIXTURE", fixture.path())
                .env("PR_REVIEW_CLI_SCENARIO", scenario)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{scenario}: {}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            let log = fs::read_to_string(fixture.path().join("calls.jsonl")).unwrap_or_default();
            let calls: Vec<serde_json::Value> = log
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let gh: Vec<_> = calls
                .iter()
                .filter(|call| call["program"] == "gh")
                .collect();
            let discoveries: Vec<_> = gh.iter().filter(|call| call["args"][0] == "repo").collect();
            if scenario == "cwd" || scenario == "numeric" {
                assert_eq!(discoveries.len(), 1, "{scenario}");
                assert_eq!(
                    discoveries[0]["cwd"].as_str().unwrap(),
                    cwd.canonicalize().unwrap().to_str().unwrap()
                );
            }
            if scenario == "no_colocate" {
                assert_eq!(discoveries.len(), 2);
                assert_eq!(
                    discoveries[1]["cwd"].as_str().unwrap(),
                    git_store.canonicalize().unwrap().to_str().unwrap()
                );
            }
            if scenario == "url" || scenario == "malformed" {
                assert!(calls.is_empty(), "{scenario} unexpectedly queried the cwd");
            }
            for call in gh {
                assert!(
                    call["gh_repo"].is_null(),
                    "GH_REPO must not override cwd discovery"
                );
                if call["args"][0] == "pr" {
                    let args = call["args"].as_array().unwrap();
                    assert_eq!(args[1], "list");
                    let repo_flag = args.iter().position(|arg| arg == "--repo").unwrap();
                    assert_eq!(args[repo_flag + 1], "scope/repo");
                    assert!(!args.iter().any(|arg| arg == "--review-requested"));
                    if scenario == "query" {
                        let search_flag = args.iter().position(|arg| arg == "--search").unwrap();
                        assert_eq!(args[search_flag + 1], "repo:other/repo author:@me");
                    }
                }
            }
        }
    }

    const SCRIPT: &str = r#"#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

root = Path(os.environ['PR_REVIEW_CLI_FIXTURE'])
scenario = os.environ['PR_REVIEW_CLI_SCENARIO']
program = Path(sys.argv[0]).name
args = sys.argv[1:]
with (root / 'calls.jsonl').open('a') as log:
    log.write(json.dumps({'program': program, 'args': args, 'cwd': os.getcwd(), 'gh_repo': os.environ.get('GH_REPO')}) + '\n')
if program == 'jj':
    if scenario == 'no_colocate':
        print(root / 'git-store')
        sys.exit(0)
    sys.stderr.write('not a jj repository')
    sys.exit(1)
if args[:2] == ['repo', 'view']:
    if scenario == 'missing_repo' or (scenario == 'no_colocate' and Path.cwd() != (root / 'git-store').resolve()):
        sys.stderr.write('not a git repository')
        sys.exit(1)
    print(json.dumps({'nameWithOwner': 'scope/repo'}))
elif args[:2] == ['pr', 'list']:
    if scenario == 'query':
        print('[]')
    else:
        repo = 'other/repo' if scenario == 'hostile_results' else 'scope/repo'
        print(json.dumps([{'number': 42, 'title': 'Scoped PR', 'author': {'login': 'author'}, 'isDraft': False, 'url': 'https://github.com/' + repo + '/pull/42'}]))
else:
    sys.stderr.write('Unexpected request: ' + repr(args))
    sys.exit(1)
"#;
}

#[test]
fn both_installed_commands_expose_the_canonical_help() {
    for executable in [
        env!("CARGO_BIN_EXE_gh-preview"),
        env!("CARGO_BIN_EXE_pr-review"),
    ] {
        let result = std::process::Command::new(executable)
            .arg("--help")
            .output()
            .unwrap();
        assert!(result.status.success());
        let help = String::from_utf8(result.stdout).unwrap();
        assert!(help.contains("Usage: gh-preview"));
        assert!(help.contains("PR URL or number"));
        assert!(help.contains("current directory"));
    }
}
