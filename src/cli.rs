use crate::{app, store, ui};

use std::{
    io,
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
use clap::Parser;
use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use directories::ProjectDirs;
use pr_review::{
    analysis, github,
    model::*,
    settings::{self, Provider},
    workspace,
};
use ratatui::{Terminal, backend::CrosstermBackend};

use app::{Action, App};

#[derive(Parser)]
#[command(
    name = "gh-preview",
    bin_name = "gh-preview",
    version,
    about = "Review the current repository’s GitHub pull requests with Jujutsu and Helix"
)]
struct Args {
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
        help = "Directory for isolated review workspaces and local drafts"
    )]
    workspace_root: Option<PathBuf>,
    #[arg(long, help = "Explore the interface offline with an example PR")]
    demo: bool,
    #[arg(
        long,
        help = "Configuration file; defaults to ~/.config/gh-preview/config.toml"
    )]
    config: Option<PathBuf>,
    #[arg(long, value_enum, help = "AI explanation provider for this launch")]
    agent: Option<Provider>,
    #[arg(long, help = "AI model override for this launch")]
    model: Option<String>,
    #[arg(long, help = "Print the configuration path and exit")]
    config_path: bool,
}

impl Args {
    fn resolve(mut self) -> Result<(Self, Option<PrId>)> {
        if self.demo {
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
    Inbox(Result<Vec<PrSummary>>),
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

type Tui = Terminal<CrosstermBackend<io::Stdout>>;

#[derive(Default)]
pub(crate) struct Runtime {
    generation: u64,
    pending_comments: Option<Vec<ReviewComment>>,
    pending_ai: Option<bool>,
    ai_request: u64,
    ai_cancel: Option<Arc<AtomicBool>>,
    ai_handle: Option<thread::JoinHandle<()>>,
}

impl Runtime {
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
            self.generation = generation;
            self.pending_comments = None;
            self.pending_ai = None;
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop_ai();
    }
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
    }
}

pub fn run() -> Result<()> {
    let args = Args::parse();
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
    let root = args.workspace_root.clone().map(Ok).unwrap_or_else(|| {
        ProjectDirs::from("dev", "pr-review", "pr-review")
            .map(|p| p.data_local_dir().to_path_buf())
            .context("Cannot find data directory; pass --workspace-root")
    })?;
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir()?.join(root)
    };
    let mut app = App::new(root, args.demo);
    app.repo = args.repo.clone();
    app.settings = settings;
    app.config_path = Some(config_path);
    if args.demo {
        let demo = app::demo_data();
        app.inbox = vec![PrSummary {
            id: demo.pr.id.clone(),
            title: demo.pr.title.clone(),
            author: demo.pr.author.clone(),
            draft: false,
        }];
        app.opened(demo_overview(&demo));
        app.accept_diff(demo)?;
        app.message(
            "Offline demo. 1 Overview / 2 Diff; no agent or GitHub requests are made.",
            false,
        );
    }
    let panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        panic_hook(info);
    }));
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let (sender, receiver) = mpsc::channel();
    let mut runtime = Runtime::default();
    if let Some(id) = target {
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
                JobResult::Overview { .. }
                | JobResult::Diff { .. }
                | JobResult::Comments { .. }
                | JobResult::Analysis { .. } => {}
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
                Event::Paste(text) => {
                    if let Some(
                        app::Dialog::Compose { input, .. }
                        | app::Dialog::Submit { input, .. }
                        | app::Dialog::OpenPr { input },
                    ) = &mut app.dialog
                    {
                        input.text.insert_str(input.cursor, &text);
                        input.cursor += text.len();
                    }
                }
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
        app.message("Offline demo does not run an AI agent", false);
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

fn demo_overview(data: &ReviewData) -> PrOverview {
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
    runtime.sync(app.generation);
    if matches!(action, Action::None | Action::Quit) {
        return Ok(());
    }
    if let Action::OpenEditor(target) = action {
        open_editor_action(app, terminal, *target);
        return Ok(());
    }
    match action {
        Action::Explain { force } => {
            start_analysis(app, sender, runtime, force);
            return Ok(());
        }
        Action::DisableAi => {
            runtime.stop_ai();
            runtime.pending_ai = None;
            return Ok(());
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
            app.opened(demo_overview(&data));
            app.accept_diff(data)?;
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
        Action::None
        | Action::Quit
        | Action::OpenEditor(_)
        | Action::LoadDiff
        | Action::Explain { .. }
        | Action::DisableAi => {
            app.busy = false;
        }
    }
    Ok(())
}

fn open_editor_action(app: &mut App, terminal: &mut Tui, target: app::EditorTarget) {
    let result = (|| -> Result<()> {
        let review = app.review.as_ref().context("No review is open")?;
        let workspace = review
            .workspace
            .as_ref()
            .context("Prepare the review workspace first")?;
        let line = target.line;
        let path = workspace::editor_location(
            &review.data.pr,
            &target.file,
            &target.side,
            line,
            workspace,
        )?;
        disable_raw_mode()?;
        execute!(
            io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        )?;
        let result = Command::new("hx")
            .current_dir(workspace)
            .arg(format!("+{line}"))
            .arg("--")
            .arg(&path)
            .status()
            .context("Cannot start Helix; install hx and ensure it is on PATH");
        let resume = (|| -> Result<()> {
            enable_raw_mode()?;
            execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
            terminal.clear()?;
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
            "gh-preview",
            "https://github.com/owner/repo/pull/42?diff=split#discussion",
        ])
        .unwrap();
        let (args, target) = args.resolve().unwrap();
        assert_eq!(args.repo.as_deref(), Some("owner/repo"));
        assert_eq!(target.unwrap().number, 42);
    }

    #[test]
    fn numeric_target_uses_the_explicit_repository() {
        let args = Args::try_parse_from(["gh-preview", "123", "--repo", "owner/repo"]).unwrap();
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
            vec!["gh-preview", "0", "--repo", "owner/repo"],
            vec!["gh-preview", "https://github.com/owner/repo/issues/42"],
            vec![
                "gh-preview",
                "https://github.com/owner/repo/pull/42",
                "--repo",
                "other/repo",
            ],
            vec!["gh-preview", "--repo", "../repo"],
        ] {
            assert!(Args::try_parse_from(argv).unwrap().resolve().is_err());
        }
    }

    #[test]
    fn offline_demo_never_resolves_a_repository() {
        let args = Args::try_parse_from(["gh-preview", "--demo"]).unwrap();
        let (args, target) = args.resolve().unwrap();
        assert!(args.repo.is_none());
        assert!(target.is_none());
    }
}
