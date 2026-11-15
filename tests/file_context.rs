#[cfg(unix)]
mod unix {
    use prpr::{
        github,
        model::{ChangedFile, PrId, PullRequest},
    };
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

    fn fixture() -> (PullRequest, ChangedFile) {
        let pr = PullRequest {
            id: PrId {
                repo: "owner/repo".into(),
                number: 17,
            },
            title: "Change a line".into(),
            body: String::new(),
            url: "https://github.com/owner/repo/pull/17".into(),
            author: "author".into(),
            head_sha: "a".repeat(40),
            base_sha: "b".repeat(40),
            draft: false,
        };
        let file = ChangedFile {
            filename: "src/new name?.rs".into(),
            previous_filename: Some("src/old #é.rs".into()),
            status: "renamed".into(),
            additions: 1,
            deletions: 1,
            patch: Some("@@ -2 +2 @@\n-old\n+new".into()),
        };
        (pr, file)
    }

    #[test]
    fn file_context_fixture_child() {
        let Ok(scenario) = std::env::var("PRPR_CONTEXT_SCENARIO") else {
            return;
        };
        let root = PathBuf::from(std::env::var_os("PRPR_CONTEXT_ROOT").unwrap());
        let (mut pr, mut file) = fixture();
        if scenario.starts_with("browser") {
            let result = github::open_in_browser(&pr.id);
            assert_eq!(result.is_ok(), scenario == "browser");
            return;
        }
        match scenario.as_str() {
            "added" => {
                file.status = "added".into();
                file.previous_filename = None;
                file.patch = Some("@@ -0,0 +1 @@\n+new".into());
            }
            "removed" => {
                file.status = "removed".into();
                file.patch = Some("@@ -1 +0,0 @@\n-old".into());
            }
            _ => {}
        }
        let cache = root.join("cache");
        if scenario == "cache_blocked" {
            fs::write(&cache, "blocks cache directory").unwrap();
        }
        let result = github::load_file_context(&pr, &file, &cache);
        if [
            "oversize",
            "binary",
            "non_utf8",
            "mismatch",
            "directory",
            "missing",
        ]
        .contains(&scenario.as_str())
        {
            assert!(result.is_err());
            assert!(!cache.exists());
            return;
        }
        let source = result.unwrap();
        if scenario == "added" {
            assert!(source.base.is_empty());
            assert_eq!(source.head, "new\n");
        } else if scenario == "removed" {
            assert_eq!(source.base, "old\n");
            assert!(source.head.is_empty());
        } else {
            assert!(source.base.contains("old\n"));
            assert!(source.head.contains("new\n"));
        }
        if scenario == "cache" {
            github::load_file_context(&pr, &file, &cache).unwrap();
            assert_eq!(calls(&root).len(), 2);
            let entry = fs::read_dir(cache.join("owner/repo"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&entry).unwrap()).unwrap();
            value["source"]["head"] = serde_json::json!("wrong revision");
            fs::write(entry, serde_json::to_vec(&value).unwrap()).unwrap();
            github::load_file_context(&pr, &file, &cache).unwrap();
            assert_eq!(calls(&root).len(), 4);
            pr.head_sha = "c".repeat(40);
            github::load_file_context(&pr, &file, &cache).unwrap();
            assert_eq!(calls(&root).len(), 6);
        }
    }

    fn calls(root: &std::path::Path) -> Vec<serde_json::Value> {
        fs::read_to_string(root.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn run(scenario: &str) -> Vec<serde_json::Value> {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("gh");
        fs::write(&executable, GH).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        let mut path = vec![root.path().to_path_buf()];
        path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "unix::file_context_fixture_child", "--nocapture"])
            .env("PATH", std::env::join_paths(path).unwrap())
            .env("PRPR_CONTEXT_ROOT", root.path())
            .env("PRPR_CONTEXT_SCENARIO", scenario)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{scenario}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        calls(root.path())
    }

    #[test]
    fn exact_revisions_renamed_paths_cache_reuse_and_cache_revalidation() {
        let calls = run("cache");
        assert_eq!(calls.len(), 6);
        assert!(
            calls
                .iter()
                .any(|args| args.to_string().contains("src/old%20%23%C3%A9.rs?ref=bbbb"))
        );
        assert!(
            calls
                .iter()
                .any(|args| args.to_string().contains("src/new%20name%3F.rs?ref=aaaa"))
        );
        assert!(
            calls
                .iter()
                .any(|args| args.to_string().contains("ref=cccc"))
        );
        assert_eq!(run("added").len(), 1);
        assert_eq!(run("removed").len(), 1);
        assert_eq!(run("cache_blocked").len(), 2);
        let large = run("large");
        assert_eq!(large.len(), 4);
        assert_eq!(
            large
                .iter()
                .filter(|args| args.to_string().contains("/git/blobs/"))
                .count(),
            2
        );
    }

    #[test]
    fn unsafe_or_unavailable_file_context_does_not_get_cached() {
        for scenario in [
            "oversize",
            "binary",
            "non_utf8",
            "mismatch",
            "directory",
            "missing",
        ] {
            run(scenario);
        }
    }

    #[test]
    fn browser_uses_the_selected_repository_and_pr_number() {
        assert_eq!(
            run("browser"),
            vec![serde_json::json!([
                "pr",
                "view",
                "17",
                "--repo",
                "owner/repo",
                "--web"
            ])]
        );
        run("browser_error");
    }

    const GH: &str = r#"#!/usr/bin/env python3
import base64, json, os, sys
from pathlib import Path
root = Path(os.environ['PRPR_CONTEXT_ROOT'])
scenario = os.environ['PRPR_CONTEXT_SCENARIO']
args = sys.argv[1:]
with (root / 'calls.jsonl').open('a') as log: log.write(json.dumps(args) + '\n')
if args[0] == 'pr':
    if scenario == 'browser_error': sys.exit(3)
    sys.exit(0)
endpoint = args[-1]
if scenario == 'missing': sys.exit(1)
left = 'ref=' + 'b' * 40 in endpoint or endpoint.endswith('/' + 'd' * 40)
body = ('before\nold\nafter\n' if left else 'before\nnew\nafter\n').encode()
if scenario == 'added': body = b'new\n'
if scenario == 'removed': body = b'old\n'
if scenario == 'binary': body = b'content\x00'
if scenario == 'non_utf8': body = b'\xff'
if scenario == 'mismatch': body = b'wrong revision\n'
if scenario == 'large': body = b'x' * 1000010 + (b'\nold\nafter\n' if left else b'\nnew\nafter\n')
value = {'type': 'dir' if scenario == 'directory' else 'file', 'size': len(body), 'encoding': 'base64', 'content': base64.encodebytes(body).decode(), 'sha': ('d' if left else 'e') * 40}
if scenario == 'large' and '/contents/' in endpoint: value.update(encoding='none', content='')
if scenario == 'oversize': value.update(size=3000000, encoding='none', content='')
print(json.dumps(value))
"#;
}
