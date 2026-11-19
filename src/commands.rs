use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use prpr::{keybindings::Keybindings, settings::Provider};

use super::{AiStatus, App, PageTab};

pub struct Command {
    pub label: &'static str,
    pub shortcut: String,
    pub id: &'static str,
    pub disabled: Option<&'static str>,
    pub key: KeyEvent,
    aliases: &'static str,
}

impl Command {
    fn new(label: &'static str, shortcut: &'static str, key: char, aliases: &'static str) -> Self {
        Self {
            label,
            shortcut: shortcut.into(),
            id: "",
            key: KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
            disabled: None,
            aliases,
        }
    }

    fn disabled_if(mut self, condition: bool, reason: &'static str) -> Self {
        if condition && self.disabled.is_none() {
            self.disabled = Some(reason);
        }
        self
    }

    /// Names the action directly, for bindings whose default is a key sequence rather than one key.
    fn action(mut self, id: &'static str) -> Self {
        self.id = id;
        self
    }

    fn control(mut self) -> Self {
        self.key.modifiers = KeyModifiers::CONTROL;
        self
    }

    fn matches(&self, query: &str) -> bool {
        let text = format!("{} {} {}", self.label, self.shortcut, self.aliases).to_lowercase();
        query
            .split_whitespace()
            .all(|word| text.contains(&word.to_lowercase()))
    }
}

