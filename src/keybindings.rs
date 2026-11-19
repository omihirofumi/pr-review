use std::{collections::BTreeMap, fmt};

use anyhow::{Context, Result, bail, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Inbox,
    Overview,
    Guide,
    Diff,
}

impl View {
    fn flag(self) -> u8 {
        match self {
            Self::Inbox => I,
            Self::Overview => O,
            Self::Guide => G,
            Self::Diff => D,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Inbox => "PR list",
            Self::Overview => "Overview",
            Self::Guide => "AI Guide",
            Self::Diff => "Diff",
        }
    }
}

const I: u8 = 1;
const O: u8 = 2;
const D: u8 = 4;
const G: u8 = 8;
/// Diff while a `/` search is active; these keys take precedence over the Diff bindings.
const S: u8 = 16;
const ALL: u8 = I | O | G | D;
const REVIEW: u8 = O | G | D;

pub const HELP_GROUPS: &[(u8, &str)] = &[
    (ALL, "GENERAL"),
    (REVIEW, "OPEN REVIEW"),
    (I, "PR LIST"),
    (O, "OVERVIEW"),
    (G, "AI GUIDE"),
    (D, "DIFF"),
    (S, "DIFF SEARCH (while a search is active)"),
];

pub struct BindingSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub scope: u8,
    pub defaults: &'static [&'static str],
}

impl BindingSpec {
    pub fn applies(&self, view: View) -> bool {
        self.scope & view.flag() != 0
    }
    pub fn views(&self) -> String {
        [View::Inbox, View::Overview, View::Guide, View::Diff]
            .into_iter()
            .filter(|view| self.applies(*view))
            .map(View::name)
            .chain((self.scope & S != 0).then_some("Diff search"))
            .collect::<Vec<_>>()
            .join(", ")
    }
    pub fn canonical(&self) -> Vec<KeyEvent> {
        parse_sequence(self.defaults[0])
            .expect("Built-in shortcut must be valid")
            .into_iter()
            .map(Stroke::event)
            .collect()
    }
}

macro_rules! specs {
    ($(($id:literal, $label:literal, $scope:expr, [$($key:literal),+ $(,)?])),+ $(,)?) => {
        pub static SPECS: &[BindingSpec] = &[$(BindingSpec { id: $id, label: $label, scope: $scope, defaults: &[$($key),+] }),+];
    };
}

