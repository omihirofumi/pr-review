#[path = "session.rs"]
mod session;
use crate::{app, store, ui};

use std::{
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Args as ClapArgs, Parser, Subcommand};
use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use directories::ProjectDirs;
use prpr::{
    analysis, assistant, context, github, local,
    model::*,
    settings::{self, Provider},
    workspace,
};
use ratatui::{Terminal, backend::CrosstermBackend};

use app::{Action, App};

#[derive(Subcommand)]
enum Mode {
    /// Review local Jujutsu changes
    Diff(DiffArgs),
    /// View two directories supplied by jj diff --tool
    Difftool {
        left: PathBuf,
        right: PathBuf,
        #[arg(long)]
        repo_root: Option<PathBuf>,
    },
    /// Inspect and annotate a running review without opening another TUI
    Session(session::Args),
    /// Install the live review skill for Codex and Claude
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
}

#[derive(Subcommand)]
enum SkillCommand {
    Install {
        #[arg(long, help = "Replace an existing skill file")]
        force: bool,
    },
}

#[derive(ClapArgs)]
struct DiffArgs {
    #[arg(short = 'r', long, conflicts_with_all = ["from", "to"])]
    revision: Option<String>,
    #[arg(long)]
    from: Option<String>,
    #[arg(long)]
    to: Option<String>,
}

#[derive(Parser)]
#[command(
    name = "prpr",
    bin_name = "prpr",
    version,
    about = "Review GitHub PRs and local Jujutsu changes with Helix and AI explanations"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Mode>,
    #[arg(help = "PR URL or number; omit to browse the current repository")]
    target: Option<String>,
    #[arg(
        short = 'R',
        long,
        help = "Repository in owner/name form; defaults to the current directory"
    )]
    repo: Option<String>,
    #[arg(long, help = "GitHub PR search query, e.g. author:@me")]
    search: Option<String>,
    #[arg(
        long,
        global = true,
        help = "Directory for isolated review workspaces and local drafts"
    )]
    workspace_root: Option<PathBuf>,
    #[arg(long, help = "Explore the interface offline with an example PR")]
    demo: bool,
    #[arg(
        long,
        global = true,
        help = "Configuration file; defaults to ~/.config/prpr/config.toml"
    )]
    config: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        value_enum,
        help = "AI explanation provider for this launch"
    )]
    agent: Option<Provider>,
    #[arg(long, global = true, help = "AI model override for this launch")]
    model: Option<String>,
    #[arg(long, help = "Print the configuration path and exit")]
    config_path: bool,
}

impl Args {
    fn resolve(mut self) -> Result<(Self, Option<PrId>)> {
        if self.demo || self.command.is_some() {
            return Ok((self, None));
        }
        let target = self
            .target
            .as_deref()
            .map(|target| github::parse_target(target, self.repo.as_deref()))
            .transpose()?;
        self.repo = Some(match &target {
            Some(id) => id.repo.clone(),
            None => github::resolve_repo(self.repo.as_deref())?,
        });
        Ok((self, target))
    }
}

pub(crate) enum JobResult {
    Local {
        generation: u64,
        result: Result<local::Snapshot>,
    },
    Inbox(Result<Vec<PrSummary>>),
    FileContext {
        generation: u64,
        pr: Box<PullRequest>,
        filename: String,
        result: Result<context::FileContext>,
    },
    Browser {
        generation: u64,
        id: PrId,
        result: Result<()>,
    },
    PreviewContext {
        generation: u64,
        request: u64,
        id: PrId,
        result: Result<(PrOverview, Box<ReviewData>)>,
    },
    PreviewGuide {
        generation: u64,
        request: u64,
        id: PrId,
        revision: String,
        result: Result<Box<analysis::Analysis>>,
    },
    Answer {
        generation: u64,
        request: u64,
        result: Result<assistant::Answer>,
    },
    Overview {
        generation: u64,
        result: Result<PrOverview>,
    },
    Diff {
        generation: u64,
        result: Result<Box<ReviewData>>,
    },
    Comments {
        generation: u64,
        result: Result<Vec<ReviewComment>>,
    },
    Analysis {
        generation: u64,
        request: u64,
        result: Result<Box<analysis::Analysis>>,
    },
    Activity(Result<String>),
    Prepared {
        result: Result<PathBuf>,
        open_editor: Option<Box<app::EditorTarget>>,
    },
    Submitted {
        result: Result<String>,
        body: String,
        event: ReviewEvent,
    },
}

type Tui = Terminal<CrosstermBackend<std::fs::File>>;

fn tty() -> std::io::Result<std::fs::File> {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let open = |path: &std::path::Path| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
    };
    let name = rustix::termios::ttyname(std::io::stderr(), Vec::new())
        .or_else(|_| rustix::termios::ttyname(std::io::stdin(), Vec::new()));
    if let Ok(name) = name {
        let path = std::path::Path::new(OsStr::from_bytes(name.as_bytes()));
        if path != std::path::Path::new("/dev/tty") {
            return open(path);
        }
    }
    // Pagers can redirect every standard stream, and macOS kqueue cannot monitor the /dev/tty alias.
    let output = Command::new("/bin/ps")
        .args(["-o", "tty=", "-p", &std::process::id().to_string()])
        .output()?;
    let name = String::from_utf8(output.stdout).map_err(std::io::Error::other)?;
    let name = name.trim();
    if !output.status.success()
        || name.is_empty()
        || name.contains('?')
        || !std::path::Path::new(name)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Err(std::io::Error::other(
            "Cannot find the controlling terminal; an interactive terminal is required",
        ));
    }
    open(&std::path::Path::new("/dev").join(name))
}

