use crate::{app, store, ui};

use std::{io, path::PathBuf, process::Command, sync::mpsc, thread, time::Duration};

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use directories::ProjectDirs;
use pr_review::{github, model::*, workspace};
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

enum JobResult {
    Inbox(Result<Vec<PrSummary>>),
    Review(Result<Box<ReviewData>>),
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
    let (args, target) = Args::parse().resolve()?;
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
    if args.demo {
        let demo = app::demo_data();
        app.inbox = vec![PrSummary {
            id: demo.pr.id.clone(),
            title: demo.pr.title.clone(),
            author: demo.pr.author.clone(),
            draft: false,
        }];
        app.loaded(demo)?;
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
    if let Some(id) = target {
        dispatch(Action::LoadPr(id), &mut app, &args, &sender, &mut terminal)?;
    } else if !args.demo {
        dispatch(Action::LoadInbox, &mut app, &args, &sender, &mut terminal)?;
    }
    loop {
        while let Ok(result) = receiver.try_recv() {
            app.busy = false;
            match result {
                JobResult::Inbox(Ok(prs)) => {
                    app.inbox = prs;
                    app.selected_pr = 0;
                    app.message(format!("{} pull requests loaded", app.inbox.len()), false);
                }
                JobResult::Review(Ok(data)) => {
                    if let Err(error) = app.loaded(*data) {
                        app.message(format!("{error:#}"), true);
                    }
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
                | JobResult::Review(Err(error))
                | JobResult::Activity(Err(error))
                | JobResult::Prepared {
                    result: Err(error), ..
                } => app.message(format!("{error:#}"), true),
            }
        }
        terminal.draw(|frame| ui::draw(frame, &app))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match app.key(key) {
                    Ok(Action::Quit) => break,
                    Ok(action) => {
                        if let Err(error) =
                            dispatch(action, &mut app, &args, &sender, &mut terminal)
                        {
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

fn dispatch(
    action: Action,
    app: &mut App,
    args: &Args,
    sender: &mpsc::Sender<JobResult>,
    terminal: &mut Tui,
) -> Result<()> {
    if matches!(action, Action::None | Action::Quit) {
        return Ok(());
    }
    if let Action::OpenEditor(target) = action {
        open_editor_action(app, terminal, *target);
        return Ok(());
    }
    if app.demo {
        if let Action::LoadPr(_) = action {
            app.loaded(app::demo_data())?;
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
            app.message(format!("Loading {} #{}...", id.repo, id.number), false);
            thread::spawn(move || {
                let _ = sender.send(JobResult::Review(github::load_review(&id).map(Box::new)));
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
        Action::None | Action::Quit | Action::OpenEditor(_) => {
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
