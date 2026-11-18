use crate::app::App;
use anyhow::{Context, Result, bail, ensure};
use clap::{Args as ClapArgs, Subcommand};
use prpr::{diff::LineKind, model::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    command: SessionCommand,
}

#[derive(Subcommand)]
enum SessionCommand {
    List {
        #[arg(long)]
        json: bool,
    },
    Get(Selector),
    Context(Selector),
    Review(ReviewArgs),
    Comment {
        #[command(subcommand)]
        command: CommentCommand,
    },
    Navigate(NavigateArgs),
    /// Reload the review as `r` does, picking up new pushes, comments, and local edits
    Reload(Selector),
    /// Highlight source ranges for the reviewer; marks last until the review reloads
    Mark {
        #[command(subcommand)]
        command: MarkCommand,
    },
}

#[derive(Subcommand)]
enum MarkCommand {
    Add(MarkArgs),
    Clear(MarkClearArgs),
}

#[derive(ClapArgs)]
struct MarkArgs {
    #[command(flatten)]
    selector: Selector,
    #[arg(long)]
    file: String,
    #[arg(
        long,
        conflicts_with = "new_line",
        required_unless_present = "new_line"
    )]
    old_line: Option<u32>,
    #[arg(long, conflicts_with = "old_line")]
    new_line: Option<u32>,
    #[arg(long, help = "Last line of a multi-line mark on the same side")]
    end_line: Option<u32>,
    #[arg(
        long,
        requires = "end_column",
        help = "First highlighted character, 1-based"
    )]
    start_column: Option<u32>,
    #[arg(
        long,
        requires = "start_column",
        help = "Last highlighted character, 1-based and inclusive"
    )]
    end_column: Option<u32>,
}

#[derive(ClapArgs)]
struct MarkClearArgs {
    #[command(flatten)]
    selector: Selector,
    #[arg(long, help = "Only clear marks in this file")]
    file: Option<String>,
}

#[derive(Clone, ClapArgs)]
struct Selector {
    #[arg(
        long,
        help = "Exact session ID; required when a repository has several sessions"
    )]
    session: Option<String>,
    #[arg(long, help = "Select a session by repository path")]
    repo: Option<PathBuf>,
    #[arg(long, help = "Machine-readable JSON (also the default)")]
    json: bool,
}

#[derive(ClapArgs)]
struct ReviewArgs {
    #[command(flatten)]
    selector: Selector,
    #[arg(long)]
    include_patch: bool,
    #[arg(long)]
    file: Option<String>,
}

#[derive(Subcommand)]
enum CommentCommand {
    List(Selector),
    Add(CommentArgs),
}

#[derive(ClapArgs)]
struct CommentArgs {
    #[command(flatten)]
    selector: Selector,
    #[arg(long, conflicts_with_all = ["file", "old_line", "new_line", "start_line"])]
    reply_to: Option<String>,
    #[arg(long)]
    file: Option<String>,
    #[arg(long, conflicts_with = "new_line")]
    old_line: Option<u32>,
    #[arg(long, conflicts_with = "old_line")]
    new_line: Option<u32>,
    #[arg(long, help = "First line of a range on the selected side")]
    start_line: Option<u32>,
    #[arg(long)]
    body: String,
    #[arg(
        long,
        help = "Reject the note if the loaded revision changed since inspection"
    )]
    revision: Option<String>,
    #[arg(long, default_value = "agent")]
    author: String,
    #[arg(long)]
    focus: bool,
}

#[derive(ClapArgs)]
struct NavigateArgs {
    #[command(flatten)]
    selector: Selector,
    #[arg(long)]
    file: String,
    #[arg(
        long,
        conflicts_with = "new_line",
        required_unless_present = "new_line"
    )]
    old_line: Option<u32>,
    #[arg(long, conflicts_with = "old_line")]
    new_line: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