pub fn available(app: &App, query: &str) -> Vec<Command> {
    let mut commands = Vec::new();
    let diff = app.review.is_some() && (app.overview.is_none() || app.tab == PageTab::Diff);
    let opened = app.overview.is_some() || app.review.is_some();
    let target = opened || app.visible_prs().get(app.selected_pr).is_some();
    let local = app.local_spec.is_some()
        || app
            .review
            .as_ref()
            .is_some_and(|review| review.data.source.is_local());
    let busy = "Wait for the current operation to finish";
    if diff {
        let review = app.review.as_ref().unwrap();
        let file = review.current_file().is_some();
        let text = review
            .current_lines()
            .iter()
            .any(|row| row.kind == prpr::diff::LineKind::Hunk);
        commands.extend([
            Command::new("Next change", "]", ']', "forward hunk jump")
                .disabled_if(review.change_target(true).is_none(), "No next change"),
            Command::new("Previous change", "[", '[', "prev backward hunk jump")
                .disabled_if(review.change_target(false).is_none(), "No previous change"),
            Command::new(
                "Toggle file sidebar",
                "Ctrl-b",
                'b',
                "files tree panel show hide",
            )
            .control(),
            Command::new("Next file", "n", 'n', "forward path").disabled_if(
                review.selected_file + 1 >= review.data.files.len(),
                "Already at the last file",
            ),
            Command::new("Previous file", "p", 'p', "prev backward path")
                .disabled_if(review.selected_file == 0, "Already at the first file"),
            Command::new(
                "Open file in editor",
                "gf / e",
                'e',
                "editor source helix ide",
            )
            .disabled_if(!file, "Choose a file")
            .disabled_if(app.busy, busy)
            .disabled_if(
                app.demo,
                "Source opening is unavailable in the offline demo",
            ),
            Command::new(
                "Toggle unified / split view",
                "|",
                '|',
                "side by side layout columns diff",
            ),
            Command::new(
                "Toggle line wrapping",
                "W",
                'W',
                "wrap long lines horizontal scroll cut off",
            ),
            Command::new(
                "Copy selected lines as Markdown",
                "y",
                'y',
                "clipboard yank snippet",
            )
            .disabled_if(!text, "Choose a textual hunk"),
            Command::new(
                "Search in the current file",
                "/",
                '/',
                "find grep match next previous",
            ),
            Command::new(
                "Search every file",
                "Space /",
                '/',
                "global find grep match picker",
            )
            .action("global_search"),
            Command::new(
                "Open a changed file by name",
                "Space f",
                'f',
                "file picker fuzzy jump goto path",
            )
            .action("file_picker"),
            Command::new("Explain diff or selection", "E", 'E', "ai hunk code")
                .disabled_if(!text, "Choose a textual hunk")
                .disabled_if(
                    app.settings.ai.provider == Provider::None,
                    "Choose an AI agent first",
                ),
            Command::new(
                "Expand hunk context",
                "+",
                '+',
                "unchanged reveal more lines",
            )
            .disabled_if(!file, "Choose a file"),
            Command::new("Collapse hunk context", "-", '-', "unchanged hide lines")
                .disabled_if(!file, "Choose a file"),
            Command::new(
                "Toggle full file context",
                "F",
                'F',
                "expand collapse unchanged",
            )
            .disabled_if(!file, "Choose a file"),
            Command::new(
                "Toggle inline comments",
                "t",
                't',
                "notes threads drafts show hide",
            ),
            Command::new("Read comment threads", "T", 'T', "replies notes full text"),
            Command::new(
                "Manage local notes and replies",
                "N",
                'N',
                "agent comments saved",
            ),
            Command::new(
                "Add local note",
                if local { "c / C" } else { "C" },
                'C',
                "agent comment selection range",
            )
            .disabled_if(
                review.comment_target(true).is_err(),
                "Select numbered lines on one diff side",
            )
            .disabled_if(app.busy, busy),
        ]);
        if !local {
            commands.extend([
                Command::new(
                    "Copy GitHub permalink to selected lines",
                    "gy / Y",
                    'Y',
                    "clipboard url link share",
                )
                .disabled_if(!text, "Choose a textual hunk"),
                Command::new(
                    "Check out PR in this repository",
                    "w",
                    'w',
                    "checkout prepare local branch workspace switch",
                )
                .disabled_if(app.busy, busy)
                .disabled_if(app.demo, "Checkout is unavailable in the offline demo"),
                Command::new(
                    "Add GitHub review comment draft",
                    "c",
                    'c',
                    "inline selection range",
                )
                .disabled_if(
                    review.comment_target(false).is_err(),
                    "Select lines inside an original diff hunk",
                )
                .disabled_if(app.busy, busy),
                Command::new(
                    "Manage GitHub comment drafts",
                    "D",
                    'D',
                    "saved edit delete",
                ),
                Command::new(
                    "Write GitHub review summary",
                    "s",
                    's',
                    "submit approve request changes preview",
                )
                .disabled_if(app.busy, busy),
                Command::new(
                    "Recover uncertain GitHub submission",
                    "u",
                    'u',
                    "review retry sent",
                )
                .disabled_if(app.busy, busy),
            ]);
        }
    }
    if app.review.is_some() {
        commands.push(
            Command::new(
                "Toggle AI chat panel",
                "Ctrl-l",
                'l',
                "ask conversation agent codex claude thread",
            )
            .control(),
        );
        commands.push(
            Command::new(
                if diff {
                    "Ask AI about selected code"
                } else {
                    "Ask AI about this changeset"
                },
                "Q",
                'Q',
                "question agent codex claude",
            )
            .disabled_if(
                app.settings.ai.provider == Provider::None,
                "Choose an AI agent first",
            ),
        );
    }
    if app.overview.is_some() && !diff && !local {
        commands.push(
            Command::new(
                "Check out PR in this repository",
                "w",
                'w',
                "checkout prepare local branch workspace switch",
            )
            .disabled_if(app.review.is_none(), "Wait for the diff to load")
            .disabled_if(app.busy, busy)
            .disabled_if(app.demo, "Checkout is unavailable in the offline demo"),
        );
    }
    if app.overview.is_some() {
        commands.extend([
            Command::new(
                "Show Overview",
                "1",
                '1',
                "description title labels ci checks conversation comments",
            ),
            Command::new(
                "Show AI Guide",
                "2",
                '2',
                "summary intent risks reading order",
            ),
            Command::new("Show Diff", "3", '3', "code changes files"),
            Command::new(
                "Read full change description",
                "b",
                'b',
                "body author markdown",
            ),
        ]);
    }
    if app.overview.is_some() && !local {
        commands.push(
            Command::new(
                "Run or open AI code review",
                "R",
                'R',
                "provider code-review findings claude codex",
            )
            .disabled_if(
                app.settings.ai.provider == Provider::None,
                "Choose an AI agent first",
            )
            .disabled_if(app.review.is_none(), "Wait for the diff to load"),
        );
        commands.push(
            Command::new(
                "Read PR conversation and reviews",
                "V",
                'V',
                "comments activity summaries published",
            )
            .disabled_if(app.busy, busy),
        );
    }
    if app.overview.is_some() || (app.review.is_none() && !local) {
        commands.extend([
            Command::new(
                "Prepare AI reading guide",
                "a",
                'a',
                "explain overview summary intent risks",
            )
            .disabled_if(!target, "Choose a PR first")
            .disabled_if(
                app.overview
                    .as_ref()
                    .is_some_and(|state| matches!(state.ai, AiStatus::Loading)),
                "The AI guide is being prepared",
            ),
            Command::new(
                "Choose AI agent",
                "A",
                'A',
                "codex claude provider settings",
            ),
        ]);
    }
    if opened && !local {
        commands.push(
            Command::new(
                "Comment on the PR conversation",
                "M",
                'M',
                "message reply discussion issue comment draft",
            )
            .disabled_if(app.busy, busy),
        );
    }
    if !local {
        commands.extend([
            Command::new("Open PR on GitHub", "B", 'B', "browser web")
                .disabled_if(!target, "Choose a PR first"),
            Command::new("Open PR by number", "#", '#', "pull request").disabled_if(app.busy, busy),
        ]);
    }
    if !opened && !local {
        commands.extend([
            Command::new("Open selected PR diff", "o", 'o', "review pull request")
                .disabled_if(!target, "Choose a PR first")
                .disabled_if(app.busy, busy),
            Command::new("Filter PR list", "/", '/', "search pull requests"),
            Command::new("Toggle PR preview", "p", 'p', "description panel show hide"),
            Command::new(
                "Change PR preview position",
                "P",
                'P',
                "bottom right panel layout",
            ),
        ]);
    }
    let running = app.chat.pending.is_some()
        || app
            .overview
            .as_ref()
            .is_some_and(|state| matches!(state.ai, AiStatus::Loading))
        || app
            .preview
            .guides
            .values()
            .any(|guide| matches!(guide.status, AiStatus::Loading))
        || app
            .ai_reviews
            .values()
            .any(|status| matches!(status, super::AiReviewStatus::Running(_)));
    commands.extend([
        Command::new(
            "Stop running AI work",
            "Ctrl-x",
            'x',
            "cancel abort interrupt chat answer guide review agent",
        )
        .control()
        .disabled_if(!running, "No AI work is running"),
        Command::new("Refresh review", "r", 'r', "reload update").disabled_if(app.busy, busy),
        Command::new("Show keyboard shortcuts", "?", '?', "help bindings"),
        Command::new("Show operation details", "!", '!', "status errors log"),
        Command::new("Quit prpr", "q", 'q', "exit close").disabled_if(app.busy, busy),
    ]);
    for command in &mut commands {
        if command.id.is_empty() {
            command.id = Keybindings::default_action(app.binding_view(), command.key)
                .expect("Palette command must have a normal-view action");
        }
        command.shortcut = if command.id == "local_note" && local {
            let mut keys = app.settings.keybindings.keys("comment");
            keys.extend(app.settings.keybindings.keys("local_note"));
            if keys.is_empty() {
                "Unbound".into()
            } else {
                keys.join(" / ")
            }
        } else {
            app.shortcut(command.id)
        };
    }
    commands
        .into_iter()
        .filter(|command| command.matches(query))
        .collect()
}
