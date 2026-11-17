//! Runs each provider's own code-review command so prpr follows the provider's review practice
//! instead of maintaining a review prompt of its own.

use std::{path::Path, process::Command, sync::atomic::AtomicBool, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use crate::{
    analysis::run_bounded,
    model::PullRequest,
    settings::{AiConfig, Provider},
};

/// Read-only tools for Claude's `/code-review`; anything else is denied in print mode, so
/// instructions planted in the PR cannot post to GitHub or change files.
const CLAUDE_TOOLS: &[&str] = &[
    "Read",
    "Grep",
    "Glob",
    "Agent",
    "Skill",
    "Bash(gh pr view:*)",
    "Bash(gh pr diff:*)",
    "Bash(gh pr checks:*)",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(git blame:*)",
    "Bash(git rev-parse:*)",
    "Bash(git merge-base:*)",
    "Bash(git status:*)",
    "Bash(git ls-files:*)",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    pub line: u32,
    pub summary: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewOutput {
    pub text: String,
    pub findings: Vec<Finding>,
}

pub fn run(
    config: &AiConfig,
    pr: &PullRequest,
    checkout: &Path,
    cancel: &AtomicBool,
) -> Result<ReviewOutput> {
    let timeout = Duration::from_secs(config.review_timeout_seconds);
    let text = match config.provider {
        Provider::Claude => {
            let mut command = Command::new("claude");
            command
                .current_dir(checkout)
                .args(["-p", "--output-format", "json", "--allowedTools"])
                .args(CLAUDE_TOOLS)
                .args(["--disallowedTools", "Edit", "Write", "NotebookEdit"]);
            if let Some(model) = &config.model {
                command.args(["--model", model]);
            }
            let output = run_bounded(
                &mut command,
                format!("/code-review {}", pr.id.number).into_bytes(),
                timeout,
                cancel,
            )?;
            claude_result(&output.stdout, &output.stderr)?
        }
        Provider::Codex => {
            ensure!(
                at_pr_head(checkout, pr),
                "Codex reviews your checkout, which is not at PR #{} head {}; press w to check it out",
                pr.id.number,
                &pr.head_sha[..pr.head_sha.len().min(8)],
            );
            let directory = tempfile::tempdir()?;
            let report = directory.path().join("review.md");
            let mut command = Command::new("codex");
            command
                .current_dir(checkout)
                .args(["exec", "review", "--base", &pr.base_sha])
                .args(["-c", "sandbox_mode=\"read-only\"", "-o"])
                .arg(&report);
            if let Some(model) = &config.model {
                command.args(["-m", model]);
            }
            let output = run_bounded(&mut command, Vec::new(), timeout, cancel)?;
            ensure!(
                output.status.success(),
                "Codex review failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            std::fs::read_to_string(&report).context("Codex did not write a review")?
        }
        Provider::None => bail!("Choose Codex or Claude before running an AI review"),
    };
    Ok(ReviewOutput {
        findings: findings(&text),
        text,
    })
}

/// Whether the checkout's Git HEAD is the PR head, so file reads and line numbers in a review
/// refer to the reviewed code.
pub fn at_pr_head(checkout: &Path, pr: &PullRequest) -> bool {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(checkout)
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == pr.head_sha)
}

fn claude_result(stdout: &[u8], stderr: &[u8]) -> Result<String> {
    let envelope: Value = serde_json::from_slice(stdout).with_context(|| {
        format!(
            "Claude returned no review: {}",
            String::from_utf8_lossy(stderr).trim()
        )
    })?;
    let text = envelope["result"].as_str().unwrap_or_default().to_owned();
    ensure!(
        envelope["is_error"] != true && !text.trim().is_empty(),
        "Claude review failed: {}",
        if text.is_empty() { "no result" } else { &text }
    );
    Ok(text)
}

/// Line-anchored findings from the last fenced JSON array in the review, when the provider
/// emits one; reviews without it are shown as text only.
pub fn findings(text: &str) -> Vec<Finding> {
    let Some(start) = text.rfind("```json") else {
        return Vec::new();
    };
    let body = &text[start + "```json".len()..];
    let Some(end) = body.find("```") else {
        return Vec::new();
    };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(body[..end].trim()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            Some(Finding {
                file: item["file"].as_str()?.to_owned(),
                line: u32::try_from(item["line"].as_u64()?).ok()?,
                summary: item["summary"].as_str()?.to_owned(),
                detail: item["failure_scenario"]
                    .as_str()
                    .or(item["detail"].as_str())
                    .map(str::to_owned),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn findings_come_from_the_last_json_block_and_skip_incomplete_items() {
        let text = "Intro\n```json\n[{\"file\": \"old.rs\", \"line\": 1, \"summary\": \"stale\"}]\n```\nThen\n```json\n[\n  {\"file\": \"lib.rs\", \"line\": 3, \"summary\": \"Wrong sign\", \"failure_scenario\": \"add(2,3) is -1\"},\n  {\"file\": \"lib.rs\", \"summary\": \"no line\"}\n]\n```\n";
        assert_eq!(
            findings(text),
            [Finding {
                file: "lib.rs".into(),
                line: 3,
                summary: "Wrong sign".into(),
                detail: Some("add(2,3) is -1".into()),
            }]
        );
        assert!(findings("No JSON here").is_empty());
        assert!(findings("```json\nnot json\n```").is_empty());
    }

    #[test]
    fn claude_errors_surface_the_result_text() {
        let error =
            claude_result(br#"{"is_error": true, "result": "Not logged in"}"#, b"").unwrap_err();
        assert!(error.to_string().contains("Not logged in"));
        assert_eq!(
            claude_result(br#"{"is_error": false, "result": "Looks good"}"#, b"").unwrap(),
            "Looks good"
        );
    }
}