specs! {
    ("command_palette", "Open command palette", ALL, ["Ctrl-k"]),
    ("toggle_sidebar", "Toggle file sidebar", REVIEW, ["Ctrl-b"]),
    ("move_down", "Move down / scroll", ALL, ["j", "Down"]),
    ("move_up", "Move up / scroll", ALL, ["k", "Up"]),
    ("half_page_down", "Half-page down", ALL, ["Ctrl-d"]),
    ("half_page_up", "Half-page up", ALL, ["Ctrl-u"]),
    ("page_down", "Page down", ALL, ["PageDown"]),
    ("page_up", "Page up", ALL, ["PageUp"]),
    ("go_to_start", "Go to first item / top", ALL, ["gg"]),
    ("go_to_end", "Go to last item / bottom", ALL, ["ge"]),
    ("home", "Home", ALL, ["Home"]),
    ("end", "End", ALL, ["G", "End"]),
    ("next_view", "Switch view / focus", ALL, ["Tab"]),
    ("previous_view", "Switch view / focus backward", ALL, ["Shift-Tab"]),
    ("overview", "Show Overview", REVIEW, ["1"]),
    ("guide", "Show AI Guide", REVIEW, ["2"]),
    ("diff", "Show Diff", REVIEW, ["3"]),
    ("ask", "Ask AI", REVIEW, ["Q"]),
    ("explain_diff", "Explain diff or selection", REVIEW, ["E"]),
    ("toggle_chat", "Toggle AI chat panel", REVIEW, ["Ctrl-l"]),
    ("ai_guide", "Prepare AI reading guide", ALL, ["a"]),
    ("ai_review", "Run or open the provider's AI code review", REVIEW, ["R"]),
    ("stop_ai", "Stop running AI work", ALL, ["Ctrl-x"]),
    ("choose_agent", "Choose AI agent", ALL, ["A"]),
    ("description", "Read full change description", REVIEW, ["b"]),
    ("open_github", "Open PR on GitHub", ALL, ["B"]),
    ("open_pr_number", "Open PR by number", ALL, ["#"]),
    ("refresh", "Refresh", ALL, ["r"]),
    ("help", "Show keyboard shortcuts", ALL, ["?"]),
    ("status", "Show operation details", ALL, ["!"]),
    ("back", "Clear selection or search / back / exit local review", ALL, ["Esc"]),
    ("quit", "Quit", ALL, ["q"]),
    ("focus_files", "Show and focus file sidebar", D, ["h"]),
    ("focus_diff", "Focus diff", D, ["l", "Enter"]),
    ("next_file", "Next file", D, ["n", "."]),
    ("previous_file", "Previous file", D, ["p", ","]),
    ("next_hunk", "Next change", D, ["]"]),
    ("previous_hunk", "Previous change", D, ["["]),
    ("scroll_left", "Scroll diff left", D, ["Left"]),
    ("scroll_right", "Scroll diff right", D, ["Right"]),
    ("expand_context", "Expand hunk context", D, ["+"]),
    ("collapse_context", "Collapse hunk context", D, ["-"]),
    ("full_file", "Toggle full file context", D, ["F"]),
    ("select", "Toggle selection extension", D, ["v"]),
    ("select_line", "Select line / extend downward", D, ["x"]),
    ("select_bounds", "Select whole-line bounds", D, ["X"]),
    ("clear_selection", "Clear selection", D, [";"]),
    ("toggle_comments", "Toggle inline comments and notes", D, ["t"]),
    ("threads", "Read full comment threads", D, ["T"]),
    ("reply_thread", "Reply to the review thread on this line", D, ["gr"]),
    ("notes", "Manage local notes and replies", D, ["N"]),
    ("drafts", "Manage GitHub drafts / local notes", D, ["D"]),
    ("comment", "GitHub comment draft / local note", D, ["c"]),
    ("local_note", "Add local note", D, ["C"]),
    ("checkout", "Check out the PR in this repository", REVIEW, ["w"]),
    ("open_file", "Open source in editor", D, ["gf", "e"]),
    ("copy_selection", "Copy selected lines as Markdown", D, ["y"]),
    ("search", "Search in the current file", D, ["/"]),
    ("global_search", "Search every file in a picker", D, ["Space /"]),
    ("file_picker", "Open a changed file by name", D, ["Space f"]),
    ("search_next", "Next search match", S, ["n"]),
    ("search_previous", "Previous search match", S, ["N"]),
    ("toggle_split", "Toggle unified / split diff", D, ["|"]),
    ("toggle_wrap", "Toggle line wrapping in the diff", D, ["W"]),
    ("copy_permalink", "Copy GitHub permalink to selected lines", D, ["gy", "Y"]),
    ("review_summary", "Write GitHub review summary", D, ["s"]),
    ("review_activity", "Read PR conversation and reviews", REVIEW, ["V"]),
    ("conversation_comment", "Write a PR conversation comment (drafts are kept)", REVIEW, ["M"]),
    ("recover_review", "Recover uncertain GitHub submission", D, ["u"]),
    ("next_reading_step", "Next reading-guide step", G, ["n"]),
    ("previous_reading_step", "Previous reading-guide step", G, ["p"]),
    ("open_reading_step", "Open reading-guide step in Diff", G, ["Enter", "o", "gf"]),
    ("focus_pr_list", "Focus PR list", I, ["h", "Left"]),
    ("focus_preview", "Focus PR preview", I, ["l", "Right"]),
    ("toggle_preview", "Toggle PR preview", I, ["p"]),
    ("preview_position", "Change PR preview position", I, ["P"]),
    ("cycle_preview", "Switch description / guide preview", I, ["[", "]"]),
    ("preview_description", "Show description preview", I, ["1"]),
    ("preview_guide", "Show AI guide preview", I, ["2"]),
    ("filter", "Filter PR list", I, ["/"]),
    ("open_pr", "Open selected PR", I, ["o", "Enter"]),
}