enum Request {
    Get,
    Context,
    Review {
        include_patch: bool,
        file: Option<String>,
    },
    Comments,
    Comment {
        #[serde(default)]
        revision: Option<String>,
        anchor: Option<DraftComment>,
        reply_to: Option<String>,
        body: String,
        author: String,
        focus: bool,
    },
    Navigate {
        file: String,
        side: Side,
        line: u32,
    },
    Reload,
    Mark {
        mark: Mark,
    },
    ClearMarks {
        file: Option<String>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
struct Manifest {
    id: String,
    repository: PathBuf,
    socket: PathBuf,
    pid: u32,
}

pub struct Server {
    listener: UnixListener,
    manifest: PathBuf,
    identity: Manifest,
    _directory: tempfile::TempDir,
}

impl Server {
    pub fn start(root: &Path, repository: PathBuf) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("prpr-")
            .tempdir_in("/tmp")?;
        let socket = directory.path().join("session.sock");
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let id = directory
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let identity = Manifest {
            id: id.clone(),
            repository,
            socket,
            pid: std::process::id(),
        };
        let registry = root.join("sessions");
        std::fs::create_dir_all(&registry)?;
        std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o700))?;
        let manifest = registry.join(format!("{id}.json"));
        let mut file = tempfile::NamedTempFile::new_in(&registry)?;
        serde_json::to_writer(&mut file, &identity)?;
        file.persist(&manifest)?;
        Ok(Self {
            listener,
            manifest,
            identity,
            _directory: directory,
        })
    }

    pub fn poll(&self, app: &mut App) {
        for _ in 0..4 {
            let Ok((mut stream, _)) = self.listener.accept() else {
                break;
            };
            let result = (|| -> Result<Value> {
                stream.set_read_timeout(Some(Duration::from_millis(100)))?;
                stream.set_write_timeout(Some(Duration::from_millis(100)))?;
                let mut line = String::new();
                BufReader::new((&stream).take(1024 * 1024)).read_line(&mut line)?;
                ensure!(
                    line.ends_with('\n'),
                    "Incomplete or oversized session request"
                );
                let request = serde_json::from_str(&line)?;
                handle(app, &self.identity, request)
            })();
            let value = match result {
                Ok(value) => json!({"ok": true, "result": value}),
                Err(error) => json!({"ok": false, "error": format!("{error:#}")}),
            };
            let _ = writeln!(stream, "{value}");
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.manifest);
    }
}

fn handle(app: &mut App, identity: &Manifest, request: Request) -> Result<Value> {
    match request {
        Request::Get => Ok(
            json!({"session": identity, "source": app.review.as_ref().map(|review| &review.data.source), "title": app.review.as_ref().map(|review| &review.data.pr.title), "loading": app.busy}),
        ),
        Request::Context => {
            let review = app.review.as_ref().context("No diff is loaded yet")?;
            let row = review.current_lines().get(review.position());
            Ok(
                json!({"session": identity.id, "revision": review.data.pr.head_sha, "file": review.current_file().map(|file| &file.filename), "old_line": row.and_then(|row| row.old_line), "new_line": row.and_then(|row| row.new_line), "selection": review.selected_range(), "focus": review.question_focus().ok()}),
            )
        }
        Request::Review {
            include_patch,
            file: only_file,
        } => {
            let review = app.review.as_ref().context("No diff is loaded yet")?;
            if let Some(path) = &only_file {
                ensure!(
                    review.data.files.iter().any(|file| file.filename == *path),
                    "File is outside this review"
                );
            }
            let files = review.data.files.iter().enumerate().filter(|(_, file)| only_file.as_ref().is_none_or(|path| *path == file.filename)).map(|(index, file)| {
                let hunks = review.lines[index].iter().filter(|row| row.kind == LineKind::Hunk).map(|row| row.text.as_str()).collect::<Vec<_>>();
                let mut value = json!({"path": file.filename, "previous_path": file.previous_filename, "status": file.status, "hunks": hunks});
                if include_patch { value["patch"] = json!(review.contexts[index].view.as_ref().map(|view| view.patch.as_str()).or(file.patch.as_deref())); }
                value
            }).collect::<Vec<_>>();
            Ok(
                json!({"session": identity.id, "source": review.data.source, "revision": review.data.pr.head_sha, "title": review.data.pr.title, "description": review.data.pr.body, "files": files, "notes": review.notes, "marks": review.marks}),
            )
        }
        Request::Comments => {
            let review = app.review.as_ref().context("No diff is loaded yet")?;
            Ok(
                json!({"session": identity.id, "revision": review.data.pr.head_sha, "notes": review.notes, "github_comments": review.data.comments, "github_drafts": review.drafts}),
            )
        }
        Request::Comment {
            revision,
            anchor,
            reply_to,
            body,
            author,
            focus,
        } => {
            ensure!(!app.busy, "Review is loading; retry when it finishes");
            if let Some(revision) = revision {
                ensure!(
                    app.review
                        .as_ref()
                        .is_some_and(|review| review.data.pr.head_sha == revision),
                    "The review changed since inspection; read it again before commenting"
                );
            }
            let mut anchor = if let Some(id) = &reply_to {
                let review = app.review.as_ref().context("No diff is loaded")?;
                review
                    .notes
                    .iter()
                    .find(|note| note.id == *id)
                    .context("Unknown parent note")?
                    .anchor
                    .clone()
            } else {
                anchor.context("Supply --reply-to or --file with --old-line/--new-line")?
            };
            anchor.body = body;
            let id = app.add_note(anchor.clone(), reply_to, author)?;
            if focus {
                let _ = app.navigate(&anchor.path, &anchor.side, anchor.line);
            }
            app.message(
                format!("Agent note received: {id}. N shows notes; t toggles inline notes."),
                false,
            );
            Ok(json!({"id": id}))
        }
        Request::Navigate { file, side, line } => {
            ensure!(
                app.dialog.is_none(),
                "A dialog is open; close it before navigating"
            );
            app.navigate(&file, &side, line)?;
            Ok(json!({"file": file, "side": side, "line": line}))
        }
        Request::Reload => {
            ensure!(!app.busy, "Review is loading; retry when it finishes");
            let action = if app.local_spec.is_some() {
                crate::app::Action::RefreshLocal
            } else {
                let review = app.review.as_ref().context("No diff is loaded yet")?;
                crate::app::Action::LoadPr(review.data.pr.id.clone())
            };
            app.open_on_diff = app.tab == crate::app::PageTab::Diff;
            app.requested = Some(action);
            app.message("Agent requested a reload.", false);
            Ok(json!({"reloading": true}))
        }
        Request::Mark { mark } => {
            let review = app.review.as_mut().context("No diff is loaded yet")?;
            validate_mark(review, &mark)?;
            review.marks.push(mark.clone());
            app.message(
                format!(
                    "Agent highlighted {}:{}{}.",
                    mark.path,
                    mark.line,
                    if mark.end_line > mark.line {
                        format!("-{}", mark.end_line)
                    } else {
                        String::new()
                    }
                ),
                false,
            );
            Ok(json!({"marks": review_marks(app)}))
        }
        Request::ClearMarks { file } => {
            let review = app.review.as_mut().context("No diff is loaded yet")?;
            let before = review.marks.len();
            review
                .marks
                .retain(|mark| file.as_ref().is_some_and(|path| *path != mark.path));
            Ok(json!({"cleared": before - review.marks.len()}))
        }
    }
}