fn data_root(args: &Args) -> Result<PathBuf> {
    let root = args.workspace_root.clone().map(Ok).unwrap_or_else(|| {
        ProjectDirs::from("dev", "prpr", "prpr")
            .map(|dirs| dirs.data_local_dir().to_path_buf())
            .context("Cannot find data directory; pass --workspace-root")
    })?;
    Ok(if root.is_absolute() {
        root
    } else {
        std::env::current_dir()?.join(root)
    })
}

#[derive(Default)]
pub(crate) struct Runtime {
    generation: u64,
    provider: Option<Provider>,
    ask_request: u64,
    ask_cancel: Option<Arc<AtomicBool>>,
    ask_handle: Option<thread::JoinHandle<()>>,
    pending_comments: Option<Vec<ReviewComment>>,
    pending_ai: Option<bool>,
    ai_request: u64,
    ai_cancel: Option<Arc<AtomicBool>>,
    ai_handle: Option<thread::JoinHandle<()>>,
}

impl Runtime {
    fn stop_ask(&mut self) {
        if let Some(cancel) = self.ask_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(handle) = self.ask_handle.take() {
            let _ = handle.join();
        }
        self.ask_request = self.ask_request.wrapping_add(1);
    }

    fn stop_ai(&mut self) {
        if let Some(cancel) = self.ai_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(handle) = self.ai_handle.take() {
            let _ = handle.join();
        }
        self.ai_request = self.ai_request.wrapping_add(1);
    }

    fn sync(&mut self, generation: u64) {
        if generation != self.generation {
            self.stop_ai();
            self.stop_ask();
            self.generation = generation;
            self.pending_comments = None;
            self.pending_ai = None;
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop_ai();
        self.stop_ask();
    }
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        if let Ok(mut output) = tty() {
            let _ = execute!(
                output,
                DisableBracketedPaste,
                LeaveAlternateScreen,
                crossterm::cursor::Show
            );
        }
    }
}