pub fn spec(id: &str) -> Option<&'static BindingSpec> {
    SPECS.iter().find(|spec| spec.id == id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stroke {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl Stroke {
    pub fn from_event(event: KeyEvent) -> Self {
        let mut code = event.code;
        let mut modifiers = event.modifiers;
        match code {
            KeyCode::Char(mut character) => {
                if modifiers.contains(KeyModifiers::SHIFT) && character.is_ascii_lowercase() {
                    character = character.to_ascii_uppercase();
                }
                if modifiers.contains(KeyModifiers::CONTROL) {
                    character = character.to_ascii_lowercase();
                }
                modifiers.remove(KeyModifiers::SHIFT);
                code = KeyCode::Char(character);
            }
            KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => {
                code = KeyCode::BackTab;
                modifiers.remove(KeyModifiers::SHIFT);
            }
            KeyCode::BackTab => modifiers.remove(KeyModifiers::SHIFT),
            _ => {}
        }
        if modifiers.contains(KeyModifiers::CONTROL) {
            let alias = match code {
                KeyCode::Char('h' | '?') => Some(KeyCode::Backspace),
                KeyCode::Char('i') => Some(KeyCode::Tab),
                KeyCode::Char('j' | 'm') => Some(KeyCode::Enter),
                KeyCode::Char('[') => Some(KeyCode::Esc),
                _ => None,
            };
            if let Some(alias) = alias {
                code = alias;
                modifiers.remove(KeyModifiers::CONTROL);
            }
        }
        Self { code, modifiers }
    }
    pub fn event(self) -> KeyEvent {
        KeyEvent::new(self.code, self.modifiers)
    }
    pub fn is_escape(self) -> bool {
        self.code == KeyCode::Esc
    }
    pub fn is_palette_control(self) -> bool {
        matches!(
            self.code,
            KeyCode::Enter
                | KeyCode::Esc
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Backspace
                | KeyCode::Delete
        ) || (self.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(self.code, KeyCode::Char('n' | 'p' | 'u' | 'h')))
    }
    pub fn is_text(self) -> bool {
        matches!(self.code, KeyCode::Char(_)) && !self.modified()
    }
    pub fn modified(self) -> bool {
        self.modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    }
}

impl fmt::Display for Stroke {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (flag, name) in [
            (KeyModifiers::CONTROL, "Ctrl-"),
            (KeyModifiers::ALT, "Alt-"),
            (KeyModifiers::SHIFT, "Shift-"),
        ] {
            if self.modifiers.contains(flag) {
                formatter.write_str(name)?;
            }
        }
        match self.code {
            KeyCode::Char(' ') => formatter.write_str("Space"),
            KeyCode::Char(character) => write!(formatter, "{character}"),
            KeyCode::BackTab => formatter.write_str("Shift-Tab"),
            KeyCode::F(number) => write!(formatter, "F{number}"),
            code => write!(formatter, "{code:?}"),
        }
    }
}

fn parse_token(token: &str) -> Result<Stroke> {
    let mut rest = token;
    let mut modifiers = KeyModifiers::NONE;
    loop {
        let lower = rest.to_ascii_lowercase();
        let found = [
            ("ctrl-", KeyModifiers::CONTROL),
            ("alt-", KeyModifiers::ALT),
            ("shift-", KeyModifiers::SHIFT),
        ]
        .into_iter()
        .find(|(prefix, _)| lower.starts_with(prefix));
        let Some((prefix, flag)) = found else {
            break;
        };
        ensure!(!modifiers.contains(flag), "Duplicate modifier in {token}");
        modifiers.insert(flag);
        rest = &rest[prefix.len()..];
    }
    let code = match rest.to_ascii_lowercase().as_str() {
        "space" => KeyCode::Char(' '),
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        _ if rest.starts_with('F')
            && rest[1..]
                .parse::<u8>()
                .is_ok_and(|number| (1..=24).contains(&number)) =>
        {
            KeyCode::F(rest[1..].parse()?)
        }
        _ if rest.chars().count() == 1 => KeyCode::Char(rest.chars().next().unwrap()),
        _ => bail!(
            "Unknown key {token}; use a character, named key, or Ctrl-/Alt-/Shift- combination"
        ),
    };
    if let KeyCode::Char(character) = code {
        ensure!(
            !modifiers.contains(KeyModifiers::SHIFT) || character.is_ascii_alphabetic(),
            "Use the resulting character directly for shifted punctuation or spaces"
        );
    }
    Ok(Stroke::from_event(KeyEvent::new(code, modifiers)))
}

pub fn parse_sequence(text: &str) -> Result<Vec<Stroke>> {
    ensure!(
        !text.trim().is_empty(),
        "A shortcut cannot be empty; use [] to disable its binding"
    );
    let mut keys = Vec::new();
    for token in text.split_whitespace() {
        if !token.contains('-')
            && token.chars().count() > 1
            && token.chars().all(char::is_lowercase)
            && ![
                "space",
                "enter",
                "return",
                "esc",
                "escape",
                "tab",
                "backtab",
                "backspace",
                "delete",
                "up",
                "down",
                "left",
                "right",
                "home",
                "end",
                "pageup",
                "pagedown",
            ]
            .contains(&token)
        {
            for character in token.chars() {
                keys.push(parse_token(&character.to_string())?);
            }
        } else {
            keys.push(parse_token(token)?);
        }
    }
    ensure!(keys.len() <= 4, "A shortcut can contain at most four keys");
    let interrupt = Stroke::from_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    ensure!(
        !keys.contains(&interrupt),
        "Ctrl-c is reserved for immediate exit"
    );
    ensure!(
        keys.len() == 1 || !keys.iter().any(|key| key.code == KeyCode::Esc),
        "Esc cancels pending sequences and cannot be part of a multi-key shortcut"
    );
    Ok(keys)
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Keybindings(pub BTreeMap<String, Vec<String>>);

pub enum Match {
    None,
    Prefix,
    Action(&'static str),
}

impl Keybindings {
    pub fn keys(&self, id: &str) -> Vec<String> {
        self.0.get(id).cloned().unwrap_or_else(|| {
            spec(id).map_or_else(Vec::new, |spec| {
                spec.defaults.iter().map(|key| (*key).to_owned()).collect()
            })
        })
    }
    pub fn display(&self, id: &str) -> String {
        let keys = self.keys(id);
        if keys.is_empty() {
            "Unbound".into()
        } else {
            keys.join(" / ")
        }
    }
    pub fn primary(&self, id: &str) -> String {
        self.keys(id)
            .into_iter()
            .next()
            .unwrap_or_else(|| "Unbound".into())
    }
    pub fn validate(&self) -> Result<()> {
        for id in self.0.keys() {
            ensure!(
                spec(id).is_some(),
                "Unknown shortcut action {id}; use --print-keybindings to list action names"
            );
        }
        for (flag, name) in [
            (I, View::Inbox.name()),
            (O, View::Overview.name()),
            (G, View::Guide.name()),
            (D, View::Diff.name()),
            (S, "Diff search"),
        ] {
            let mut seen: Vec<(Vec<Stroke>, &str)> = Vec::new();
            for spec in SPECS.iter().filter(|spec| spec.scope & flag != 0) {
                for text in self.keys(spec.id) {
                    let sequence = parse_sequence(&text)
                        .with_context(|| format!("Invalid keybindings.{}", spec.id))?;
                    for (previous, id) in &seen {
                        ensure!(
                            !sequence.starts_with(previous) && !previous.starts_with(&sequence),
                            "Shortcut conflict in {}: {} ({}) overlaps {} ({})",
                            name,
                            spec.id,
                            text,
                            id,
                            previous
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join(" ")
                        );
                    }
                    seen.push((sequence, spec.id));
                }
            }
        }
        Ok(())
    }
    pub fn resolve(&self, view: View, pending: &[Stroke]) -> Result<Match> {
        let mut prefix = false;
        for spec in SPECS.iter().filter(|spec| spec.applies(view)) {
            for text in self.keys(spec.id) {
                let sequence = parse_sequence(&text)?;
                if sequence == pending {
                    return Ok(Match::Action(spec.id));
                }
                prefix |= sequence.starts_with(pending);
            }
        }
        Ok(if prefix { Match::Prefix } else { Match::None })
    }
    pub fn single_matches(&self, id: &str, key: KeyEvent) -> bool {
        self.keys(id)
            .iter()
            .any(|text| parse_sequence(text).is_ok_and(|keys| keys == [Stroke::from_event(key)]))
    }
    pub fn default_action(view: View, key: KeyEvent) -> Option<&'static str> {
        let key = Stroke::from_event(key);
        SPECS
            .iter()
            .filter(|spec| spec.applies(view))
            .find(|spec| {
                spec.defaults
                    .iter()
                    .any(|text| parse_sequence(text).is_ok_and(|sequence| sequence == [key]))
            })
            .map(|spec| spec.id)
    }
    pub fn print(&self) -> Result<String> {
        let mut text = String::from("[keybindings]\n");
        for spec in SPECS {
            text.push_str(&format!(
                "# {} ({})\n{} = {}\n",
                spec.label,
                spec.views(),
                spec.id,
                serde_json::to_string(&self.keys(spec.id))?
            ));
        }
        Ok(text)
    }
}
