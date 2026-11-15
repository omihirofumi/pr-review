use crate::{local, model::*};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Note {
    pub id: String,
    pub anchor: DraftComment,
    pub author: String,
    pub reply_to: Option<String>,
    pub file_revision: String,
    #[serde(default)]
    pub outdated: bool,
}

pub fn file_revision(data: &ReviewData, path: &str) -> Result<String> {
    let file = data
        .files
        .iter()
        .find(|file| file.filename == path)
        .context("File is outside this review")?;
    Ok(local::digest(&serde_json::to_vec(&(
        file,
        &data.pr.base_sha,
    ))?))
}

fn path(root: &Path, data: &ReviewData) -> PathBuf {
    let identity = match &data.source {
        ReviewSource::GitHub => format!("github:{}#{}", data.pr.id.repo, data.pr.id.number),
        ReviewSource::Local { repository, .. } => format!("local:{}", repository.display()),
    };
    root.join("notes")
        .join(format!("{}.json", local::digest(identity.as_bytes())))
}

pub fn load(root: &Path, data: &ReviewData) -> Result<Vec<Note>> {
    let mut notes: Vec<Note> = match std::fs::read(path(root, data)) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    for note in &mut notes {
        note.outdated = file_revision(data, &note.anchor.path)
            .map_or(true, |revision| revision != note.file_revision);
    }
    Ok(notes)
}

pub fn save(root: &Path, data: &ReviewData, notes: &[Note]) -> Result<()> {
    let path = path(root, data);
    let directory = path.parent().unwrap();
    std::fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(&serde_json::to_vec_pretty(notes)?)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub fn update(
    root: &Path,
    data: &ReviewData,
    expected: &[Note],
    replacement: &[Note],
) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = path(root, data);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    ensure!(
        load(root, data)? == expected,
        "Notes changed in another viewer; press r to refresh before saving"
    );
    save(root, data, replacement)
}

pub fn add(
    notes: &mut Vec<Note>,
    data: &ReviewData,
    mut anchor: DraftComment,
    reply_to: Option<String>,
    author: String,
) -> Result<String> {
    ensure!(
        !anchor.body.trim().is_empty() && anchor.body.len() <= 64 * 1024,
        "A note needs 1–65536 bytes of text"
    );
    ensure!(
        !author.trim().is_empty() && author.len() <= 100,
        "Invalid note author"
    );
    if let Some(parent) = &reply_to {
        let parent = notes
            .iter()
            .find(|note| note.id == *parent)
            .context("Unknown parent note")?;
        ensure!(
            !parent.outdated,
            "This note belongs to an outdated file; refresh and inspect the current diff first"
        );
        let body = anchor.body;
        anchor = parent.anchor.clone();
        anchor.body = body;
    }
    let file_revision = file_revision(data, &anchor.path)?;
    let id = format!(
        "note-{}",
        local::digest(
            format!(
                "{}:{}:{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos(),
                notes.len()
            )
            .as_bytes()
        )
    );
    notes.push(Note {
        id: id.clone(),
        anchor,
        author,
        reply_to,
        file_revision,
        outdated: false,
    });
    Ok(id)
}

pub fn comments(notes: &[Note]) -> Vec<ReviewComment> {
    let number =
        |id: &str| u64::from_str_radix(&local::digest(id.as_bytes()), 16).unwrap() | (1 << 63);
    notes
        .iter()
        .map(|note| ReviewComment {
            id: number(&note.id),
            path: note.anchor.path.clone(),
            body: note.anchor.body.clone(),
            author: format!("{} · local", note.author),
            line: (!note.outdated).then_some(note.anchor.line),
            original_line: Some(note.anchor.line),
            side: if note.anchor.side == Side::Left {
                "LEFT"
            } else {
                "RIGHT"
            }
            .into(),
            start_line: note.anchor.start_line,
            start_side: note
                .anchor
                .start_side
                .as_ref()
                .map(|side| if *side == Side::Left { "LEFT" } else { "RIGHT" }.into()),
            in_reply_to_id: note.reply_to.as_deref().map(number),
        })
        .collect()
}