pub fn run() -> Result<()> {
    let args = Args::parse();
    if matches!(&args.command, Some(Mode::Difftool { .. })) {
        use std::{io::IsTerminal, os::unix::process::CommandExt};
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            let error = Command::new(std::env::current_exe()?)
                .args(std::env::args_os().skip(1))
                .stdin(std::process::Stdio::from(tty()?))
                .stdout(std::process::Stdio::from(tty()?))
                .stderr(std::process::Stdio::from(tty()?))
                .exec();
            return Err(error).context("Cannot attach the diff viewer to the terminal");
        }
    }
    if let Some(Mode::Skill {
        command: SkillCommand::Install { force },
    }) = &args.command
    {
        let home = directories::BaseDirs::new().context("Cannot find the home directory")?;
        for relative in [".agents/skills/prpr-review", ".claude/skills/prpr-review"] {
            let directory = home.home_dir().join(relative);
            std::fs::create_dir_all(&directory)?;
            let path = directory.join("SKILL.md");
            let content = include_str!("../skills/prpr-review/SKILL.md");
            if !force && path.exists() && std::fs::read_to_string(&path)? != content {
                bail!(
                    "Existing skill differs at {}; use --force to replace it",
                    path.display()
                );
            }
            std::fs::write(&path, content)?;
            println!("Installed {}", path.display());
        }
        return Ok(());
    }
    let root = data_root(&args)?;
    if let Some(Mode::Session(command)) = &args.command {
        return session::run(command, &root);
    }
    let config_path = args
        .config
        .clone()
        .map(Ok)
        .unwrap_or_else(settings::default_path)?;
    if args.config_path {
        println!("{}", config_path.display());
        return Ok(());
    }
    let mut settings = settings::load(&config_path)?;
    if let Some(provider) = args.agent {
        settings.ai.provider = provider;
    }
    if let Some(model) = &args.model {
        settings.ai.model = Some(model.clone());
    }
    settings.ai.validate()?;
    let (args, target) = args.resolve()?;
    let mut app = App::new(root, args.demo);
    app.repo = args.repo.clone();
    app.settings = settings;
    app.config_path = Some(config_path);
    let cwd = std::env::current_dir()?;
    let origin = match &args.command {
        Some(Mode::Difftool { repo_root, .. }) => repo_root
            .clone()
            .or_else(|| std::env::var_os("PWD").map(PathBuf::from))
            .unwrap_or(cwd.clone()),
        _ => cwd.clone(),
    };
    let repository = local::repository(&origin).unwrap_or(origin.canonicalize()?);
    app.local_spec = match &args.command {
        Some(Mode::Diff(options)) => Some(local::Spec::Jj {
            repository: repository.clone(),
            revision: options.revision.clone(),
            from: options.from.clone(),
            to: options.to.clone(),
        }),
        Some(Mode::Difftool { left, right, .. }) => Some(local::Spec::Directories {
            repository: repository.clone(),
            left: left.clone(),
            right: right.clone(),
        }),
        _ => None,
    };
    let session = session::Server::start(&app.root, repository)?;
    if args.demo {
        let demo = app::demo_data();
        app.repo = Some(demo.pr.id.repo.clone());
        app.inbox = vec![PrSummary {
            body: demo.pr.body.clone(),
            id: demo.pr.id.clone(),
            title: demo.pr.title.clone(),
            author: demo.pr.author.clone(),
            draft: false,
        }];
        app.message(
            "Offline demo. Browse PRs; o opens Diff; a shows a sample guide. Nothing is sent.",
            false,
        );
    }
    let panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        if let Ok(mut output) = tty() {
            let _ = execute!(
                output,
                DisableBracketedPaste,
                LeaveAlternateScreen,
                crossterm::cursor::Show
            );
        }
        panic_hook(info);
    }));
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(tty()?, EnterAlternateScreen, EnableBracketedPaste)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(tty()?))?;
    terminal.resize(terminal.size()?.into())?;
    let (sender, receiver) = mpsc::channel();
    let mut runtime = Runtime::default();
    if app.local_spec.is_some() {
        dispatch(
            Action::RefreshLocal,
            &mut app,
            &args,
            &sender,
            &mut terminal,
            &mut runtime,
        )?;
    } else if let Some(id) = target {
        dispatch(
            Action::LoadPr(id),
            &mut app,
            &args,
            &sender,
            &mut terminal,
            &mut runtime,
        )?;
    } else if !args.demo {
        dispatch(
            Action::LoadInbox,
            &mut app,
            &args,
            &sender,
            &mut terminal,
            &mut runtime,
        )?;
    }
    loop {
        session.poll(&mut app);
        while let Ok(result) = receiver.try_recv() {
            let Some(result) = apply_background(result, &mut app, &sender, &mut runtime) else {
                continue;
            };
            app.busy = false;
            match result {
                JobResult::Inbox(Ok(prs)) => {
                    app.inbox = prs;
                    app.selected_pr = 0;
                    app.message(format!("{} pull requests loaded", app.inbox.len()), false);
                }
                JobResult::Activity(Ok(text)) => {
                    app.dialog = Some(app::Dialog::Activity { text, scroll: 0 })
                }
                JobResult::Prepared {
                    result: Ok(path),
                    open_editor,
                } => {
                    if let Some(review) = &mut app.review {
                        review.workspace = Some(path.clone());
                    }
                    app.message(format!("Review workspace: {}", path.display()), false);
                    if let Some(target) = open_editor {
                        open_editor_action(&mut app, &mut terminal, *target);
                    }
                }
                JobResult::Submitted {
                    result: Ok(url), ..
                } => {
                    if let Some(review) = &mut app.review {
                        if let Err(error) = store::save(&app.root, &review.data.pr, &[]) {
                            review.drafts.clear();
                            app.message(format!("Submitted: {url}. Could not clear saved drafts: {error:#}. Do not resubmit."), true);
                            continue;
                        }
                        review.drafts.clear();
                        if let Err(error) = store::clear_attempt(&app.root, &review.data.pr) {
                            app.message(format!("Submitted: {url}. Could not clear submission record: {error:#}. Press u to mark it sent."), true);
                            continue;
                        }
                    }
                    app.message(
                        format!("Review submitted: {url}. Press r to refresh comments."),
                        false,
                    );
                }
                JobResult::Submitted {
                    result: Err(error),
                    body,
                    event,
                } => {
                    if !github::is_submission_uncertain(&error)
                        && let Some(review) = &app.review
                        && let Err(cleanup) = store::clear_attempt(&app.root, &review.data.pr)
                    {
                        app.message(format!("{error:#}. Could not clear submission record: {cleanup:#}. Press u to recover."), true);
                        continue;
                    }
                    if !github::is_submission_uncertain(&error) {
                        app.dialog = Some(app::Dialog::Submit {
                            event,
                            input: app::TextInput::new(body),
                        });
                    }
                    app.message(format!("{error:#}. Drafts remain saved. V shows published reviews; u recovers an uncertain submission."), true);
                }
                JobResult::Inbox(Err(error))
                | JobResult::Activity(Err(error))
                | JobResult::Prepared {
                    result: Err(error), ..
                } => app.message(format!("{error:#}"), true),
                JobResult::Local { .. }
                | JobResult::Overview { .. }
                | JobResult::Diff { .. }
                | JobResult::Comments { .. }
                | JobResult::Analysis { .. }
                | JobResult::PreviewContext { .. }
                | JobResult::PreviewGuide { .. }
                | JobResult::Answer { .. }
                | JobResult::FileContext { .. }
                | JobResult::Browser { .. } => {}
            }
        }
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match app.key(key) {
                    Ok(Action::Quit) => break,
                    Ok(action) => {
                        if let Err(error) = dispatch(
                            action,
                            &mut app,
                            &args,
                            &sender,
                            &mut terminal,
                            &mut runtime,
                        ) {
                            app.busy = false;
                            app.message(format!("{error:#}"), true);
                        }
                    }
                    Err(error) => app.message(format!("{error:#}"), true),
                },
                Event::Paste(text) => app.paste(&text),
                _ => {}
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_background(
    result: JobResult,
    app: &mut App,
    sender: &mpsc::Sender<JobResult>,
    runtime: &mut Runtime,
) -> Option<JobResult> {
    match result {
        JobResult::Local { generation, result } => {
            if generation != app.generation {
                return None;
            }
            app.busy = false;
            match result {
                Ok(snapshot) => {
                    let previous = app.review.as_ref().map(|review| {
                        (
                            app.tab,
                            review.show_comments,
                            review.current_file().map(|file| file.filename.clone()),
                            review.anchor(),
                        )
                    });
                    let mut metadata = overview_from_review(&snapshot.data);
                    metadata.state = "local".into();
                    app.open_on_diff = true;
                    app.opened(metadata);
                    app.local_contents = snapshot.contents;
                    if let Err(error) = app.accept_diff(snapshot.data) {
                        app.diff_loading = false;
                        app.diff_error = Some(format!("{error:#}"));
                        app.message(format!("Cannot load local review: {error:#}"), true);
                        return None;
                    }
                    if let Some((tab, shown, path, anchor)) = previous {
                        if let (Some(path), Some((side, line))) = (path, anchor) {
                            let _ = app.navigate(&path, &side, line);
                        }
                        if let Some(review) = &mut app.review {
                            review.show_comments = shown;
                        }
                        app.tab = tab;
                    }
                    app.message(
                        "Local review. c note · N notes · Q ask · E explain · a guide · r refresh",
                        false,
                    );
                    if app.settings.ai.auto_explain {
                        start_analysis(app, sender, runtime, false);
                    }
                }
                Err(error) => app.message(format!("Cannot load local diff: {error:#}"), true),
            }
        }
        JobResult::Browser {
            generation,
            id,
            result,
        } => {
            if generation != app.generation {
                return None;
            }
            match result {
                Ok(()) => app.message(
                    format!("Opened {} #{} on GitHub", id.repo, id.number),
                    false,
                ),
                Err(error) => app.message(format!("Could not open GitHub: {error:#}"), true),
            }
        }
        JobResult::FileContext {
            generation,
            pr,
            filename,
            result,
        } => {
            if generation != app.generation {
                return None;
            }
            let Some(review) = &mut app.review else {
                return None;
            };
            if review.data.pr.id != pr.id
                || review.data.pr.head_sha != pr.head_sha
                || review.data.pr.base_sha != pr.base_sha
            {
                return None;
            }
            let index = review
                .data
                .files
                .iter()
                .position(|file| file.filename == filename)?;
            review.contexts[index].loading = false;
            let result = result.and_then(|source| review.accept_context(index, source));
            if let Err(error) = result {
                review.contexts[index].expansion = Default::default();
                app.message(
                    format!("Cannot expand {filename}: {error:#}. + or F retries.",),
                    true,
                );
            } else {
                app.message(format!("Context ready for {filename}. +/- expands/collapses; F toggles the full file."), false);
            }
        }
        JobResult::PreviewContext {
            generation,
            request,
            id,
            result,
        } => {
            if generation != app.generation || request != runtime.ai_request {
                return None;
            }
            match result {
                Ok((overview, review)) => {
                    let config = app.settings.ai.clone();
                    let cache = app.root.join("analysis");
                    let cancel = Arc::new(AtomicBool::new(false));
                    runtime.ai_cancel = Some(cancel.clone());
                    let sender = sender.clone();
                    runtime.ai_handle = Some(thread::spawn(move || {
                        let result =
                            analysis::generate(&overview, &review, &config, &cache, cancel, false)
                                .map(Box::new);
                        let _ = sender.send(JobResult::PreviewGuide {
                            generation,
                            request,
                            id,
                            revision: overview.head_sha,
                            result,
                        });
                    }));
                }
                Err(error) => {
                    app.preview.guides.insert(
                        app::preview_key(&id),
                        app::PreviewGuide {
                            revision: String::new(),
                            status: app::AiStatus::Error(format!("{error:#}")),
                        },
                    );
                }
            }
        }
        JobResult::PreviewGuide {
            generation,
            request,
            id,
            revision,
            result,
        } => {
            if generation != app.generation || request != runtime.ai_request {
                return None;
            }
            app.preview.guides.insert(
                app::preview_key(&id),
                app::PreviewGuide {
                    revision,
                    status: match result {
                        Ok(guide) => app::AiStatus::Ready(guide),
                        Err(error) => app::AiStatus::Error(format!("{error:#}")),
                    },
                },
            );
        }
        JobResult::Answer {
            generation,
            request,
            result,
        } => {
            if generation != app.generation || request != runtime.ask_request {
                return None;
            }
            if let Some((question, scope)) = app.chat.pending.take() {
                match result {
                    Ok(response) => app.chat.turns.push(assistant::Turn {
                        question,
                        scope,
                        response,
                    }),
                    Err(error) => {
                        app.chat.error = Some(format!("{error:#}"));
                        if let Some(app::Dialog::Ask { input, .. }) = &mut app.dialog
                            && input.text.is_empty()
                        {
                            *input = app::TextInput::new(question);
                        }
                    }
                }
            }
            if let Some(app::Dialog::Ask { scroll, .. }) = &mut app.dialog {
                *scroll = u16::MAX;
            }
        }
        JobResult::Overview { generation, result } => {
            if generation != app.generation {
                return None;
            }
            app.busy = false;
            match result {
                Ok(metadata) => {
                    app.opened(metadata.clone());
                    app.comments_loading = true;
                    spawn_diff(sender, generation, metadata.clone());
                    let sender = sender.clone();
                    thread::spawn(move || {
                        let _ = sender.send(JobResult::Comments {
                            generation,
                            result: github::load_comments(&metadata),
                        });
                    });
                }
                Err(error) => app.message(format!("{error:#}"), true),
            }
        }
        JobResult::Diff { generation, result } => {
            if generation != app.generation {
                return None;
            }
            app.diff_loading = false;
            match result {
                Ok(mut data) => {
                    if let Some(comments) = runtime.pending_comments.take() {
                        data.comments = comments;
                    }
                    if let Err(error) = app.accept_diff(*data) {
                        app.diff_error = Some(format!("{error:#}"));
                        return None;
                    }
                    let pending = runtime.pending_ai.take();
                    if (app.settings.ai.auto_explain || pending.is_some())
                        && app.settings.ai.provider != Provider::None
                    {
                        start_analysis(app, sender, runtime, pending.unwrap_or(false));
                    }
                }
                Err(error) => {
                    app.diff_error = Some(format!("{error:#}"));
                    app.message(
                        "The diff could not be loaded. The overview is still available; r retries.",
                        true,
                    );
                }
            }
        }
        JobResult::Comments { generation, result } => {
            if generation != app.generation {
                return None;
            }
            app.comments_loading = false;
            match result {
                Ok(comments) => {
                    if let Some(review) = &mut app.review {
                        review.data.comments = comments;
                    } else {
                        runtime.pending_comments = Some(comments);
                    }
                }
                Err(error) => app.message(
                    format!("Review comments could not be loaded: {error:#}. r refreshes the PR."),
                    true,
                ),
            }
        }
        JobResult::Analysis {
            generation,
            request,
            result,
        } => {
            if generation != app.generation || request != runtime.ai_request {
                return None;
            }
            if let Some(overview) = &mut app.overview {
                overview.ai = match result {
                    Ok(analysis) => app::AiStatus::Ready(analysis),
                    Err(error) => app::AiStatus::Error(format!("{error:#}")),
                };
            }
        }
        foreground => return Some(foreground),
    }
    None
}

fn spawn_diff(sender: &mpsc::Sender<JobResult>, generation: u64, metadata: PrOverview) {
    let sender = sender.clone();
    thread::spawn(move || {
        let _ = sender.send(JobResult::Diff {
            generation,
            result: github::load_diff(&metadata).map(Box::new),
        });
    });
}

fn start_analysis(
    app: &mut App,
    sender: &mpsc::Sender<JobResult>,
    runtime: &mut Runtime,
    force: bool,
) {
    if app.demo {
        if let (Some(overview), Some(review)) = (&mut app.overview, &app.review) {
            overview.ai = app::AiStatus::Ready(sample_guide(&review.data));
        }
        app.message("Offline sample guide. No AI agent was run.", false);
        return;
    }
    if app.settings.ai.provider == Provider::None {
        return;
    }
    let Some(overview) = app.overview.as_ref() else {
        return;
    };
    let Some(review) = app.review.as_ref() else {
        runtime.pending_ai = Some(force);
        return;
    };
    let metadata = overview.metadata.clone();
    let review = review.data.clone();
    let config = app.settings.ai.clone();
    let cache_root = app.root.join("analysis");
    runtime.stop_ai();
    let request = runtime.ai_request;
    let generation = app.generation;
    let cancel = Arc::new(AtomicBool::new(false));
    runtime.ai_cancel = Some(cancel.clone());
    app.overview.as_mut().unwrap().ai = app::AiStatus::Loading;
    let sender = sender.clone();
    runtime.ai_handle = Some(thread::spawn(move || {
        let result = analysis::generate(&metadata, &review, &config, &cache_root, cancel, force)
            .map(Box::new);
        let _ = sender.send(JobResult::Analysis {
            generation,
            request,
            result,
        });
    }));
}

fn sample_context(file: &ChangedFile) -> context::FileContext {
    let rows = prpr::diff::parse_patch(file.patch.as_deref().unwrap_or_default());
    let prefix = rows
        .iter()
        .find_map(|row| row.old_line.or(row.new_line))
        .unwrap_or(1)
        .saturating_sub(1);
    let prefix = (0..prefix)
        .map(|index| format!("// unchanged before hunk {}\n", index + 1))
        .collect::<String>();
    let suffix = if file.status == "modified" {
        (1..=30)
            .map(|index| format!("// unchanged after hunk {index}\n"))
            .collect::<String>()
    } else {
        String::new()
    };
    let source = |left| {
        prefix.clone()
            + &rows
                .iter()
                .filter(|row| {
                    if left {
                        row.old_line.is_some()
                    } else {
                        row.new_line.is_some()
                    }
                })
                .map(|row| format!("{}\n", &row.text[1..]))
                .collect::<String>()
            + &suffix
    };
    context::FileContext {
        base: source(true),
        head: source(false),
    }
}

fn start_context(app: &mut App, sender: &mpsc::Sender<JobResult>, index: usize) -> Result<()> {
    let review = app.review.as_mut().context("Open a diff first")?;
    let file = review
        .data
        .files
        .get(index)
        .context("Choose a file")?
        .clone();
    if review.data.source.is_local() {
        let source = app
            .local_contents
            .get(&file.filename)
            .context("Full text is unavailable for this file")?
            .clone();
        review.accept_context(index, source)?;
        return Ok(());
    }
    if app.demo {
        let source = sample_context(&file);
        review.accept_context(index, source)?;
        return Ok(());
    }
    let pr = review.data.pr.clone();
    let filename = file.filename.clone();
    review.contexts[index].loading = true;
    let generation = app.generation;
    let root = app.root.join("file-context");
    let sender = sender.clone();
    app.message(
        format!("Loading exact base/head context for {filename}…"),
        false,
    );
    thread::spawn(move || {
        let result = github::load_file_context(&pr, &file, &root);
        let _ = sender.send(JobResult::FileContext {
            generation,
            pr: Box::new(pr),
            filename,
            result,
        });
    });
    Ok(())
}

fn sample_guide(data: &ReviewData) -> Box<analysis::Analysis> {
    Box::new(analysis::Analysis {
        summary: "Offline sample: keep **retry waits** within the caller’s `budget`.".into(),
        intent: analysis::Intent { text: "Bound request latency while preserving retries.".into(), basis: "author_description".into() },
        changes: vec![analysis::Change { title: "Request deadline".into(), detail: "Clamp `self.retry_delay` to `budget`, then reject a zero delay before calling `sleep`.\n\n```rust\nlet delay = self.retry_delay.min(budget);\nif delay.is_zero() {\n    return Err(Error::DeadlineExceeded);\n}\nsleep(delay);\n```".into(), files: vec![data.files[0].filename.clone()] }],
        reading_order: vec![analysis::ReadingStep { path: data.files[0].filename.clone(), line: None, side: Side::Right, reason: "Start with the request implementation.".into() }],
        risks: Vec::new(), verification: analysis::Verification { existing_tests: Vec::new(), suggested_checks: vec!["Run the timeout tests.".into()], unknowns: vec!["This is an offline example, not a generated analysis.".into()] },
        coverage: analysis::Coverage { included_files: data.files.iter().map(|file| file.filename.clone()).collect(), ..Default::default() },
    })
}

fn start_preview(app: &mut App, sender: &mpsc::Sender<JobResult>, runtime: &mut Runtime, id: PrId) {
    runtime.stop_ai();
    for guide in app.preview.guides.values_mut() {
        if matches!(guide.status, app::AiStatus::Loading) {
            guide.status = app::AiStatus::Waiting;
        }
    }
    app.preview.shown = true;
    app.preview.tab = app::PreviewTab::Guide;
    app.preview.scroll = 0;
    let key = app::preview_key(&id);
    if app.demo {
        let data = app::demo_data();
        app.preview.guides.insert(
            key,
            app::PreviewGuide {
                revision: data.pr.head_sha.clone(),
                status: app::AiStatus::Ready(sample_guide(&data)),
            },
        );
        return;
    }
    if app.settings.ai.provider == Provider::None {
        return;
    }
    app.preview.guides.insert(
        key,
        app::PreviewGuide {
            revision: String::new(),
            status: app::AiStatus::Loading,
        },
    );
    let generation = app.generation;
    let request = runtime.ai_request;
    let sender = sender.clone();
    thread::spawn(move || {
        let result = github::load_overview(&id).and_then(|overview| {
            github::load_diff(&overview).map(|review| (overview, Box::new(review)))
        });
        let _ = sender.send(JobResult::PreviewContext {
            generation,
            request,
            id,
            result,
        });
    });
}

fn start_question(
    app: &mut App,
    sender: &mpsc::Sender<JobResult>,
    runtime: &mut Runtime,
    question: String,
    focus: Option<Box<assistant::Focus>>,
) -> Result<()> {
    if app.chat.pending.is_some() {
        return Ok(());
    }
    let review = app
        .review
        .as_ref()
        .context("Wait for the diff before asking a question")?
        .ai_data(focus.as_deref());
    let overview = app
        .overview
        .as_ref()
        .map(|state| state.metadata.clone())
        .unwrap_or_else(|| overview_from_review(&review));
    let config = app.settings.ai.clone();
    if config.provider == Provider::None {
        bail!("Choose an agent with A before asking");
    }
    runtime.stop_ask();
    let scope = focus
        .as_ref()
        .map_or_else(|| "Entire changeset".into(), |focus| focus.label.clone());
    app.chat.error = None;
    app.chat.pending = Some((question.clone(), scope));
    let generation = app.generation;
    let request = runtime.ask_request;
    let cancel = Arc::new(AtomicBool::new(false));
    runtime.ask_cancel = Some(cancel.clone());
    let history = app.chat.turns.clone();
    let sender = sender.clone();
    let demo = app.demo;
    runtime.ask_handle = Some(thread::spawn(move || {
        let result = if demo {
            Ok(assistant::Answer { answer: "Offline example: compare the removed and added lines to understand the timeout change. Ask a follow-up below; real PRs use your configured agent.".into(), references: Vec::new(), unknowns: vec!["No AI agent was run in this demo.".into()], coverage: Default::default() })
        } else {
            assistant::answer(
                &overview,
                &review,
                &config,
                &question,
                focus.as_deref(),
                &history,
                cancel,
            )
        };
        let _ = sender.send(JobResult::Answer {
            generation,
            request,
            result,
        });
    }));
    Ok(())
}

fn overview_from_review(data: &ReviewData) -> PrOverview {
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

fn dispatch(
    action: Action,
    app: &mut App,
    args: &Args,
    sender: &mpsc::Sender<JobResult>,
    terminal: &mut Tui,
    runtime: &mut Runtime,
) -> Result<()> {
    if app.local_spec.is_some()
        && matches!(
            &action,
            Action::Browse(_)
                | Action::LoadPr(_)
                | Action::LoadInbox
                | Action::Prepare { .. }
                | Action::Activity(_)
                | Action::Submit { .. }
                | Action::PreviewGuide(_)
        )
    {
        bail!("This action requires a GitHub PR; local notes remain on this Mac");
    }
    runtime.sync(app.generation);
    if runtime.provider != Some(app.settings.ai.provider) {
        runtime.stop_ai();
        runtime.stop_ask();
        runtime.provider = Some(app.settings.ai.provider);
    }
    if matches!(action, Action::LoadInbox) {
        runtime.stop_ai();
    }
    if matches!(action, Action::None | Action::Quit) {
        return Ok(());
    }
    if let Action::OpenEditor(target) = action {
        open_editor_action(app, terminal, *target);
        return Ok(());
    }
    match action {
        Action::RefreshLocal => {
            if app.busy {
                return Ok(());
            }
            let spec = app.local_spec.clone().context("No local source is open")?;
            app.generation = app.generation.wrapping_add(1);
            runtime.sync(app.generation);
            app.chat = Default::default();
            app.busy = true;
            let generation = app.generation;
            let sender = sender.clone();
            app.message("Loading local diff snapshot…", false);
            thread::spawn(move || {
                let _ = sender.send(JobResult::Local {
                    generation,
                    result: spec.load(),
                });
            });
            return Ok(());
        }

        Action::Browse(id) => {
            if app.demo {
                app.message(
                    "Offline demo: B opens the selected real PR on GitHub.",
                    false,
                );
                return Ok(());
            }
            let generation = app.generation;
            let sender = sender.clone();
            thread::spawn(move || {
                let result = github::open_in_browser(&id);
                let _ = sender.send(JobResult::Browser {
                    generation,
                    id,
                    result,
                });
            });
            return Ok(());
        }
        Action::LoadContext(index) => {
            start_context(app, sender, index)?;
            return Ok(());
        }
        Action::PreviewGuide(id) => {
            start_preview(app, sender, runtime, id);
            return Ok(());
        }
        Action::Ask { question, focus } => {
            start_question(app, sender, runtime, question, focus)?;
            return Ok(());
        }
        Action::CancelAsk => {
            runtime.stop_ask();
            return Ok(());
        }
        Action::Explain { force: _ } if app.overview.is_none() => {
            if let Some(pr) = app.visible_prs().get(app.selected_pr) {
                let id = pr.id.clone();
                start_preview(app, sender, runtime, id);
            }
            return Ok(());
        }
        Action::Explain { force } => {
            start_analysis(app, sender, runtime, force);
            return Ok(());
        }
        Action::DisableAi => {
            runtime.stop_ai();
            runtime.stop_ask();
            runtime.pending_ai = None;
            return Ok(());
        }
        Action::LoadDiff if app.local_spec.is_some() => {
            return dispatch(Action::RefreshLocal, app, args, sender, terminal, runtime);
        }
        Action::LoadDiff => {
            if let Some(overview) = &app.overview {
                let id = overview.metadata.id.clone();
                return dispatch(Action::LoadPr(id), app, args, sender, terminal, runtime);
            }
            return Ok(());
        }
        _ => {}
    }
    if app.demo {
        if let Action::LoadPr(_) = action {
            let data = app::demo_data();
            app.opened(overview_from_review(&data));
            app.accept_diff(data)?;
            if let Some(overview) = &mut app.overview {
                overview.ai = app::AiStatus::Ready(sample_guide(&app::demo_data()));
            }
        } else {
            app.message("Demo is offline. Nothing was sent to GitHub.", false);
        }
        return Ok(());
    }
    if app.busy {
        return Ok(());
    }
    app.busy = true;
    let sender = sender.clone();
    match action {
        Action::LoadInbox => {
            app.message("Fetching PR inbox...", false);
            let repo = app.repo.clone();
            let search = args.search.clone();
            thread::spawn(move || {
                let _ = sender.send(JobResult::Inbox(github::list_prs(
                    repo.as_deref(),
                    search.as_deref(),
                )));
            });
        }
        Action::LoadPr(id) => {
            app.generation = app.generation.wrapping_add(1);
            runtime.sync(app.generation);
            app.overview = None;
            app.review = None;
            app.diff_loading = false;
            app.diff_error = None;
            app.comments_loading = false;
            app.goto_pending = false;
            app.tab = app::PageTab::Overview;
            let generation = app.generation;
            app.message(
                format!("Loading the overview for {} #{}...", id.repo, id.number),
                false,
            );
            thread::spawn(move || {
                let _ = sender.send(JobResult::Overview {
                    generation,
                    result: github::load_overview(&id),
                });
            });
        }
        Action::Activity(id) => {
            app.message("Loading published reviews...", false);
            thread::spawn(move || {
                let _ = sender.send(JobResult::Activity(github::reviews_text(&id)));
            });
        }
        Action::Prepare { pr, open_editor } => {
            app.message(
                "Preparing a Jujutsu workspace at the reviewed revision...",
                false,
            );
            let root = app.root.join("workspaces");
            thread::spawn(move || {
                let result = workspace::prepare(&pr, &root);
                let _ = sender.send(JobResult::Prepared {
                    result,
                    open_editor,
                });
            });
        }
        Action::Submit {
            data,
            drafts,
            body,
            event,
        } => {
            if let Err(error) =
                store::begin_submission(&app.root, &data.pr, &drafts, &body, event.as_str())
            {
                app.busy = false;
                app.dialog = Some(app::Dialog::Confirm {
                    event,
                    body,
                    scroll: 0,
                });
                return Err(error);
            }
            app.message(
                "Checking revision and submitting review. Do not retry until the result arrives...",
                false,
            );
            thread::spawn(move || {
                let result = github::submit_review(&data, &drafts, &body, event);
                let _ = sender.send(JobResult::Submitted {
                    result,
                    body,
                    event,
                });
            });
        }
        Action::RefreshLocal
        | Action::None
        | Action::Quit
        | Action::OpenEditor(_)
        | Action::LoadDiff
        | Action::Explain { .. }
        | Action::DisableAi
        | Action::PreviewGuide(_)
        | Action::Ask { .. }
        | Action::CancelAsk
        | Action::Browse(_)
        | Action::LoadContext(_) => {
            app.busy = false;
        }
    }
    Ok(())
}

fn open_editor_action(app: &mut App, terminal: &mut Tui, target: app::EditorTarget) {
    let result = (|| -> Result<()> {
        let review = app.review.as_ref().context("No review is open")?;
        let line = target.line;
        let snapshot_directory;
        let (workspace, path) = if let ReviewSource::Local { repository, .. } = &review.data.source
        {
            local::validate_path(&target.file.filename)?;
            let source = app
                .local_contents
                .get(&target.file.filename)
                .context("Text source is unavailable for this file")?;
            let text = if target.side == Side::Left {
                &source.base
            } else {
                &source.head
            };
            let candidate = repository.join(&target.file.filename);
            let editable = target.side == Side::Right
                && candidate
                    .canonicalize()
                    .is_ok_and(|path| path.starts_with(repository))
                && std::fs::read(&candidate).is_ok_and(|bytes| bytes == text.as_bytes());
            if editable {
                (repository.as_path(), candidate)
            } else {
                snapshot_directory = tempfile::tempdir()?;
                let path = snapshot_directory.path().join(
                    std::path::Path::new(&target.file.filename)
                        .file_name()
                        .context("Invalid filename")?,
                );
                std::fs::write(&path, text)?;
                let mut permissions = std::fs::metadata(&path)?.permissions();
                permissions.set_readonly(true);
                std::fs::set_permissions(&path, permissions)?;
                (repository.as_path(), path)
            }
        } else {
            let workspace = review
                .workspace
                .as_ref()
                .context("Prepare the review workspace first")?;
            let path = workspace::editor_location(
                &review.data.pr,
                &target.file,
                &target.side,
                line,
                workspace,
            )?;
            (workspace.as_path(), path)
        };
        disable_raw_mode()?;
        execute!(
            tty()?,
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        )?;
        let result = Command::new("hx")
            .stdin(std::process::Stdio::from(tty()?))
            .stdout(std::process::Stdio::from(tty()?))
            .stderr(std::process::Stdio::from(tty()?))
            .current_dir(workspace)
            .arg(format!("+{line}"))
            .arg("--")
            .arg(&path)
            .status()
            .context("Cannot start Helix; install hx and ensure it is on PATH");
        let resume = (|| -> Result<()> {
            enable_raw_mode()?;
            execute!(tty()?, EnterAlternateScreen, EnableBracketedPaste)?;
            terminal.resize(terminal.size()?.into())?;
            Ok(())
        })();
        resume?;
        if !result?.success() {
            bail!("Helix exited unsuccessfully");
        }
        Ok(())
    })();
    match result {
        Ok(()) => app.message("Returned from Helix. Review position preserved.", false),
        Err(error) => app.message(format!("{error:#}"), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_sets_the_inbox_repository_without_cwd_discovery() {
        let args = Args::try_parse_from([
            "prpr",
            "https://github.com/owner/repo/pull/42?diff=split#discussion",
        ])
        .unwrap();
        let (args, target) = args.resolve().unwrap();
        assert_eq!(args.repo.as_deref(), Some("owner/repo"));
        assert_eq!(target.unwrap().number, 42);
    }

    #[test]
    fn numeric_target_uses_the_explicit_repository() {
        let args = Args::try_parse_from(["prpr", "123", "--repo", "owner/repo"]).unwrap();
        let (args, target) = args.resolve().unwrap();
        assert_eq!(args.repo.as_deref(), Some("owner/repo"));
        assert_eq!(
            target.unwrap(),
            PrId {
                repo: "owner/repo".into(),
                number: 123
            }
        );
    }

    #[test]
    fn malformed_targets_and_conflicting_repositories_fail_before_terminal_setup() {
        for argv in [
            vec!["prpr", "0", "--repo", "owner/repo"],
            vec!["prpr", "https://github.com/owner/repo/issues/42"],
            vec![
                "prpr",
                "https://github.com/owner/repo/pull/42",
                "--repo",
                "other/repo",
            ],
            vec!["prpr", "--repo", "../repo"],
        ] {
            assert!(Args::try_parse_from(argv).unwrap().resolve().is_err());
        }
    }

    #[test]
    fn offline_demo_never_resolves_a_repository() {
        let args = Args::try_parse_from(["prpr", "--demo"]).unwrap();
        let (args, target) = args.resolve().unwrap();
        assert!(args.repo.is_none());
        assert!(target.is_none());
    }
}
