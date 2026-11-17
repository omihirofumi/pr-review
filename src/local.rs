use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    context::FileContext,
    diff::{self, LineKind},
    model::*,
};
use anyhow::{Context, Result, ensure};

const MAX_FILE: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub enum Spec {
    Jj {
        repository: PathBuf,
        revision: Option<String>,
        from: Option<String>,
        to: Option<String>,
    },
    Directories {
        repository: PathBuf,
        left: PathBuf,
        right: PathBuf,
    },
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub data: ReviewData,
    pub contents: BTreeMap<String, FileContext>,
}

impl Spec {
    pub fn repository(&self) -> &Path {
        match self {
            Self::Jj { repository, .. } | Self::Directories { repository, .. } => repository,
        }
    }

    pub fn load(&self) -> Result<Snapshot> {
        match self {
            Self::Jj {
                repository,
                revision,
                from,
                to,
            } => load_jj(
                repository,
                revision.as_deref(),
                from.as_deref(),
                to.as_deref(),
            ),
            Self::Directories {
                repository,
                left,
                right,
            } => load_directories(repository, left, right),
        }
    }
}

fn jj(repository: &Path) -> Command {
    let mut command = Command::new("jj");
    command
        .current_dir(repository)
        .args(["--no-pager", "--color=never"]);
    command
}

fn run(command: &mut Command) -> Result<String> {
    String::from_utf8(crate::workspace::output(command)?).context("Command output is not UTF-8")
}

pub fn repository(path: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(run(jj(path).arg("root"))?.trim()).canonicalize()?)
}

/// Prefers the Jujutsu root so colocated workspaces resolve the same way as `prpr diff`.
pub fn checkout_root(path: &Path) -> Option<PathBuf> {
    repository(path).ok().or_else(|| {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(path)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        PathBuf::from(String::from_utf8(output.stdout).ok()?.trim())
            .canonicalize()
            .ok()
    })
}