fn review_marks(app: &App) -> &[Mark] {
    app.review.as_ref().map_or(&[], |review| &review.marks)
}

fn validate_mark(review: &crate::app::ReviewState, mark: &Mark) -> Result<()> {
    let index = review
        .data
        .files
        .iter()
        .position(|file| file.filename == mark.path)
        .context("File is outside this review")?;
    ensure!(
        mark.line > 0 && mark.end_line >= mark.line,
        "--end-line must not precede the first line"
    );
    for line in mark.line..=mark.end_line {
        ensure!(
            review.lines[index]
                .iter()
                .any(|row| prpr::thread::matches_anchor(row, &mark.side, line)),
            "Line {line} is outside the displayed diff; expand it first"
        );
    }
    if let (Some(start), Some(end)) = (mark.start_column, mark.end_column) {
        ensure!(
            mark.line == mark.end_line,
            "Columns apply to a single line; omit --end-line"
        );
        let text = review.lines[index]
            .iter()
            .find(|row| prpr::thread::matches_anchor(row, &mark.side, mark.line))
            .map(|row| row.text.get(1..).unwrap_or_default().chars().count())
            .unwrap_or_default();
        ensure!(
            start >= 1 && start <= end && end as usize <= text,
            "Columns must satisfy 1 <= start <= end <= {text}"
        );
    }
    Ok(())
}

fn manifests(root: &Path) -> Result<Vec<Manifest>> {
    let directory = root.join("sessions");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_none_or(|extension| extension != "json")
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else {
            continue;
        };
        if UnixStream::connect(&manifest.socket).is_ok() {
            sessions.push(manifest);
        }
    }
    sessions.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(sessions)
}

fn select(root: &Path, selector: &Selector) -> Result<Manifest> {
    let repo = selector
        .repo
        .as_ref()
        .map(|path| {
            prpr::local::repository(path)
                .or_else(|_| path.canonicalize().map_err(anyhow::Error::from))
        })
        .transpose()?;
    let mut matching = manifests(root)?
        .into_iter()
        .filter(|session| {
            selector.session.as_ref().is_none_or(|id| *id == session.id)
                && repo.as_ref().is_none_or(|repo| *repo == session.repository)
        })
        .collect::<Vec<_>>();
    ensure!(
        !matching.is_empty(),
        "No active prpr session. Open prpr or jj diff --tool prpr in your terminal first."
    );
    ensure!(
        matching.len() == 1,
        "Multiple sessions match; use --session with an ID from prpr session list"
    );
    Ok(matching.remove(0))
}

