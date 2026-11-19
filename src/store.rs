use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use prpr::model::{DraftComment, PrId, PullRequest};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct DraftFile {
    id: PrId,
    head_sha: String,
    base_sha: String,
    comments: Vec<DraftComment>,
}

pub fn draft_path(root: &Path, pr: &PullRequest) -> PathBuf {
    root.join("drafts").join(&pr.id.repo).join(format!(
        "{}-{}-{}.json",
        pr.id.number, pr.head_sha, pr.base_sha
    ))
}

pub fn load(root: &Path, pr: &PullRequest) -> Result<Vec<DraftComment>> {
    let path = draft_path(root, pr);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let saved: DraftFile = serde_json::from_slice(&std::fs::read(&path)?)
        .with_context(|| format!("Cannot read saved drafts at {}", path.display()))?;
    if saved.id != pr.id || saved.head_sha != pr.head_sha || saved.base_sha != pr.base_sha {
        bail!("Saved drafts belong to a different PR revision");
    }
    Ok(saved.comments)
}

pub fn save(root: &Path, pr: &PullRequest, comments: &[DraftComment]) -> Result<()> {
    let path = draft_path(root, pr);
    let parent = path.parent().context("Draft path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let saved = DraftFile {
        id: pr.id.clone(),
        head_sha: pr.head_sha.clone(),
        base_sha: pr.base_sha.clone(),
        comments: comments.to_vec(),
    };
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, &saved)?;
    temp.as_file().sync_all()?;
    temp.persist(&path)
        .with_context(|| format!("Cannot save drafts to {}", path.display()))?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubmissionAttempt {
    pub head_sha: String,
    pub base_sha: String,
    pub body: String,
    pub event: String,
    pub comments: Vec<DraftComment>,
}

pub fn attempt_path(root: &Path, pr: &PullRequest) -> PathBuf {
    root.join("submissions")
        .join(&pr.id.repo)
        .join(format!("{}.json", pr.id.number))
}

pub fn attempt(root: &Path, pr: &PullRequest) -> Result<Option<SubmissionAttempt>> {
    let path = attempt_path(root, pr);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("Cannot read submission attempt {}", path.display())
        })?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn begin_submission(
    root: &Path,
    pr: &PullRequest,
    drafts: &[DraftComment],
    body: &str,
    event: &str,
) -> Result<()> {
    let path = attempt_path(root, pr);
    let parent = path.parent().context("Submission path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(
        &mut temp,
        &SubmissionAttempt {
            head_sha: pr.head_sha.clone(),
            base_sha: pr.base_sha.clone(),
            body: body.into(),
            event: event.into(),
            comments: drafts.to_vec(),
        },
    )?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(&path).with_context(|| {
        format!(
            "An unresolved submission exists at {}; press u to recover it",
            path.display()
        )
    })?;
    Ok(())
}

pub fn clear_attempt(root: &Path, pr: &PullRequest) -> Result<()> {
    match std::fs::remove_file(attempt_path(root, pr)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// A conversation comment is not tied to a revision, so its draft survives new pushes to the PR.
fn conversation_path(root: &Path, id: &PrId) -> PathBuf {
    root.join("drafts")
        .join(&id.repo)
        .join(format!("{}-conversation.md", id.number))
}

/// A reply belongs to its thread, which outlives new pushes, so the draft is keyed by the thread's first comment.
fn reply_path(root: &Path, id: &PrId, comment: u64) -> PathBuf {
    root.join("drafts")
        .join(&id.repo)
        .join(format!("{}-reply-{comment}.md", id.number))
}

pub fn conversation_draft(root: &Path, id: &PrId) -> Result<String> {
    read_text_draft(&conversation_path(root, id))
        .context("Cannot read the conversation comment draft")
}

/// Saves the draft, or removes it when the text is blank.
pub fn save_conversation_draft(root: &Path, id: &PrId, text: &str) -> Result<()> {
    save_text_draft(&conversation_path(root, id), text)
}

pub fn reply_draft(root: &Path, id: &PrId, comment: u64) -> Result<String> {
    read_text_draft(&reply_path(root, id, comment)).context("Cannot read the reply draft")
}

/// Saves the draft, or removes it when the text is blank.
pub fn save_reply_draft(root: &Path, id: &PrId, comment: u64, text: &str) -> Result<()> {
    save_text_draft(&reply_path(root, id, comment), text)
}

fn read_text_draft(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn save_text_draft(path: &Path, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        };
    }
    let parent = path.parent().context("Draft path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut temp, text.as_bytes())?;
    temp.persist(path)
        .with_context(|| format!("Cannot save the draft to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use prpr::model::Side;

    fn pr(sha: &str) -> PullRequest {
        PullRequest {
            id: PrId {
                repo: "owner/repo".into(),
                number: 12,
            },
            title: String::new(),
            body: String::new(),
            url: String::new(),
            author: String::new(),
            head_sha: sha.into(),
            base_sha: "base".into(),
            draft: false,
        }
    }

    #[test]
    fn drafts_survive_restart_and_are_isolated_by_revision() {
        let dir = tempfile::tempdir().unwrap();
        let comment = DraftComment {
            start_line: None,
            start_side: None,
            path: "a.rs".into(),
            line: 5,
            side: Side::Right,
            body: "Why?".into(),
        };
        save(dir.path(), &pr("abc"), std::slice::from_ref(&comment)).unwrap();
        assert_eq!(load(dir.path(), &pr("abc")).unwrap(), vec![comment]);
        assert!(load(dir.path(), &pr("def")).unwrap().is_empty());
        save(dir.path(), &pr("abc"), &[]).unwrap();
        assert!(load(dir.path(), &pr("abc")).unwrap().is_empty());
    }

    #[test]
    fn legacy_saved_drafts_load_and_multiline_ranges_survive_restart() {
        let directory = tempfile::tempdir().unwrap();
        let pull_request = pr("abc");
        let path = draft_path(directory.path(), &pull_request);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "id": {"repo": "owner/repo", "number": 12},
                "head_sha": "abc", "base_sha": "base",
                "comments": [{"path": "a.rs", "line": 5, "side": "RIGHT", "body": "Legacy comment"}]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut draft = load(directory.path(), &pull_request).unwrap().remove(0);
        assert!(draft.start_line.is_none());
        assert!(draft.start_side.is_none());
        draft.start_line = Some(3);
        draft.start_side = Some(Side::Right);
        save(
            directory.path(),
            &pull_request,
            std::slice::from_ref(&draft),
        )
        .unwrap();
        assert_eq!(load(directory.path(), &pull_request).unwrap(), vec![draft]);
    }
}