fn load_jj(
    cwd: &Path,
    revision: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Snapshot> {
    let root = repository(cwd)?;
    run(jj(&root).args(["log", "--no-graph", "-r", "@", "-T", "commit_id"]))?;
    let operation = run(jj(&root).args(["op", "log", "--no-graph", "--limit", "1", "-T", "id"]))?;
    let operation = operation.trim();
    ensure!(
        !operation.is_empty(),
        "Cannot resolve the Jujutsu operation"
    );
    let mut selector = Vec::new();
    if from.is_some() || to.is_some() {
        selector.extend([
            "--from".to_owned(),
            from.unwrap_or("@").to_owned(),
            "--to".into(),
            to.unwrap_or("@").into(),
        ]);
    } else {
        selector.extend(["-r".into(), revision.unwrap_or("@").into()]);
    }
    let command = || {
        let mut command = jj(&root);
        command.args(["--at-op", operation, "diff"]).args(&selector);
        command
    };
    let inventory = run(command().args([
        "-T",
        "json(path) ++ \"\\t\" ++ json(status) ++ \"\\t\" ++ json(self.source().path()) ++ \"\\n\"",
    ]))?;
    let entries = inventory
        .lines()
        .map(|line| -> Result<(String, String, String)> {
            let values = line
                .split('\t')
                .map(serde_json::from_str::<String>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            ensure!(values.len() == 3, "Unexpected Jujutsu file inventory");
            Ok((values[0].clone(), values[1].clone(), values[2].clone()))
        })
        .collect::<Result<Vec<_>>>()?;
    let raw = run(command().args(["--git", "--context=100001"]))?;
    let sections = raw
        .strip_prefix("diff --git ")
        .map(|raw| raw.split("\ndiff --git ").collect::<Vec<_>>())
        .unwrap_or_default();
    ensure!(
        sections.len() == entries.len(),
        "Jujutsu returned an inconsistent file inventory"
    );
    let mut contents = BTreeMap::new();
    let mut files = Vec::new();
    for ((path, status, old), section) in entries.into_iter().zip(sections) {
        validate_path(&path)?;
        let patch = section.find("\n@@ ").map(|start| &section[start + 1..]);
        let source = patch.and_then(|patch| reconstruct(patch).ok()).or_else(|| {
            if patch.is_some() || section.contains("Binary file") {
                return None;
            }
            let text = if status == "removed" {
                String::new()
            } else {
                let mut command = jj(&root);
                command.args([
                    "--at-op",
                    operation,
                    "file",
                    "show",
                    "-r",
                    to.or(revision).unwrap_or("@"),
                    "--",
                    &format!("root:{path}"),
                ]);
                run(&mut command).ok()?
            };
            if text.len() > MAX_FILE || text.lines().count() > 100_000 || text.contains('\0') {
                return None;
            }
            Some(FileContext {
                base: if status == "added" {
                    String::new()
                } else {
                    text.clone()
                },
                head: text,
            })
        });
        let mut file = make_file(path.clone(), &status, source.as_ref());
        if old != path {
            file.previous_filename = Some(old);
        }
        if let Some(source) = source {
            contents.insert(path, source);
        }
        files.push(file);
    }
    let label = if from.is_some() || to.is_some() {
        format!("{} → {}", from.unwrap_or("@"), to.unwrap_or("@"))
    } else {
        format!("jj diff -r {}", revision.unwrap_or("@"))
    };
    let description = run(jj(&root).args([
        "--at-op",
        operation,
        "log",
        "--no-graph",
        "-r",
        to.or(revision).unwrap_or("@"),
        "-T",
        "description",
    ]))
    .unwrap_or_default();
    Ok(snapshot(root, label, description, files, contents))
}

fn reconstruct(patch: &str) -> Result<FileContext> {
    let rows = diff::parse_patch(patch);
    ensure!(
        rows.iter().filter(|row| row.kind == LineKind::Hunk).count() == 1,
        "Full source exceeds the supported line count"
    );
    let mut source = FileContext {
        base: String::new(),
        head: String::new(),
    };
    let mut old = 1;
    let mut new = 1;
    let mut previous = (false, false);
    for (row, raw) in rows.into_iter().zip(patch.split_terminator('\n')) {
        if row.text.starts_with("\\ No newline") {
            if previous.0 {
                source.base.pop();
            }
            if previous.1 {
                source.head.pop();
            }
            continue;
        }
        previous = (row.old_line.is_some(), row.new_line.is_some());
        if let Some(number) = row.old_line {
            ensure!(number == old, "Incomplete base source");
            old += 1;
            source.base.push_str(&raw[1..]);
            source.base.push('\n');
        }
        if let Some(number) = row.new_line {
            ensure!(number == new, "Incomplete head source");
            new += 1;
            source.head.push_str(&raw[1..]);
            source.head.push('\n');
        }
    }
    ensure!(
        source.base.len() <= MAX_FILE && source.head.len() <= MAX_FILE,
        "File exceeds 2 MiB"
    );
    ensure!(
        source.base.lines().count() <= 100_000 && source.head.lines().count() <= 100_000,
        "File exceeds 100000 lines"
    );
    ensure!(
        !source.base.contains('\0') && !source.head.contains('\0'),
        "Binary source"
    );
    Ok(source)
}

fn collect(root: &Path, current: &Path, paths: &mut BTreeSet<String>) -> Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(root, &path, paths)?;
        } else {
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("Non-UTF-8 paths are unsupported")?
                .to_owned();
            validate_path(&relative)?;
            paths.insert(relative);
        }
        ensure!(paths.len() <= 20_000, "Diff contains more than 20000 files");
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
enum Content {
    Missing,
    Bytes(Vec<u8>),
    Unavailable,
}

fn read_source(root: &Path, path: &str) -> Result<Content> {
    let relative = Path::new(path);
    if let Some(parent) = relative.parent() {
        let mut directory = root.to_path_buf();
        for part in parent.components() {
            directory.push(part);
            match std::fs::symlink_metadata(&directory) {
                Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                    return Ok(Content::Missing);
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Content::Missing);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    let path = root.join(relative);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(Content::Bytes(
            std::fs::read_link(path)?
                .to_string_lossy()
                .as_bytes()
                .to_vec(),
        )),
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_FILE as u64 => {
            Ok(Content::Bytes(std::fs::read(path)?))
        }
        Ok(metadata) if metadata.is_dir() => Ok(Content::Missing),
        Ok(_) => Ok(Content::Unavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Content::Missing),
        Err(error) => Err(error.into()),
    }
}

fn load_directories(repository: &Path, left: &Path, right: &Path) -> Result<Snapshot> {
    let left = left.canonicalize()?;
    let right = right.canonicalize()?;
    let root = repository.canonicalize()?;
    let mut paths = BTreeSet::new();
    collect(&left, &left, &mut paths)?;
    collect(&right, &right, &mut paths)?;
    let mut contents = BTreeMap::new();
    let mut files = Vec::new();
    for path in paths {
        let base = read_source(&left, &path)?;
        let head = read_source(&right, &path)?;
        if base != Content::Unavailable && head != Content::Unavailable && base == head {
            continue;
        }
        let status = if base == Content::Missing {
            "added"
        } else if head == Content::Missing {
            "removed"
        } else {
            "modified"
        };
        let source = if base == Content::Unavailable || head == Content::Unavailable {
            None
        } else {
            let bytes = |content| match content {
                Content::Bytes(bytes) => bytes,
                Content::Missing | Content::Unavailable => Vec::new(),
            };
            match (
                String::from_utf8(bytes(base)),
                String::from_utf8(bytes(head)),
            ) {
                (Ok(base), Ok(head))
                    if !base.contains('\0')
                        && !head.contains('\0')
                        && base.lines().count() <= 100_000
                        && head.lines().count() <= 100_000 =>
                {
                    Some(FileContext { base, head })
                }
                _ => None,
            }
        };
        files.push(make_file(path.clone(), status, source.as_ref()));
        if let Some(source) = source {
            contents.insert(path, source);
        }
    }
    Ok(snapshot(
        root,
        "jj external diff".into(),
        String::new(),
        files,
        contents,
    ))
}

/// Git-style text of the changes between two directories, printed when no terminal is
/// available so agents and scripts calling `jj diff` still read the diff instead of failing.
pub fn plain_diff(left: &Path, right: &Path) -> Result<String> {
    let snapshot = load_directories(right, left, right)?;
    let mut text = String::new();
    for file in &snapshot.data.files {
        let name = &file.filename;
        text.push_str(&format!("diff --git a/{name} b/{name}\n"));
        let Some(patch) = &file.patch else {
            text.push_str(&format!("Binary files a/{name} and b/{name} differ\n"));
            continue;
        };
        let old = match file.status.as_str() {
            "added" => "/dev/null".to_owned(),
            _ => format!("a/{name}"),
        };
        let new = match file.status.as_str() {
            "removed" => "/dev/null".to_owned(),
            _ => format!("b/{name}"),
        };
        text.push_str(&format!("--- {old}\n+++ {new}\n{patch}"));
        if !patch.ends_with('\n') {
            text.push('\n');
        }
    }
    Ok(text)
}

fn make_file(filename: String, status: &str, source: Option<&FileContext>) -> ChangedFile {
    let patch = source
        .map(|source| {
            similar::TextDiff::from_lines(&source.base, &source.head)
                .unified_diff()
                .context_radius(3)
                .to_string()
        })
        .filter(|patch| !patch.is_empty());
    let rows = diff::parse_patch(patch.as_deref().unwrap_or_default());
    ChangedFile {
        filename,
        previous_filename: None,
        status: status.into(),
        additions: rows
            .iter()
            .filter(|row| row.kind == LineKind::Addition)
            .count() as u64,
        deletions: rows
            .iter()
            .filter(|row| row.kind == LineKind::Deletion)
            .count() as u64,
        patch,
    }
}

fn snapshot(
    repository: PathBuf,
    label: String,
    description: String,
    files: Vec<ChangedFile>,
    contents: BTreeMap<String, FileContext>,
) -> Snapshot {
    let identity = serde_json::to_vec(&(&files, &contents)).unwrap_or_default();
    let revision = digest(&identity);
    let key = digest(repository.as_os_str().as_encoded_bytes());
    Snapshot {
        data: ReviewData {
            source: ReviewSource::Local {
                repository,
                label: label.clone(),
            },
            pr: PullRequest {
                id: PrId {
                    repo: format!("local/{key}"),
                    number: 0,
                },
                title: label,
                body: description,
                url: String::new(),
                author: "local".into(),
                head_sha: revision,
                base_sha: "local-snapshot".into(),
                draft: false,
            },
            files,
            comments: Vec::new(),
        },
        contents,
    }
}

pub fn digest(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub fn validate_path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && Path::new(path)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
        "Expected a relative source path"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn plain_diff_prints_git_style_sections_for_each_change() {
        let directory = tempfile::tempdir().unwrap();
        let (left, right) = (
            directory.path().join("left"),
            directory.path().join("right"),
        );
        for side in [&left, &right] {
            fs::create_dir_all(side.join("src")).unwrap();
        }
        fs::write(left.join("src/lib.rs"), "one\ntwo\n").unwrap();
        fs::write(right.join("src/lib.rs"), "one\nthree\n").unwrap();
        fs::write(left.join("gone.txt"), "bye\n").unwrap();
        fs::write(right.join("new.txt"), "hello\n").unwrap();
        fs::write(left.join("blob.bin"), b"a\0b").unwrap();
        fs::write(right.join("blob.bin"), b"a\0c").unwrap();
        let text = plain_diff(&left, &right).unwrap();
        assert!(text.contains(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n one\n-two\n+three\n"
        ));
        assert!(text.contains("--- a/gone.txt\n+++ /dev/null\n"));
        assert!(text.contains("--- /dev/null\n+++ b/new.txt\n"));
        assert!(text.contains("Binary files a/blob.bin and b/blob.bin differ\n"));
        assert!(plain_diff(&left, &left).unwrap().is_empty());
    }
}