fn call(root: &Path, selector: &Selector, request: Request) -> Result<Value> {
    let manifest = select(root, selector)?;
    let mut stream =
        UnixStream::connect(&manifest.socket).context("Session is no longer running")?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    writeln!(stream, "{}", serde_json::to_string(&request)?)?;
    let mut response = String::new();
    BufReader::new(stream.take(16 * 1024 * 1024))
        .read_line(&mut response)
        .context(
            "Session did not respond; close the editor or wait for the current operation to finish",
        )?;
    let response: Value = serde_json::from_str(&response).context("Invalid session response")?;
    if response["ok"] != true {
        bail!(
            "{}",
            response["error"]
                .as_str()
                .unwrap_or("Session request failed")
        );
    }
    Ok(response["result"].clone())
}

pub fn run(args: &Args, root: &Path) -> Result<()> {
    let value = match &args.command {
        SessionCommand::List { .. } => json!(manifests(root)?),
        SessionCommand::Get(selector) => call(root, selector, Request::Get)?,
        SessionCommand::Context(selector) => call(root, selector, Request::Context)?,
        SessionCommand::Review(args) => call(
            root,
            &args.selector,
            Request::Review {
                include_patch: args.include_patch,
                file: args.file.clone(),
            },
        )?,
        SessionCommand::Comment {
            command: CommentCommand::List(selector),
        } => call(root, selector, Request::Comments)?,
        SessionCommand::Comment {
            command: CommentCommand::Add(args),
        } => {
            let anchor = if args.reply_to.is_none() {
                ensure!(
                    args.old_line.is_some() != args.new_line.is_some(),
                    "Specify exactly one of --old-line or --new-line"
                );
                let side = if args.old_line.is_some() {
                    Side::Left
                } else {
                    Side::Right
                };
                Some(DraftComment {
                    path: args
                        .file
                        .clone()
                        .context("--file is required for a new note")?,
                    line: args.old_line.or(args.new_line).unwrap(),
                    side: side.clone(),
                    start_line: args.start_line,
                    start_side: args.start_line.map(|_| side),
                    body: String::new(),
                })
            } else {
                None
            };
            call(
                root,
                &args.selector,
                Request::Comment {
                    revision: args.revision.clone(),
                    anchor,
                    reply_to: args.reply_to.clone(),
                    body: args.body.clone(),
                    author: args.author.clone(),
                    focus: args.focus,
                },
            )?
        }
        SessionCommand::Reload(selector) => call(root, selector, Request::Reload)?,
        SessionCommand::Mark {
            command: MarkCommand::Add(args),
        } => {
            let line = args.old_line.or(args.new_line).unwrap();
            call(
                root,
                &args.selector,
                Request::Mark {
                    mark: Mark {
                        path: args.file.clone(),
                        side: if args.old_line.is_some() {
                            Side::Left
                        } else {
                            Side::Right
                        },
                        line,
                        end_line: args.end_line.unwrap_or(line),
                        start_column: args.start_column,
                        end_column: args.end_column,
                    },
                },
            )?
        }
        SessionCommand::Mark {
            command: MarkCommand::Clear(args),
        } => call(
            root,
            &args.selector,
            Request::ClearMarks {
                file: args.file.clone(),
            },
        )?,
        SessionCommand::Navigate(args) => call(
            root,
            &args.selector,
            Request::Navigate {
                file: args.file.clone(),
                side: if args.old_line.is_some() {
                    Side::Left
                } else {
                    Side::Right
                },
                line: args.old_line.or(args.new_line).unwrap(),
            },
        )?,
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn marks_validate_visible_lines_and_columns_and_reload_queues_a_refresh() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let mut app = App::new(root.clone(), false);
        let mut data = crate::app::demo_data();
        data.source = ReviewSource::Local {
            repository: root.clone(),
            label: "local".into(),
        };
        app.loaded(data).unwrap();
        app.local_spec = Some(prpr::local::Spec::Directories {
            repository: root.clone(),
            left: root.clone(),
            right: root.clone(),
        });
        let review = app.review.as_ref().unwrap();
        let path = review.current_file().unwrap().filename.clone();
        let row = review
            .current_lines()
            .iter()
            .find(|row| row.new_line.is_some() && row.text.len() > 3)
            .unwrap();
        let line = row.new_line.unwrap();
        let identity = Manifest {
            id: "test".into(),
            repository: root.clone(),
            socket: root.join("socket"),
            pid: 0,
        };
        let mark = |line, end_line, columns: Option<(u32, u32)>| Request::Mark {
            mark: Mark {
                path: path.clone(),
                side: Side::Right,
                line,
                end_line,
                start_column: columns.map(|columns| columns.0),
                end_column: columns.map(|columns| columns.1),
            },
        };
        handle(&mut app, &identity, mark(line, line, Some((1, 2)))).unwrap();
        assert!(handle(&mut app, &identity, mark(99_999, 99_999, None)).is_err());
        assert!(handle(&mut app, &identity, mark(line, line, Some((2, 1)))).is_err());
        assert!(handle(&mut app, &identity, mark(line, line, Some((1, 10_000)))).is_err());
        assert_eq!(app.review.as_ref().unwrap().marks.len(), 1);
        let cleared = handle(&mut app, &identity, Request::ClearMarks { file: None }).unwrap();
        assert_eq!(cleared["cleared"], 1);
        handle(&mut app, &identity, Request::Reload).unwrap();
        assert!(matches!(
            app.requested,
            Some(crate::app::Action::RefreshLocal)
        ));
    }

    #[test]
    fn live_client_reads_user_notes_replies_validates_anchors_and_cleans_up() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let mut app = App::new(root.clone(), false);
        let mut data = crate::app::demo_data();
        data.source = ReviewSource::Local {
            repository: root.clone(),
            label: "local".into(),
        };
        app.loaded(data).unwrap();
        let review = app.review.as_ref().unwrap();
        let row = review
            .current_lines()
            .iter()
            .find(|row| row.new_line.is_some())
            .unwrap();
        let path = review.current_file().unwrap().filename.clone();
        let line = row.new_line.unwrap();
        let anchor = DraftComment {
            path: path.clone(),
            side: Side::Right,
            line,
            start_line: None,
            start_side: None,
            body: "Why change this?".into(),
        };
        let parent = app.add_note(anchor, None, "user".into()).unwrap();
        let server = Server::start(&root, root.clone()).unwrap();
        let selector = Selector {
            session: Some(server.identity.id.clone()),
            repo: None,
            json: true,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                server.poll(&mut app);
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let result = call(
            &root,
            &selector,
            Request::Review {
                include_patch: false,
                file: None,
            },
        )
        .unwrap();
        assert!(result["files"][0].get("patch").is_none());
        let filtered = call(
            &root,
            &selector,
            Request::Review {
                include_patch: true,
                file: Some(path.clone()),
            },
        )
        .unwrap();
        assert_eq!(filtered["files"].as_array().unwrap().len(), 1);
        assert!(filtered["files"][0]["patch"].is_string());
        assert!(
            call(
                &root,
                &selector,
                Request::Review {
                    include_patch: true,
                    file: Some("missing".into())
                }
            )
            .is_err()
        );
        assert!(
            call(
                &root,
                &selector,
                Request::Comment {
                    revision: Some("stale-revision".into()),
                    anchor: None,
                    reply_to: Some(parent.clone()),
                    body: "stale".into(),
                    author: "agent".into(),
                    focus: false
                }
            )
            .is_err()
        );
        let result = call(&root, &selector, Request::Comments).unwrap();
        assert_eq!(result["notes"][0]["author"], "user");
        assert_eq!(result["notes"][0]["id"], parent);
        let reply = call(
            &root,
            &selector,
            Request::Comment {
                revision: None,
                anchor: None,
                reply_to: Some(parent),
                body: "Because the deadline must be bounded.".into(),
                author: "codex".into(),
                focus: false,
            },
        )
        .unwrap();
        assert!(reply["id"].as_str().unwrap().starts_with("note-"));
        let result = call(&root, &selector, Request::Comments).unwrap();
        assert_eq!(result["notes"].as_array().unwrap().len(), 2);
        let bad = DraftComment {
            path: "not-in-review".into(),
            line: 1,
            side: Side::Right,
            start_line: None,
            start_side: None,
            body: "bad".into(),
        };
        assert!(
            call(
                &root,
                &selector,
                Request::Comment {
                    revision: None,
                    anchor: Some(bad),
                    reply_to: None,
                    body: "bad".into(),
                    author: "agent".into(),
                    focus: false
                }
            )
            .is_err()
        );
        call(
            &root,
            &selector,
            Request::Navigate {
                file: path,
                side: Side::Right,
                line,
            },
        )
        .unwrap();
        let context = call(&root, &selector, Request::Context).unwrap();
        assert_eq!(context["new_line"], line);
        stop.store(true, Ordering::Relaxed);
        thread.join().unwrap();
        assert!(manifests(&root).unwrap().is_empty());
    }
}
