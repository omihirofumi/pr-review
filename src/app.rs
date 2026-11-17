#[path = "commands.rs"]
pub mod commands;
use std::{collections::HashMap, path::PathBuf};

use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use prpr::{
    analysis::Analysis,
    assistant::{self, Chat},
    context::{self, FileContext, FullFile},
    diff::{self, DiffLine, LineKind},
    keybindings::{self, Stroke, View},
    local,
    model::*,
    notes,
    settings::{self, OpenIn, Provider, Settings},
};

use crate::store;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Files,
    Diff,
}

#[derive(Default, Debug)]
pub struct TextInput {
    pub text: String,
    pub cursor: usize,
}

impl TextInput {
    pub fn new(text: String) -> Self {
        let cursor = text.len();
        Self { text, cursor }
    }

    pub fn key(&mut self, key: KeyEvent) {
        let code =
            if key.code == KeyCode::Char('h') && key.modifiers.contains(KeyModifiers::CONTROL) {
                KeyCode::Backspace
            } else {
                key.code
            };
        match code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            KeyCode::Enter => {
                self.text.insert(self.cursor, '\n');
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let previous = self.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .unwrap()
                    .0;
                self.text.drain(previous..self.cursor);
                self.cursor = previous;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                let next = self.text[self.cursor..].chars().next().unwrap().len_utf8();
                self.text.drain(self.cursor..self.cursor + next);
            }
            KeyCode::Left if self.cursor > 0 => {
                self.cursor = self.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .unwrap()
                    .0
            }
            KeyCode::Right if self.cursor < self.text.len() => {
                self.cursor += self.text[self.cursor..].chars().next().unwrap().len_utf8()
            }
            KeyCode::Home => {
                self.cursor = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
            }
            KeyCode::End => {
                self.cursor += self.text[self.cursor..]
                    .find('\n')
                    .unwrap_or(self.text.len() - self.cursor)
            }
            KeyCode::Up | KeyCode::Down => {
                let start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
                let column = self.text[start..self.cursor].chars().count();
                let target = if code == KeyCode::Up {
                    if start == 0 {
                        return;
                    }
                    let end = start - 1;
                    let begin = self.text[..end].rfind('\n').map_or(0, |i| i + 1);
                    begin..end
                } else {
                    let Some(end) = self.text[self.cursor..].find('\n') else {
                        return;
                    };
                    let begin = self.cursor + end + 1;
                    let end = begin
                        + self.text[begin..]
                            .find('\n')
                            .unwrap_or(self.text.len() - begin);
                    begin..end
                };
                self.cursor = target.start
                    + self.text[target.clone()]
                        .char_indices()
                        .nth(column)
                        .map_or(target.len(), |(i, _)| i);
            }
            _ => {}
        }
    }
}

pub enum Dialog {
    Palette {
        input: TextInput,
        selected: usize,
    },
    Note {
        anchor: DraftComment,
        reply_to: Option<String>,
        input: TextInput,
    },
    Notes {
        selected: usize,
    },
    Ask {
        input: TextInput,
        focus: Option<Box<assistant::Focus>>,
        scroll: u16,
    },
    AgentPicker {
        selected: usize,
    },
    OpenPr {
        input: TextInput,
    },
    Help {
        scroll: u16,
    },
    Status {
        scroll: u16,
    },
    Activity {
        text: String,
        scroll: u16,
    },
    Recovery {
        attempt: store::SubmissionAttempt,
    },
    Description {
        scroll: u16,
    },
    Threads {
        scroll: u16,
    },
    Drafts {
        selected: usize,
    },
    Compose {
        draft: DraftComment,
        editing: Option<usize>,
        input: TextInput,
    },
    Submit {
        event: ReviewEvent,
        input: TextInput,
    },
    Confirm {
        event: ReviewEvent,
        body: String,
        scroll: u16,
    },
    Quit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageTab {
    #[default]
    Overview,
    Guide,
    Diff,
}

impl PageTab {
    fn cycle(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::Overview, true) | (Self::Diff, false) => Self::Guide,
            (Self::Guide, true) | (Self::Overview, false) => Self::Diff,
            (Self::Diff, true) | (Self::Guide, false) => Self::Overview,
        }
    }
}

pub enum ActivityStatus {
    Unavailable,
    Loading,
    Ready(PrActivity),
    Error(String),
}

pub enum AiStatus {
    Disabled,
    Waiting,
    Loading,
    Ready(Box<Analysis>),
    Error(String),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum PreviewTab {
    #[default]
    Description,
    Guide,
}

pub struct PreviewGuide {
    pub revision: String,
    pub status: AiStatus,
}

pub struct PreviewState {
    pub shown: bool,
    pub bottom: Option<bool>,
    pub actual_bottom: bool,
    pub focused: bool,
    pub tab: PreviewTab,
    pub scroll: u16,
    pub guides: HashMap<String, PreviewGuide>,
}

impl Default for PreviewState {
    fn default() -> Self {
        Self {
            shown: true,
            bottom: None,
            actual_bottom: false,
            focused: false,
            tab: PreviewTab::Description,
            scroll: 0,
            guides: HashMap::new(),
        }
    }
}

pub fn preview_key(id: &PrId) -> String {
    format!("{}#{}", id.repo, id.number)
}

pub struct OverviewState {
    pub metadata: PrOverview,
    pub activity: ActivityStatus,
    pub scroll: u16,
    pub guide_scroll: u16,
    pub selected_step: usize,
    pub focus_guide: bool,
    pub ai: AiStatus,
}

pub struct ReviewState {
    pub notes: Vec<notes::Note>,
    pub contexts: Vec<context::FileState>,
    pub data: ReviewData,
    pub lines: Vec<Vec<DiffLine>>,
    pub selected_file: usize,
    pub positions: Vec<usize>,
    pub focus: Focus,
    pub horizontal: u16,
    pub show_comments: bool,
    pub selection: Option<Selection>,
    pub drafts: Vec<DraftComment>,
    pub workspace: Option<PathBuf>,
    pub viewport: Viewport,
    pub marks: Vec<Mark>,
}

/// Diff scroll position, updated while drawing so it follows the rendered row layout.
#[derive(Debug, Default)]
pub struct Viewport {
    pub tops: std::cell::RefCell<HashMap<usize, usize>>,
    pub cursor_offset: std::cell::Cell<usize>,
    pub keep_offset: std::cell::Cell<bool>,
}

#[derive(Clone, Copy, Debug)]
pub struct Selection {
    pub anchor: usize,
    pub extending: bool,
}

pub struct EditorTarget {
    pub file: ChangedFile,
    pub side: Side,
    pub line: u32,
}

impl ReviewState {
    pub fn comments(&self) -> Vec<ReviewComment> {
        let mut comments = self.data.comments.clone();
        comments.extend(notes::comments(&self.notes));
        comments
    }

    pub fn validate_note(&self, anchor: &DraftComment) -> Result<()> {
        let index = self
            .data
            .files
            .iter()
            .position(|file| file.filename == anchor.path)
            .ok_or_else(|| anyhow::anyhow!("File is outside this review"))?;
        let start = anchor.start_line.unwrap_or(anchor.line);
        if start == 0
            || start > anchor.line
            || anchor
                .start_side
                .as_ref()
                .is_some_and(|side| *side != anchor.side)
        {
            bail!("Invalid note range");
        }
        let numbers = self.lines[index]
            .iter()
            .filter_map(|row| match anchor.side {
                Side::Left => row.old_line,
                Side::Right => row.new_line,
            })
            .collect::<std::collections::HashSet<_>>();
        if u64::from(anchor.line) - u64::from(start) + 1 > numbers.len() as u64
            || !(start..=anchor.line).all(|number| numbers.contains(&number))
        {
            bail!("Note range is outside the visible source; expand its context first");
        }
        Ok(())
    }

    pub fn current_file(&self) -> Option<&ChangedFile> {
        self.data.files.get(self.selected_file)
    }
    pub fn current_lines(&self) -> &[DiffLine] {
        self.lines
            .get(self.selected_file)
            .map_or(&[], Vec::as_slice)
    }
    pub fn position(&self) -> usize {
        self.positions.get(self.selected_file).copied().unwrap_or(0)
    }
    pub fn anchor(&self) -> Option<(Side, u32)> {
        self.current_lines().get(self.position())?.anchor()
    }

    pub fn selected_range(&self) -> Option<(usize, usize)> {
        let selection = self.selection?;
        let cursor = self.position();
        Some((selection.anchor.min(cursor), selection.anchor.max(cursor)))
    }

    fn current_hunks(&self, file: usize) -> Vec<usize> {
        let position = self.positions[file];
        if let Some(view) = &self.contexts[file].view {
            return view.hunks_at(position);
        }
        vec![
            self.lines[file]
                .iter()
                .take(position.saturating_add(1))
                .filter(|row| row.kind == LineKind::Hunk)
                .count()
                .saturating_sub(1),
        ]
    }

    pub fn change_context(&mut self, command: char) -> Result<Option<usize>> {
        let file = self.selected_file;
        if self.current_file().is_none() {
            bail!("Select a file to expand");
        }
        let hunks = self.current_hunks(file);
        let state = &mut self.contexts[file];
        if command == '+' && state.expansion.full {
            return Ok(None);
        }
        match command {
            '+' => {
                for hunk in hunks {
                    let radius = state.expansion.radius.entry(hunk).or_default();
                    *radius = radius.saturating_add(20);
                }
            }
            '-' => {
                if state.expansion.full {
                    state.expansion = Default::default();
                } else {
                    for hunk in hunks {
                        state.expansion.radius.remove(&hunk);
                    }
                }
            }
            'F' => state.expansion.full = !state.expansion.full,
            _ => return Ok(None),
        }
        self.focus = Focus::Diff;
        if state.full_file.is_some() {
            self.rebuild_context(file);
            return Ok(None);
        }
        if command != '-' && !state.loading {
            state.loading = true;
            return Ok(Some(file));
        }
        Ok(None)
    }

    pub fn accept_context(&mut self, file: usize, source: FileContext) -> Result<()> {
        self.contexts[file].loading = false;
        self.contexts[file].full_file = Some(FullFile::new(&self.data.files[file], &source)?);
        self.rebuild_context(file);
        Ok(())
    }

    fn rebuild_context(&mut self, file: usize) {
        let position = self.positions[file];
        let cursor = self.lines[file].get(position).cloned();
        let hint = self.current_hunks(file).first().copied().unwrap_or(0);
        let anchor = if file == self.selected_file {
            self.selection.and_then(|selection| {
                self.lines[file]
                    .get(selection.anchor)
                    .cloned()
                    .map(|row| (selection, row))
            })
        } else {
            None
        };
        let state = &mut self.contexts[file];
        let Some(source) = &state.full_file else {
            return;
        };
        let view = source.render(&state.expansion);
        let locate = |old: &DiffLine| {
            view.lines.iter().position(|row| {
                row.kind == old.kind
                    && row.old_line == old.old_line
                    && row.new_line == old.new_line
                    && row.text == old.text
            })
        };
        let exact = cursor.as_ref().and_then(locate);
        let target = exact.unwrap_or_else(|| {
            cursor
                .as_ref()
                .and_then(DiffLine::anchor)
                .and_then(|(side, line)| {
                    view.lines
                        .iter()
                        .enumerate()
                        .filter_map(|(index, row)| {
                            row.anchor()
                                .filter(|(row_side, _)| row_side == &side)
                                .map(|(_, number)| (index, number.abs_diff(line)))
                        })
                        .min_by_key(|(_, distance)| *distance)
                        .map(|(index, _)| index)
                })
                .unwrap_or_else(|| view.position_for_hunk(hint))
        });
        if file == self.selected_file {
            self.selection = anchor.and_then(|(mut selection, row)| {
                exact?;
                selection.anchor = locate(&row)?;
                Some(selection)
            });
        }
        self.positions[file] = target;
        self.lines[file] = view.lines.clone();
        state.view = Some(view);
    }

    pub fn ai_data(&self, focus: Option<&assistant::Focus>) -> ReviewData {
        let mut data = self.data.clone();
        if let Some(focus) = focus
            && let Some(index) = data
                .files
                .iter()
                .position(|file| file.filename == focus.path)
            && self.contexts[index].view.is_some()
        {
            data.files[index].patch = Some(focus.patch.clone());
        }
        data
    }

    pub fn question_focus(&self) -> Result<Box<assistant::Focus>> {
        let file = self
            .current_file()
            .ok_or_else(|| anyhow::anyhow!("Choose a file"))?;
        if let Some(view) = &self.contexts[self.selected_file].view {
            return Ok(Box::new(view.question_focus(
                file,
                self.position(),
                self.selected_range(),
            )?));
        }
        Ok(Box::new(assistant::Focus::from_file(
            file,
            self.position(),
            self.selected_range(),
        )?))
    }

    fn select_line(&mut self, extend_below: bool) -> Result<()> {
        self.focus = Focus::Diff;
        if let Some(mut selection) = self.selection {
            if extend_below {
                let start = selection.anchor.min(self.position());
                let end = selection.anchor.max(self.position());
                let max = self.current_lines().len().saturating_sub(1);
                selection.anchor = start;
                self.selection = Some(selection);
                self.positions[self.selected_file] = end.saturating_add(1).min(max);
            }
            return Ok(());
        }
        let position = self
            .current_lines()
            .iter()
            .enumerate()
            .skip(self.position())
            .find(|(_, line)| line.anchor().is_some())
            .map(|(index, _)| index)
            .or_else(|| {
                self.current_lines()
                    .iter()
                    .enumerate()
                    .take(self.position())
                    .rev()
                    .find(|(_, line)| line.anchor().is_some())
                    .map(|(index, _)| index)
            })
            .ok_or_else(|| anyhow::anyhow!("This file has no numbered diff lines to select"))?;
        self.positions[self.selected_file] = position;
        self.selection = Some(Selection {
            anchor: position,
            extending: false,
        });
        self.focus = Focus::Diff;
        Ok(())
    }

    fn toggle_selection(&mut self) -> Result<()> {
        if let Some(selection) = &mut self.selection {
            selection.extending = !selection.extending;
        } else {
            self.select_line(false)?;
            self.selection.as_mut().unwrap().extending = true;
        }
        self.focus = Focus::Diff;
        Ok(())
    }

    fn goto_line(&mut self, position: usize) {
        if self.selection.is_some_and(|selection| !selection.extending) {
            self.selection = None;
        }
        let max = self.current_lines().len().saturating_sub(1);
        if let Some(cursor) = self.positions.get_mut(self.selected_file) {
            *cursor = position.min(max);
        }
    }

    fn comment_target(&self, local_note: bool) -> Result<DraftComment> {
        let file = self
            .current_file()
            .ok_or_else(|| anyhow::anyhow!("No file is selected"))?;
        let (start, end) = self
            .selected_range()
            .unwrap_or((self.position(), self.position()));
        let first = self
            .current_lines()
            .get(start)
            .ok_or_else(|| anyhow::anyhow!("Select a numbered diff line to comment"))?;
        let last = self
            .current_lines()
            .get(end)
            .ok_or_else(|| anyhow::anyhow!("Select a numbered diff line to comment"))?;
        let selected = &self.current_lines()[start..=end];
        if selected.iter().any(|line| line.anchor().is_none()) {
            bail!("Select numbered source lines within a single complete hunk");
        }
        let deleted = selected.iter().any(|line| line.kind == LineKind::Deletion);
        let added = selected.iter().any(|line| line.kind == LineKind::Addition);
        if deleted && added {
            bail!("A comment range cannot combine removed and added lines; select one diff side");
        }
        let common = if deleted { Side::Left } else { Side::Right };
        let coordinate = |line: &DiffLine| {
            if common == Side::Left {
                line.old_line
            } else {
                line.new_line
            }
        };
        let start_line = coordinate(first)
            .ok_or_else(|| anyhow::anyhow!("Select numbered lines on the same diff side"))?;
        let line = coordinate(last)
            .ok_or_else(|| anyhow::anyhow!("Select numbered lines on the same diff side"))?;
        let multiline = start != end;
        let draft = DraftComment {
            path: file.filename.clone(),
            line,
            side: common.clone(),
            start_line: multiline.then_some(start_line),
            start_side: multiline.then_some(common),
            body: String::new(),
        };
        if !local_note && !diff::validate_comment(file, &draft) {
            bail!(
                "GitHub comments must stay inside one original diff hunk; use Q to ask about expanded context"
            );
        }
        Ok(draft)
    }

    pub fn editor_target(&self) -> Result<Box<EditorTarget>> {
        let file = self
            .current_file()
            .ok_or_else(|| anyhow::anyhow!("No file is selected"))?;
        let (side, line) = self
            .anchor()
            .or_else(|| {
                self.current_lines()
                    .iter()
                    .skip(self.position())
                    .find_map(DiffLine::anchor)
            })
            .unwrap_or((
                if file.status == "removed" {
                    Side::Left
                } else {
                    Side::Right
                },
                1,
            ));
        Ok(Box::new(EditorTarget {
            file: file.clone(),
            side,
            line,
        }))
    }

    fn move_line(&mut self, delta: isize) {
        if self.positions.is_empty() {
            return;
        }
        if self.selection.is_some_and(|selection| selection.extending) {
            let max = self.current_lines().len().saturating_sub(1);
            self.positions[self.selected_file] =
                self.position().saturating_add_signed(delta).min(max);
            return;
        }
        self.selection = None;
        let max = self.current_lines().len().saturating_sub(1);
        self.positions[self.selected_file] = self.position().saturating_add_signed(delta).min(max);
    }

    fn move_file(&mut self, delta: isize) {
        self.selection = None;
        self.selected_file = self
            .selected_file
            .saturating_add_signed(delta)
            .min(self.data.files.len().saturating_sub(1));
        self.horizontal = 0;
    }

    fn hunk_positions(&self, file: usize) -> Vec<usize> {
        if let Some(view) = &self.contexts[file].view {
            return view.hunk_positions.clone();
        }
        self.lines[file]
            .iter()
            .enumerate()
            .filter_map(|(index, row)| (row.kind == LineKind::Hunk).then_some(index))
            .collect()
    }

    fn hunk_target(&self, forward: bool) -> Option<(usize, usize)> {
        if self.selected_file >= self.lines.len() {
            return None;
        }
        let positions = self.hunk_positions(self.selected_file);
        let same_file = if forward {
            positions
                .into_iter()
                .find(|position| *position > self.position())
        } else {
            positions
                .into_iter()
                .rev()
                .find(|position| *position < self.position())
        };
        if let Some(position) = same_file {
            return Some((self.selected_file, position));
        }
        if self.selection.is_some_and(|selection| selection.extending) {
            return None;
        }
        if forward {
            (self.selected_file + 1..self.lines.len()).find_map(|file| {
                self.hunk_positions(file)
                    .first()
                    .map(|&position| (file, position))
            })
        } else {
            (0..self.selected_file).rev().find_map(|file| {
                self.hunk_positions(file)
                    .last()
                    .map(|&position| (file, position))
            })
        }
    }

    fn hunk(&mut self, forward: bool) {
        if let Some((file, position)) = self.hunk_target(forward) {
            if file != self.selected_file {
                self.selection = None;
                self.horizontal = 0;
            }
            self.selected_file = file;
            self.positions[file] = position;
        }
        self.focus = Focus::Diff;
        if self.selection.is_some_and(|selection| !selection.extending) {
            self.selection = None;
        }
    }
}

pub enum Action {
    RefreshLocal,
    Browse(PrId),
    LoadContext(usize),
    PreviewGuide(PrId),
    Ask {
        question: String,
        focus: Option<Box<assistant::Focus>>,
    },
    CancelAsk,
    None,
    Quit,
    LoadInbox,
    LoadPr(PrId),
    LoadDiff,
    Explain {
        force: bool,
    },
    DisableAi,
    Activity(PrId),
    Prepare {
        pr: PullRequest,
        open_editor: Option<Box<EditorTarget>>,
    },
    OpenEditor(Box<EditorTarget>),
    Submit {
        data: Box<ReviewData>,
        drafts: Vec<DraftComment>,
        body: String,
        event: ReviewEvent,
    },
}

pub struct App {
    /// Action requested outside the key loop, such as an agent asking to reload.
    pub requested: Option<Action>,
    /// Local checkout root and the GitHub repository it belongs to.
    pub checkout: Option<(PathBuf, String)>,
    pub file_sidebar: bool,
    pub local_spec: Option<local::Spec>,
    pub local_contents: std::collections::BTreeMap<String, FileContext>,
    pub preview: PreviewState,
    pub chat: Chat,
    pub open_on_diff: bool,
    pub tab: PageTab,
    pub overview: Option<OverviewState>,
    pub diff_loading: bool,
    pub diff_error: Option<String>,
    pub comments_loading: bool,
    pub generation: u64,
    pub settings: Settings,
    pub config_path: Option<PathBuf>,
    pub goto_pending: bool,
    pub pending_keys: Vec<Stroke>,
    pub repo: Option<String>,
    pub inbox: Vec<PrSummary>,
    pub selected_pr: usize,
    pub review: Option<ReviewState>,
    pub dialog: Option<Dialog>,
    pub status: String,
    pub error: bool,
    pub busy: bool,
    pub root: PathBuf,
    pub filter: String,
    pub filtering: bool,
    pub demo: bool,
}

impl App {
    pub fn new(root: PathBuf, demo: bool) -> Self {
        Self {
            requested: None,
            checkout: None,
            file_sidebar: true,
            local_spec: None,
            local_contents: Default::default(),
            preview: PreviewState::default(),
            chat: Chat::default(),
            open_on_diff: false,
            tab: PageTab::Overview,
            overview: None,
            diff_loading: false,
            diff_error: None,
            comments_loading: false,
            generation: 0,
            settings: Settings::default(),
            config_path: None,
            goto_pending: false,
            pending_keys: Vec::new(),
            repo: None,
            inbox: Vec::new(),
            selected_pr: 0,
            review: None,
            dialog: None,
            status: "Ready".into(),
            error: false,
            busy: false,
            root,
            filter: String::new(),
            filtering: false,
            demo,
        }
    }

    pub fn message(&mut self, text: impl Into<String>, error: bool) {
        self.status = text.into();
        self.error = error;
    }

    pub fn opened(&mut self, metadata: PrOverview) {
        self.repo = Some(metadata.id.repo.clone());
        self.overview = Some(OverviewState {
            metadata,
            activity: ActivityStatus::Loading,
            scroll: 0,
            guide_scroll: 0,
            selected_step: 0,
            focus_guide: false,
            ai: if self.settings.ai.provider == Provider::None {
                AiStatus::Disabled
            } else {
                AiStatus::Waiting
            },
        });
        self.review = None;
        self.tab = if self.open_on_diff {
            PageTab::Diff
        } else {
            PageTab::Overview
        };
        self.chat = Chat::default();
        self.diff_loading = true;
        self.diff_error = None;
        self.goto_pending = false;
        self.pending_keys.clear();
        self.message(
            "Overview ready. The diff is loading in the background.",
            false,
        );
    }

    pub fn accept_diff(&mut self, data: ReviewData) -> Result<()> {
        let tab = self.tab;
        self.loaded(data)?;
        self.tab = tab;
        self.diff_loading = false;
        self.diff_error = None;
        if !self.error {
            self.message(
                "Diff ready. Use the command palette for review actions.",
                false,
            );
        }
        Ok(())
    }

    fn leave_pr(&mut self) {
        if let Some(overview) = self.overview.take() {
            let status = if matches!(overview.ai, AiStatus::Loading) {
                AiStatus::Waiting
            } else {
                overview.ai
            };
            self.preview.guides.insert(
                preview_key(&overview.metadata.id),
                PreviewGuide {
                    revision: overview.metadata.head_sha,
                    status,
                },
            );
        }
        self.open_on_diff = false;
        self.chat = Chat::default();
        self.review = None;
        self.diff_loading = false;
        self.diff_error = None;
        self.comments_loading = false;
        self.generation = self.generation.wrapping_add(1);
        self.goto_pending = false;
        self.pending_keys.clear();
        self.message("Back to PR inbox", false);
    }

    fn jump_to_reading_step(&mut self) -> Result<()> {
        let Some(overview) = &self.overview else {
            return Ok(());
        };
        let AiStatus::Ready(analysis) = &overview.ai else {
            return Ok(());
        };
        let Some(step) = analysis.reading_order.get(overview.selected_step) else {
            return Ok(());
        };
        let review = self
            .review
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("The diff is not ready yet"))?;
        let index = review
            .data
            .files
            .iter()
            .position(|file| file.filename == step.path)
            .ok_or_else(|| {
                anyhow::anyhow!("The reading guide refers to a file outside this diff")
            })?;
        let position = step
            .line
            .and_then(|line| {
                review.lines[index]
                    .iter()
                    .position(|row| prpr::thread::matches_anchor(row, &step.side, line))
            })
            .unwrap_or(0);
        review.selected_file = index;
        review.positions[index] = position;
        review.selection = None;
        review.horizontal = 0;
        review.focus = Focus::Diff;
        self.tab = PageTab::Diff;
        Ok(())
    }

    pub fn loaded(&mut self, mut data: ReviewData) -> Result<()> {
        data.files
            .sort_by(|left, right| left.filename.cmp(&right.filename));
        let drafts = if data.source.is_local() {
            Vec::new()
        } else {
            store::load(&self.root, &data.pr)?
        };
        let notes = notes::load(&self.root, &data)?;
        for draft in &drafts {
            if !data.files.iter().any(|f| diff::validate_comment(f, draft)) {
                bail!(
                    "Saved draft no longer matches this diff: {}:{}",
                    draft.path,
                    draft.line
                );
            }
        }
        let positions = vec![0; data.files.len()];
        let contexts = (0..data.files.len())
            .map(|_| context::FileState::default())
            .collect();
        let lines = data
            .files
            .iter()
            .map(|f| {
                f.patch
                    .as_deref()
                    .map(diff::parse_patch)
                    .unwrap_or_default()
            })
            .collect();
        if self.repo.is_none() {
            self.repo = Some(data.pr.id.repo.clone());
        }
        self.review = Some(ReviewState {
            notes,
            contexts,
            data,
            lines,
            selected_file: 0,
            positions,
            focus: Focus::Diff,
            horizontal: 0,
            show_comments: self
                .review
                .as_ref()
                .is_none_or(|review| review.show_comments),
            selection: None,
            drafts,
            workspace: None,
            viewport: Viewport::default(),
            marks: Vec::new(),
        });
        self.tab = PageTab::Diff;
        self.goto_pending = false;
        self.pending_keys.clear();
        let review = self.review.as_ref().unwrap();
        let pending = !review.data.source.is_local()
            && store::attempt(&self.root, &review.data.pr)?.is_some();
        if pending {
            self.message("An earlier submission has an unknown result. Inspect published reviews and recover it from the command palette.", true);
        } else {
            self.message(
                "Review loaded. The command palette lists available actions.",
                false,
            );
        }
        Ok(())
    }

    pub fn add_note(
        &mut self,
        anchor: DraftComment,
        reply_to: Option<String>,
        author: String,
    ) -> Result<String> {
        let review = self
            .review
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Open a review first"))?;
        if reply_to.is_none() {
            review.validate_note(&anchor)?;
        }
        let mut notes = review.notes.clone();
        let id = notes::add(&mut notes, &review.data, anchor, reply_to, author)?;
        notes::update(&self.root, &review.data, &review.notes, &notes)?;
        review.notes = notes;
        Ok(id)
    }

    pub fn navigate(&mut self, path: &str, side: &Side, line: u32) -> Result<()> {
        let review = self
            .review
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Open a review first"))?;
        let index = review
            .data
            .files
            .iter()
            .position(|file| file.filename == path)
            .ok_or_else(|| anyhow::anyhow!("File is outside this review"))?;
        let position = review.lines[index]
            .iter()
            .position(|row| prpr::thread::matches_anchor(row, side, line))
            .ok_or_else(|| {
                anyhow::anyhow!("Line is outside the displayed context; expand it first")
            })?;
        review.selected_file = index;
        review.positions[index] = position;
        review.focus = Focus::Diff;
        review.selection = None;
        self.tab = PageTab::Diff;
        self.goto_pending = false;
        self.pending_keys.clear();
        Ok(())
    }

    pub fn paste(&mut self, text: &str) {
        let mut text = text.to_owned();
        let input = match &mut self.dialog {
            Some(Dialog::Palette { input, selected }) => {
                *selected = 0;
                text = text.replace(['\r', '\n'], " ");
                input
            }
            Some(
                Dialog::Note { input, .. }
                | Dialog::Compose { input, .. }
                | Dialog::Submit { input, .. }
                | Dialog::OpenPr { input }
                | Dialog::Ask { input, .. },
            ) => input,
            _ => return,
        };
        input.text.insert_str(input.cursor, &text);
        input.cursor += text.len();
    }

    pub fn visible_prs(&self) -> Vec<&PrSummary> {
        let filter = self.filter.to_lowercase();
        self.inbox
            .iter()
            .filter(|pr| {
                format!("{} {} {} {}", pr.id.repo, pr.id.number, pr.title, pr.author)
                    .to_lowercase()
                    .contains(&filter)
            })
            .collect()
    }

    pub fn binding_view(&self) -> View {
        if self.overview.is_some() && self.tab == PageTab::Overview {
            View::Overview
        } else if self.overview.is_some() && self.tab == PageTab::Guide {
            View::Guide
        } else if self.review.is_some() || self.local_spec.is_some() || self.overview.is_some() {
            View::Diff
        } else {
            View::Inbox
        }
    }

    pub fn shortcut(&self, id: &str) -> String {
        self.settings.keybindings.display(id)
    }

    pub fn execute_binding(&mut self, id: &str) -> Result<Action> {
        let binding =
            keybindings::spec(id).ok_or_else(|| anyhow::anyhow!("Unknown shortcut action {id}"))?;
        self.goto_pending = false;
        self.pending_keys.clear();
        for key in binding.canonical() {
            let action = self.default_key(key)?;
            if !matches!(action, Action::None) {
                return Ok(action);
            }
        }
        Ok(Action::None)
    }

    pub fn key(&mut self, key: KeyEvent) -> Result<Action> {
        if Stroke::from_event(key)
            == Stroke::from_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        {
            return Ok(Action::Quit);
        }
        if self.dialog.is_some() {
            if matches!(self.dialog, Some(Dialog::Palette { .. }))
                && !Stroke::from_event(key).is_text()
                && !Stroke::from_event(key).is_palette_control()
                && self
                    .settings
                    .keybindings
                    .single_matches("command_palette", key)
            {
                self.dialog = None;
                return Ok(Action::None);
            }
            return self.dialog_key(key);
        }
        if self.filtering {
            if Stroke::from_event(key).modified()
                && self
                    .settings
                    .keybindings
                    .single_matches("command_palette", key)
            {
                return self.execute_binding("command_palette");
            }
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.filtering = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.selected_pr = 0;
                    self.preview.scroll = 0;
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.filter.push(character);
                    self.selected_pr = 0;
                    self.preview.scroll = 0;
                }
                _ => {}
            }
            return Ok(Action::None);
        }
        if !self.pending_keys.is_empty() && Stroke::from_event(key).is_escape() {
            self.pending_keys.clear();
            self.goto_pending = false;
            return Ok(Action::None);
        }
        let had_prefix = !self.pending_keys.is_empty();
        self.pending_keys.push(Stroke::from_event(key));
        match self
            .settings
            .keybindings
            .resolve(self.binding_view(), &self.pending_keys)?
        {
            keybindings::Match::Action(id) => self.execute_binding(id),
            keybindings::Match::Prefix => {
                self.goto_pending = true;
                Ok(Action::None)
            }
            keybindings::Match::None => {
                self.goto_pending = false;
                self.pending_keys.clear();
                if had_prefix {
                    let review_open = self.overview.is_some();
                    for (id, enabled) in [
                        ("command_palette", true),
                        ("toggle_sidebar", true),
                        ("open_github", true),
                        ("ask", true),
                        ("explain_diff", true),
                        ("overview", review_open),
                        ("guide", review_open),
                        ("diff", review_open),
                        ("next_view", review_open),
                        ("previous_view", review_open),
                        ("choose_agent", review_open),
                        ("ai_guide", review_open),
                    ] {
                        if enabled
                            && keybindings::spec(id).unwrap().applies(self.binding_view())
                            && self.settings.keybindings.single_matches(id, key)
                        {
                            return self.execute_binding(id);
                        }
                    }
                }
                Ok(Action::None)
            }
        }
    }

    fn default_key(&mut self, key: KeyEvent) -> Result<Action> {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            match key.code {
                KeyCode::Char('k') => {
                    self.filtering = false;
                    self.goto_pending = false;
                    self.pending_keys.clear();
                    self.dialog = Some(Dialog::Palette {
                        input: TextInput::default(),
                        selected: 0,
                    });
                    return Ok(Action::None);
                }
                KeyCode::Char('b') => {
                    self.goto_pending = false;
                    self.pending_keys.clear();
                    if let Some(review) = &mut self.review {
                        self.file_sidebar = !self.file_sidebar;
                        if !self.file_sidebar {
                            review.focus = Focus::Diff;
                        }
                    }
                    return Ok(Action::None);
                }
                _ => {}
            }
        }
        if self.local_spec.is_some() {
            match key.code {
                KeyCode::Char('r') if !self.busy => return Ok(Action::RefreshLocal),
                KeyCode::Esc
                    if self
                        .review
                        .as_ref()
                        .is_none_or(|review| review.selection.is_none()) =>
                {
                    return Ok(Action::Quit);
                }
                KeyCode::Char('B' | '#' | 'V' | 'u' | 's' | 'w')
                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.message("Local review. The command palette offers notes, AI questions, explanations, and refresh.", false);
                    return Ok(Action::None);
                }
                _ => {}
            }
        }
        if key.code == KeyCode::Char('B')
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            let id = self
                .overview
                .as_ref()
                .map(|state| state.metadata.id.clone())
                .or_else(|| self.review.as_ref().map(|state| state.data.pr.id.clone()))
                .or_else(|| {
                    if self.busy {
                        None
                    } else {
                        self.visible_prs()
                            .get(self.selected_pr)
                            .map(|pr| pr.id.clone())
                    }
                });
            return Ok(id.map_or(Action::None, Action::Browse));
        }
        if matches!(key.code, KeyCode::Char('Q') | KeyCode::Char('E'))
            && let Some(review) = &self.review
        {
            if self.settings.ai.provider == Provider::None {
                bail!("Choose Codex or Claude in the command palette before asking");
            }
            let focus = if self.tab == PageTab::Diff || self.overview.is_none() {
                Some(review.question_focus()?)
            } else {
                None
            };
            self.goto_pending = false;
            self.pending_keys.clear();
            self.dialog = Some(Dialog::Ask {
                input: TextInput::default(),
                focus: focus.clone(),
                scroll: u16::MAX,
            });
            if key.code == KeyCode::Char('E') {
                return Ok(Action::Ask { question: "Explain this diff: what changes, why it may be needed, how it works, and what I should check during review. Distinguish evidence from inference.".into(), focus });
            }
            return Ok(Action::None);
        }
        if self.overview.is_some() {
            match key.code {
                KeyCode::Char('1') => {
                    self.tab = PageTab::Overview;
                    self.goto_pending = false;
                    self.pending_keys.clear();
                    return Ok(Action::None);
                }
                KeyCode::Char('2') => {
                    self.tab = PageTab::Guide;
                    self.goto_pending = false;
                    self.pending_keys.clear();
                    return Ok(Action::None);
                }
                KeyCode::Char('3') => {
                    self.tab = PageTab::Diff;
                    self.goto_pending = false;
                    self.pending_keys.clear();
                    return Ok(Action::None);
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    self.tab = self.tab.cycle(key.code == KeyCode::Tab);
                    self.goto_pending = false;
                    self.pending_keys.clear();
                    return Ok(Action::None);
                }
                KeyCode::Char('A') => {
                    self.goto_pending = false;
                    self.pending_keys.clear();
                    self.dialog = Some(Dialog::AgentPicker {
                        selected: match self.settings.ai.provider {
                            Provider::Codex => 0,
                            Provider::Claude => 1,
                            Provider::None => 2,
                        },
                    });
                    return Ok(Action::None);
                }
                KeyCode::Char('a') => {
                    self.goto_pending = false;
                    self.pending_keys.clear();
                    if self
                        .overview
                        .as_ref()
                        .is_some_and(|overview| matches!(overview.ai, AiStatus::Loading))
                    {
                        self.message("The AI guide is already being prepared", false);
                        return Ok(Action::None);
                    }
                    if self.settings.ai.provider == Provider::None {
                        self.dialog = Some(Dialog::AgentPicker { selected: 0 });
                        return Ok(Action::None);
                    }
                    return Ok(Action::Explain { force: true });
                }
                _ => {}
            }
        }
        if self.overview.is_some() && self.tab != PageTab::Diff {
            return self.overview_key(key);
        }
        if self.overview.is_some() && self.review.is_none() {
            match key.code {
                KeyCode::Esc if !self.busy => self.leave_pr(),
                KeyCode::Char('r') if !self.diff_loading => return Ok(Action::LoadDiff),
                KeyCode::Char('q') if !self.busy => return Ok(Action::Quit),
                KeyCode::Char('?') => self.dialog = Some(Dialog::Help { scroll: 0 }),
                _ => {}
            }
            return Ok(Action::None);
        }
        if self.goto_pending {
            self.goto_pending = false;
            self.pending_keys.clear();
            if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            {
                return Ok(Action::None);
            }
            match key.code {
                KeyCode::Char('f') => return self.open_selected(false),
                KeyCode::Char('g') | KeyCode::Char('e') => {
                    let end = key.code == KeyCode::Char('e');
                    if let Some(review) = &mut self.review {
                        if review.focus == Focus::Files {
                            review.selection = None;
                            review.selected_file = if end {
                                review.data.files.len().saturating_sub(1)
                            } else {
                                0
                            };
                        } else {
                            review.goto_line(if end { usize::MAX } else { 0 });
                        }
                    } else if self.preview.focused {
                        self.preview.scroll = if end { u16::MAX } else { 0 };
                    } else {
                        self.preview.scroll = 0;
                        self.selected_pr = if end {
                            self.visible_prs().len().saturating_sub(1)
                        } else {
                            0
                        };
                    }
                }
                _ => {}
            }
            return Ok(Action::None);
        }
        if key.code == KeyCode::Char('g')
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.goto_pending = true;
            return Ok(Action::None);
        }
        if key.code == KeyCode::Char('?') {
            self.dialog = Some(Dialog::Help { scroll: 0 });
            return Ok(Action::None);
        }
        if key.code == KeyCode::Char('!') {
            self.dialog = Some(Dialog::Status { scroll: 0 });
            return Ok(Action::None);
        }
        if key.code == KeyCode::Char('#') && !self.busy {
            self.dialog = Some(Dialog::OpenPr {
                input: TextInput::default(),
            });
            return Ok(Action::None);
        }
        if key.code == KeyCode::Char('q') {
            if self.busy {
                self.message(
                    "An operation is running; wait for it to finish before quitting.",
                    true,
                );
            } else if self.review.as_ref().is_some_and(|r| !r.drafts.is_empty()) {
                self.dialog = Some(Dialog::Quit);
            } else {
                return Ok(Action::Quit);
            }
            return Ok(Action::None);
        }
        if self.review.is_none() {
            return self.inbox_key(key);
        }
        let review = self.review.as_mut().unwrap();
        if let KeyCode::Char(command @ ('+' | '-' | 'F')) = key.code {
            return Ok(review
                .change_context(command)?
                .map_or(Action::None, Action::LoadContext));
        }
        match key.code {
            KeyCode::Esc if review.selection.is_some() => {
                review.selection = None;
            }
            KeyCode::Esc if !self.busy => {
                self.leave_pr();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                review.focus = if review.focus == Focus::Files {
                    Focus::Diff
                } else {
                    self.file_sidebar = true;
                    Focus::Files
                }
            }
            KeyCode::Char('h') => {
                self.file_sidebar = true;
                review.focus = Focus::Files;
            }
            KeyCode::Char('l') | KeyCode::Enter => review.focus = Focus::Diff,
            KeyCode::Char('j') | KeyCode::Down => {
                if review.focus == Focus::Files {
                    review.move_file(1);
                } else {
                    review.move_line(1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if review.focus == Focus::Files {
                    review.move_file(-1);
                } else {
                    review.move_line(-1);
                }
            }
            KeyCode::Char('n') => review.move_file(1),
            KeyCode::Char('p') => review.move_file(-1),
            KeyCode::Char(']') => review.hunk(true),
            KeyCode::Char('[') => review.hunk(false),
            KeyCode::Home => {
                if review.focus == Focus::Files {
                    review.selection = None;
                    review.selected_file = 0;
                } else {
                    review.goto_line(0);
                }
            }
            KeyCode::Char('G') | KeyCode::End => {
                if review.focus == Focus::Files {
                    review.selection = None;
                    review.selected_file = review.data.files.len().saturating_sub(1);
                } else {
                    review.goto_line(usize::MAX);
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                review.viewport.keep_offset.set(true);
                review.move_line(15)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                review.viewport.keep_offset.set(true);
                review.move_line(-15)
            }
            KeyCode::PageDown => {
                review.viewport.keep_offset.set(true);
                review.move_line(20)
            }
            KeyCode::PageUp => {
                review.viewport.keep_offset.set(true);
                review.move_line(-20)
            }
            KeyCode::Right => review.horizontal = review.horizontal.saturating_add(8),
            KeyCode::Left => review.horizontal = review.horizontal.saturating_sub(8),
            KeyCode::Char('b') => self.dialog = Some(Dialog::Description { scroll: 0 }),
            KeyCode::Char('t') => review.show_comments = !review.show_comments,
            KeyCode::Char('T') => self.dialog = Some(Dialog::Threads { scroll: 0 }),
            KeyCode::Char('v') => review.toggle_selection()?,
            KeyCode::Char('x') => review.select_line(true)?,
            KeyCode::Char('X') => review.select_line(false)?,
            KeyCode::Char(';') => review.selection = None,
            KeyCode::Char('V') if !self.busy => {
                return Ok(Action::Activity(review.data.pr.id.clone()));
            }
            KeyCode::Char('u') if !self.busy => {
                if let Some(attempt) = store::attempt(&self.root, &review.data.pr)? {
                    self.dialog = Some(Dialog::Recovery { attempt });
                } else {
                    self.message("There is no unresolved submission for this PR", false);
                }
            }
            KeyCode::Char('N') => self.dialog = Some(Dialog::Notes { selected: 0 }),
            KeyCode::Char('D') if review.data.source.is_local() => {
                self.dialog = Some(Dialog::Notes { selected: 0 })
            }
            KeyCode::Char('D') => self.dialog = Some(Dialog::Drafts { selected: 0 }),
            KeyCode::Char('C') if !self.busy => {
                let anchor = review.comment_target(true)?;
                self.dialog = Some(Dialog::Note {
                    anchor,
                    reply_to: None,
                    input: TextInput::default(),
                });
            }
            KeyCode::Char('c') if !self.busy && review.data.source.is_local() => {
                let anchor = review.comment_target(true)?;
                self.dialog = Some(Dialog::Note {
                    anchor,
                    reply_to: None,
                    input: TextInput::default(),
                });
            }
            KeyCode::Char('c') if !self.busy => {
                let draft = review.comment_target(false)?;
                self.dialog = Some(Dialog::Compose {
                    draft,
                    editing: None,
                    input: TextInput::default(),
                });
            }
            KeyCode::Char('w') | KeyCode::Char('e') if !self.busy => {
                return self.open_selected(key.code == KeyCode::Char('w'));
            }
            KeyCode::Char('s') if !self.busy => {
                if store::attempt(&self.root, &review.data.pr)?.is_some() {
                    self.message("Inspect published reviews, then recover the previous submission from the command palette before retrying.", true);
                } else {
                    self.dialog = Some(Dialog::Submit {
                        event: ReviewEvent::Comment,
                        input: TextInput::default(),
                    });
                }
            }
            KeyCode::Char('r') if !self.busy => {
                return Ok(Action::LoadPr(review.data.pr.id.clone()));
            }
            _ => {}
        }
        Ok(Action::None)
    }

    fn inbox_key(&mut self, key: KeyEvent) -> Result<Action> {
        let len = self.visible_prs().len();
        let old = self.selected_pr;
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.preview.shown = true;
                self.preview.focused = !self.preview.focused;
            }
            KeyCode::Char('h') | KeyCode::Left => self.preview.focused = false,
            KeyCode::Char('l') | KeyCode::Right => {
                self.preview.shown = true;
                self.preview.focused = true;
            }
            KeyCode::Char('p') => {
                self.preview.shown = !self.preview.shown;
                if !self.preview.shown {
                    self.preview.focused = false;
                }
            }
            KeyCode::Char('P') => self.preview.bottom = Some(!self.preview.actual_bottom),
            KeyCode::Char('[') | KeyCode::Char(']') | KeyCode::Char('1') | KeyCode::Char('2') => {
                self.preview.shown = true;
                self.preview.tab = match key.code {
                    KeyCode::Char('1') => PreviewTab::Description,
                    KeyCode::Char('2') => PreviewTab::Guide,
                    _ => {
                        if self.preview.tab == PreviewTab::Description {
                            PreviewTab::Guide
                        } else {
                            PreviewTab::Description
                        }
                    }
                };
                self.preview.scroll = 0;
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.preview.scroll = self.preview.scroll.saturating_add(12)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.preview.scroll = self.preview.scroll.saturating_sub(12)
            }
            KeyCode::PageDown => self.preview.scroll = self.preview.scroll.saturating_add(15),
            KeyCode::PageUp => self.preview.scroll = self.preview.scroll.saturating_sub(15),
            KeyCode::Char('j') | KeyCode::Down if self.preview.focused => {
                self.preview.scroll = self.preview.scroll.saturating_add(1)
            }
            KeyCode::Char('k') | KeyCode::Up if self.preview.focused => {
                self.preview.scroll = self.preview.scroll.saturating_sub(1)
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected_pr = (self.selected_pr + 1).min(len.saturating_sub(1))
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected_pr = self.selected_pr.saturating_sub(1)
            }
            KeyCode::Home if self.preview.focused => self.preview.scroll = 0,
            KeyCode::End | KeyCode::Char('G') if self.preview.focused => {
                self.preview.scroll = u16::MAX
            }
            KeyCode::Home => self.selected_pr = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected_pr = len.saturating_sub(1),
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Char('A') => {
                self.dialog = Some(Dialog::AgentPicker {
                    selected: match self.settings.ai.provider {
                        Provider::Codex => 0,
                        Provider::Claude => 1,
                        Provider::None => 2,
                    },
                })
            }
            KeyCode::Char('a') => {
                if self.settings.ai.provider == Provider::None {
                    self.dialog = Some(Dialog::AgentPicker { selected: 0 });
                } else if let Some(pr) = self.visible_prs().get(self.selected_pr) {
                    let id = pr.id.clone();
                    self.preview.tab = PreviewTab::Guide;
                    self.preview.shown = true;
                    return Ok(Action::PreviewGuide(id));
                }
            }
            KeyCode::Char('r') if !self.busy => {
                self.preview.guides.clear();
                return Ok(Action::LoadInbox);
            }
            KeyCode::Enter | KeyCode::Char('o') if !self.busy => {
                if let Some(pr) = self.visible_prs().get(self.selected_pr) {
                    let id = pr.id.clone();
                    self.open_on_diff = true;
                    return Ok(Action::LoadPr(id));
                }
            }
            _ => {}
        }
        if old != self.selected_pr {
            self.preview.scroll = 0;
        }
        Ok(Action::None)
    }

    fn open_selected(&mut self, prepare_only: bool) -> Result<Action> {
        if self.busy {
            return Ok(Action::None);
        }
        if let Some(review) = &self.review {
            if self.demo {
                self.message(
                    "Demo is offline. Workspace and Helix actions require a real PR.",
                    true,
                );
                return Ok(Action::None);
            }
            if review.data.source.is_local() {
                return Ok(Action::OpenEditor(review.editor_target()?));
            }
            if !prepare_only
                && (review.workspace.is_some()
                    || self.settings.editor.open_in == OpenIn::Repository)
            {
                return Ok(Action::OpenEditor(review.editor_target()?));
            }
            return Ok(Action::Prepare {
                pr: review.data.pr.clone(),
                open_editor: if prepare_only {
                    None
                } else {
                    Some(review.editor_target()?)
                },
            });
        }
        Ok(self
            .visible_prs()
            .get(self.selected_pr)
            .map_or(Action::None, |pr| Action::LoadPr(pr.id.clone())))
    }

    fn overview_key(&mut self, key: KeyEvent) -> Result<Action> {
        let guide = self.tab == PageTab::Guide;
        if self.goto_pending {
            self.goto_pending = false;
            self.pending_keys.clear();
            let overview = self.overview.as_mut().unwrap();
            let scroll = if guide {
                &mut overview.guide_scroll
            } else {
                &mut overview.scroll
            };
            match key.code {
                KeyCode::Char('g') => *scroll = 0,
                KeyCode::Char('e') => *scroll = u16::MAX,
                KeyCode::Char('f') if guide => self.jump_to_reading_step()?,
                _ => {}
            }
            if matches!(key.code, KeyCode::Char('g' | 'e')) {
                self.overview.as_mut().unwrap().focus_guide = false;
            }
            return Ok(Action::None);
        }
        let overview = self.overview.as_mut().unwrap();
        let scroll = if guide {
            &mut overview.guide_scroll
        } else {
            &mut overview.scroll
        };
        let moved = match key.code {
            KeyCode::Char('j') | KeyCode::Down => Some(scroll.saturating_add(1)),
            KeyCode::Char('k') | KeyCode::Up => Some(scroll.saturating_sub(1)),
            KeyCode::PageDown => Some(scroll.saturating_add(15)),
            KeyCode::PageUp => Some(scroll.saturating_sub(15)),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(scroll.saturating_add(12))
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(scroll.saturating_sub(12))
            }
            KeyCode::Home => Some(0),
            KeyCode::End | KeyCode::Char('G') => Some(u16::MAX),
            _ => None,
        };
        if let Some(position) = moved {
            *scroll = position;
            overview.focus_guide = false;
            return Ok(Action::None);
        }
        match key.code {
            KeyCode::Char('g') => self.goto_pending = true,
            KeyCode::Char('n') | KeyCode::Char('p') if guide => {
                let len = match &overview.ai {
                    AiStatus::Ready(analysis) => analysis.reading_order.len(),
                    _ => 0,
                };
                overview.selected_step = if key.code == KeyCode::Char('n') {
                    overview
                        .selected_step
                        .saturating_add(1)
                        .min(len.saturating_sub(1))
                } else {
                    overview.selected_step.saturating_sub(1)
                };
                overview.focus_guide = true;
            }
            KeyCode::Enter | KeyCode::Char('o') if guide => self.jump_to_reading_step()?,
            KeyCode::Char('V') if !self.busy && self.local_spec.is_none() => {
                return Ok(Action::Activity(overview.metadata.id.clone()));
            }
            KeyCode::Char('b') => self.dialog = Some(Dialog::Description { scroll: 0 }),
            KeyCode::Char('?') => self.dialog = Some(Dialog::Help { scroll: 0 }),
            KeyCode::Char('!') => self.dialog = Some(Dialog::Status { scroll: 0 }),
            KeyCode::Char('#') if !self.busy => {
                self.dialog = Some(Dialog::OpenPr {
                    input: TextInput::default(),
                })
            }
            KeyCode::Char('r') if !self.busy => {
                return Ok(Action::LoadPr(overview.metadata.id.clone()));
            }
            KeyCode::Esc if !self.busy => self.leave_pr(),
            KeyCode::Char('q') if !self.busy => {
                if self
                    .review
                    .as_ref()
                    .is_some_and(|review| !review.drafts.is_empty())
                {
                    self.dialog = Some(Dialog::Quit);
                } else {
                    return Ok(Action::Quit);
                }
            }
            _ => {}
        }
        Ok(Action::None)
    }

    fn dialog_key(&mut self, key: KeyEvent) -> Result<Action> {
        let dialog = self.dialog.take().unwrap();
        if key.code == KeyCode::Esc {
            if matches!(dialog, Dialog::Ask { .. }) && self.chat.pending.is_some() {
                self.chat.pending = None;
                return Ok(Action::CancelAsk);
            }
            return Ok(Action::None);
        }
        let save = key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL);
        self.dialog = match dialog {
            Dialog::Palette {
                mut input,
                mut selected,
            } => {
                let commands = commands::available(self, &input.text);
                selected = selected.min(commands.len().saturating_sub(1));
                match key.code {
                    KeyCode::Enter => {
                        if let Some(command) = commands.get(selected)
                            && command.disabled.is_none()
                        {
                            return self.execute_binding(command.id);
                        }
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        selected = (selected + 1).min(commands.len().saturating_sub(1))
                    }
                    KeyCode::Up | KeyCode::BackTab => selected = selected.saturating_sub(1),
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        selected = (selected + 1).min(commands.len().saturating_sub(1))
                    }
                    KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        selected = selected.saturating_sub(1)
                    }
                    KeyCode::PageDown => {
                        selected = selected
                            .saturating_add(10)
                            .min(commands.len().saturating_sub(1))
                    }
                    KeyCode::PageUp => selected = selected.saturating_sub(10),
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        input = TextInput::default();
                        selected = 0;
                    }
                    _ => {
                        let previous = input.text.clone();
                        input.key(key);
                        if previous != input.text {
                            selected = 0;
                        }
                    }
                }
                Some(Dialog::Palette { input, selected })
            }
            Dialog::Ask {
                mut input,
                focus,
                mut scroll,
            } => {
                if save && self.chat.pending.is_none() && !input.text.trim().is_empty() {
                    let question = input.text.clone();
                    self.dialog = Some(Dialog::Ask {
                        input: TextInput::default(),
                        focus: focus.clone(),
                        scroll: u16::MAX,
                    });
                    return Ok(Action::Ask { question, focus });
                }
                match key.code {
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        scroll = scroll.saturating_add(12)
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        scroll = scroll.saturating_sub(12)
                    }
                    KeyCode::PageDown => scroll = scroll.saturating_add(15),
                    KeyCode::PageUp => scroll = scroll.saturating_sub(15),
                    _ => input.key(key),
                }
                Some(Dialog::Ask {
                    input,
                    focus,
                    scroll,
                })
            }
            Dialog::AgentPicker { mut selected } => {
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => selected = (selected + 1).min(2),
                    KeyCode::Char('k') | KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Enter | KeyCode::Char('o') => {
                        let provider =
                            [Provider::Codex, Provider::Claude, Provider::None][selected];
                        let mut settings = self.settings.clone();
                        if settings.ai.provider != provider {
                            settings.ai.model = None;
                        }
                        settings.ai.provider = provider;
                        if let Some(path) = &self.config_path {
                            let saved = (|| -> Result<()> {
                                let mut persisted = settings.clone();
                                if path.exists() {
                                    persisted.keybindings = settings::load(path)?.keybindings;
                                }
                                settings::save(path, &persisted)
                            })();
                            if let Err(error) = saved {
                                self.dialog = Some(Dialog::AgentPicker { selected });
                                return Err(error);
                            }
                        }
                        if self.settings.ai.provider != provider {
                            self.chat = Chat::default();
                            self.preview.guides.clear();
                        }
                        self.settings = settings;
                        if let Some(overview) = &mut self.overview {
                            overview.ai = if provider == Provider::None {
                                AiStatus::Disabled
                            } else {
                                AiStatus::Waiting
                            };
                        }
                        return Ok(if provider == Provider::None {
                            Action::DisableAi
                        } else {
                            Action::Explain { force: false }
                        });
                    }
                    _ => {}
                }
                Some(Dialog::AgentPicker { selected })
            }
            Dialog::OpenPr { mut input } => {
                if key.code == KeyCode::Enter && !self.busy {
                    match self
                        .repo
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("No repository selected"))
                        .and_then(|repo| prpr::github::parse_target(input.text.trim(), Some(repo)))
                    {
                        Ok(id) => return Ok(Action::LoadPr(id)),
                        Err(error) => {
                            self.dialog = Some(Dialog::OpenPr { input });
                            return Err(error);
                        }
                    }
                }
                let backspace =
                    key.code == KeyCode::Char('h') && key.modifiers.contains(KeyModifiers::CONTROL);
                if (!matches!(key.code, KeyCode::Char(c) if !c.is_ascii_digit()) || backspace)
                    && key.code != KeyCode::Enter
                {
                    input.key(key);
                }
                Some(Dialog::OpenPr { input })
            }
            Dialog::Help { mut scroll } => {
                scroll_key(&mut scroll, key);
                Some(Dialog::Help { scroll })
            }
            Dialog::Status { mut scroll } => {
                scroll_key(&mut scroll, key);
                Some(Dialog::Status { scroll })
            }
            Dialog::Activity { text, mut scroll } => {
                scroll_key(&mut scroll, key);
                Some(Dialog::Activity { text, scroll })
            }
            Dialog::Recovery { attempt } => {
                let review = self.review.as_mut().unwrap();
                if key.code == KeyCode::Char('y') {
                    let mut attempted_pr = review.data.pr.clone();
                    attempted_pr.head_sha = attempt.head_sha.clone();
                    attempted_pr.base_sha = attempt.base_sha.clone();
                    let mut remaining = store::load(&self.root, &attempted_pr)?;
                    for sent in &attempt.comments {
                        if let Some(index) = remaining.iter().position(|draft| draft == sent) {
                            remaining.remove(index);
                        }
                    }
                    let result = store::save(&self.root, &attempted_pr, &remaining)
                        .and_then(|()| store::clear_attempt(&self.root, &review.data.pr));
                    if let Err(error) = result {
                        self.dialog = Some(Dialog::Recovery { attempt });
                        return Err(error);
                    }
                    if review.data.pr.head_sha == attempt.head_sha
                        && review.data.pr.base_sha == attempt.base_sha
                    {
                        review.drafts = remaining;
                    }
                    self.message("Marked as submitted; matching local drafts cleared", false);
                    None
                } else if key.code == KeyCode::Char('n') {
                    if let Err(error) = store::clear_attempt(&self.root, &review.data.pr) {
                        self.dialog = Some(Dialog::Recovery { attempt });
                        return Err(error);
                    }
                    self.message(
                        "Marked as not submitted. Drafts remain saved; start a new review preview when ready.",
                        false,
                    );
                    None
                } else {
                    Some(Dialog::Recovery { attempt })
                }
            }
            Dialog::Quit => {
                if key.code == KeyCode::Enter || key.code == KeyCode::Char('q') {
                    return Ok(Action::Quit);
                }
                Some(Dialog::Quit)
            }
            Dialog::Description { mut scroll } => {
                scroll_key(&mut scroll, key);
                Some(Dialog::Description { scroll })
            }
            Dialog::Threads { mut scroll } => {
                scroll_key(&mut scroll, key);
                Some(Dialog::Threads { scroll })
            }
            Dialog::Note {
                mut anchor,
                reply_to,
                mut input,
            } => {
                if save {
                    anchor.body = input.text.trim().to_owned();
                    match self.add_note(anchor.clone(), reply_to.clone(), "user".into()) {
                        Ok(_) => {
                            self.message(
                                "Local note saved. Manage notes and replies from the command palette.",
                                false,
                            );
                            None
                        }
                        Err(error) => {
                            self.dialog = Some(Dialog::Note {
                                anchor,
                                reply_to,
                                input,
                            });
                            return Err(error);
                        }
                    }
                } else {
                    input.key(key);
                    Some(Dialog::Note {
                        anchor,
                        reply_to,
                        input,
                    })
                }
            }
            Dialog::Notes { mut selected } => {
                let review = self.review.as_mut().unwrap();
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        selected = (selected + 1).min(review.notes.len().saturating_sub(1))
                    }
                    KeyCode::Char('k') | KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Char('d') if selected < review.notes.len() => {
                        let id = review.notes[selected].id.clone();
                        let mut notes = review.notes.clone();
                        let mut removed = std::collections::HashSet::from([id]);
                        loop {
                            let old_len = removed.len();
                            for note in &notes {
                                if note
                                    .reply_to
                                    .as_ref()
                                    .is_some_and(|id| removed.contains(id))
                                {
                                    removed.insert(note.id.clone());
                                }
                            }
                            if removed.len() == old_len {
                                break;
                            }
                        }
                        notes.retain(|note| !removed.contains(&note.id));
                        notes::update(&self.root, &review.data, &review.notes, &notes)?;
                        review.notes = notes;
                        selected = selected.min(review.notes.len().saturating_sub(1));
                    }
                    KeyCode::Char('c') if selected < review.notes.len() => {
                        let note = &review.notes[selected];
                        if note.outdated {
                            bail!(
                                "This note is outdated; add a note on the current source instead"
                            );
                        }
                        let mut anchor = note.anchor.clone();
                        anchor.body.clear();
                        self.dialog = Some(Dialog::Note {
                            anchor,
                            reply_to: Some(note.id.clone()),
                            input: TextInput::default(),
                        });
                        return Ok(Action::None);
                    }
                    KeyCode::Char('o') | KeyCode::Enter if selected < review.notes.len() => {
                        let note = review.notes[selected].clone();
                        if note.outdated {
                            bail!("This note is outdated");
                        }
                        self.navigate(&note.anchor.path, &note.anchor.side, note.anchor.line)?;
                        return Ok(Action::None);
                    }
                    _ => {}
                }
                Some(Dialog::Notes { selected })
            }
            Dialog::Drafts { mut selected } => {
                let review = self.review.as_mut().unwrap();
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        selected = (selected + 1).min(review.drafts.len().saturating_sub(1))
                    }
                    KeyCode::Char('k') | KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Char('d') if !self.busy && selected < review.drafts.len() => {
                        let mut drafts = review.drafts.clone();
                        drafts.remove(selected);
                        if let Err(error) = store::save(&self.root, &review.data.pr, &drafts) {
                            self.dialog = Some(Dialog::Drafts { selected });
                            return Err(error);
                        }
                        review.drafts = drafts;
                        selected = selected.min(review.drafts.len().saturating_sub(1));
                    }
                    KeyCode::Char('c') | KeyCode::Char('o') | KeyCode::Enter
                        if !self.busy && selected < review.drafts.len() =>
                    {
                        let draft = review.drafts[selected].clone();
                        let input = TextInput::new(draft.body.clone());
                        self.dialog = Some(Dialog::Compose {
                            draft,
                            editing: Some(selected),
                            input,
                        });
                        return Ok(Action::None);
                    }
                    _ => {}
                }
                Some(Dialog::Drafts { selected })
            }
            Dialog::Compose {
                mut draft,
                editing,
                mut input,
            } => {
                if save {
                    if input.text.trim().is_empty() {
                        self.message("A comment cannot be empty", true);
                        Some(Dialog::Compose {
                            draft,
                            editing,
                            input,
                        })
                    } else {
                        draft.body = input.text.trim().to_owned();
                        let review = self.review.as_mut().unwrap();
                        let mut drafts = review.drafts.clone();
                        if let Some(index) = editing {
                            drafts[index] = draft;
                        } else {
                            drafts.push(draft);
                        }
                        if let Err(error) = store::save(&self.root, &review.data.pr, &drafts) {
                            self.dialog = Some(Dialog::Compose {
                                draft: drafts[editing.unwrap_or(drafts.len() - 1)].clone(),
                                editing,
                                input,
                            });
                            return Err(error);
                        }
                        review.drafts = drafts;
                        review.selection = None;
                        self.message("Draft saved locally. Manage drafts or prepare a review from the command palette.", false);
                        None
                    }
                } else {
                    input.key(key);
                    Some(Dialog::Compose {
                        draft,
                        editing,
                        input,
                    })
                }
            }
            Dialog::Submit {
                mut event,
                mut input,
            } => {
                if key.code == KeyCode::Tab {
                    event = match event {
                        ReviewEvent::Comment => ReviewEvent::Approve,
                        ReviewEvent::Approve => ReviewEvent::RequestChanges,
                        ReviewEvent::RequestChanges => ReviewEvent::Comment,
                    };
                } else if save {
                    if input.text.trim().is_empty() && event != ReviewEvent::Approve {
                        self.message("Comment and request-changes reviews need a summary", true);
                    } else {
                        self.dialog = Some(Dialog::Confirm {
                            event,
                            body: input.text.trim().to_owned(),
                            scroll: 0,
                        });
                        return Ok(Action::None);
                    }
                } else {
                    input.key(key);
                }
                Some(Dialog::Submit { event, input })
            }
            Dialog::Confirm {
                event,
                body,
                mut scroll,
            } => {
                if key.code == KeyCode::Enter && !self.busy {
                    if self.demo {
                        self.message("Demo: preview complete. Nothing was sent to GitHub.", false);
                        return Ok(Action::None);
                    }
                    let review = self.review.as_ref().unwrap();
                    return Ok(Action::Submit {
                        data: Box::new(review.data.clone()),
                        drafts: review.drafts.clone(),
                        body,
                        event,
                    });
                }
                scroll_key(&mut scroll, key);
                Some(Dialog::Confirm {
                    event,
                    body,
                    scroll,
                })
            }
        };
        Ok(Action::None)
    }
}

fn scroll_key(scroll: &mut u16, key: KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => *scroll = scroll.saturating_add(1),
        KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
        KeyCode::PageDown => *scroll = scroll.saturating_add(15),
        KeyCode::PageUp => *scroll = scroll.saturating_sub(15),
        KeyCode::Home | KeyCode::Char('g') => *scroll = 0,
        _ => {}
    }
}

pub fn demo_activity() -> PrActivity {
    PrActivity {
        labels: vec!["enhancement".into(), "risk:low".into()],
        checks: vec![
            Check {
                name: "CI / test".into(),
                state: CheckState::Failed,
                detail: "failure".into(),
                url: None,
            },
            Check {
                name: "CI / lint".into(),
                state: CheckState::Passed,
                detail: "success".into(),
                url: None,
            },
        ],
        conversation: vec![
            ConversationEntry {
                author: "reviewer".into(),
                action: "commented".into(),
                created_at: "2026-01-02T09:30:00Z".into(),
                body: "Could the retry delay be configurable?".into(),
            },
            ConversationEntry {
                author: "maintainer".into(),
                action: "approved".into(),
                created_at: "2026-01-03T11:00:00Z".into(),
                body: String::new(),
            },
        ],
    }
}

pub fn demo_data() -> ReviewData {
    ReviewData {
        source: prpr::model::ReviewSource::GitHub,
        pr: PullRequest { id: PrId { repo: "example/service".into(), number: 42 }, title: "Keep request retries within the deadline".into(), body: "## Motivation\nRetries currently ignore the overall request deadline.\n\nThis change bounds retry waits by the remaining time and adds a regression test.\n\nReview the zero-budget case and the error returned to callers.".into(), url: "https://github.com/example/service/pull/42".into(), author: "alex".into(), head_sha: "0123456789abcdef0123456789abcdef01234567".into(), base_sha: "abcdef0123456789abcdef0123456789abcdef01".into(), draft: false },
        files: vec![
            ChangedFile { filename: "src/client.rs".into(), previous_filename: None, status: "modified".into(), additions: 5, deletions: 2, patch: Some("@@ -18,6 +18,9 @@ impl Client {\n     pub fn retry(&self, budget: Duration) -> Result<Response> {\n-        let delay = self.retry_delay;\n-        sleep(delay);\n+        let delay = self.retry_delay.min(budget);\n+        if delay.is_zero() {\n+            return Err(Error::DeadlineExceeded);\n+        }\n+        sleep(delay);\n         self.send()\n     }\n }".into()) },
            ChangedFile { filename: "tests/deadline.rs".into(), previous_filename: None, status: "added".into(), additions: 6, deletions: 0, patch: Some("@@ -0,0 +1,6 @@\n+#[test]\n+fn exhausted_budget_returns_deadline_error() {\n+    let client = Client::new();\n+    let result = client.retry(Duration::ZERO);\n+    assert!(matches!(result, Err(Error::DeadlineExceeded)));\n+}".into()) },
        ],
        comments: vec![
            ReviewComment { id: 1, path: "src/client.rs".into(), body: "Should we also account for time spent in send()?".into(), author: "sam".into(), line: Some(25), original_line: Some(25), side: "RIGHT".into(), start_line: None, start_side: None, in_reply_to_id: None },
            ReviewComment { id: 2, path: "src/client.rs".into(), body: "Yes, I will pass the remaining deadline into send() and cover it with a test.".into(), author: "alex".into(), line: Some(25), original_line: Some(25), side: "RIGHT".into(), start_line: None, start_side: None, in_reply_to_id: Some(1) },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn navigation_and_modal_roundtrip_preserve_review_position() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().into(), true);
        app.loaded(demo_data()).unwrap();
        for _ in 0..4 {
            app.key(key('j')).unwrap();
        }
        let position = app.review.as_ref().unwrap().position();
        app.key(key('n')).unwrap();
        app.key(key('p')).unwrap();
        assert_eq!(app.review.as_ref().unwrap().position(), position);
        app.key(key('b')).unwrap();
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.review.as_ref().unwrap().position(), position);
    }

    #[test]
    fn unicode_editing_keeps_valid_boundaries() {
        let mut input = TextInput::default();
        input.key(key('日'));
        input.key(key('本'));
        input.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        input.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.text, "本");
        input.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert!(input.text.is_empty());
    }

    #[test]
    fn control_h_deletes_before_the_cursor_in_comment_and_summary_editors() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().into(), false);
        app.loaded(demo_data()).unwrap();
        app.key(key('j')).unwrap();
        let backspace = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
        for summary in [false, true] {
            app.key(key(if summary { 's' } else { 'c' })).unwrap();
            for c in "日A本".chars() {
                app.key(key(c)).unwrap();
            }
            app.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
                .unwrap();
            app.key(backspace).unwrap();
            let input = match app.dialog.as_ref().unwrap() {
                Dialog::Compose { input, .. } | Dialog::Submit { input, .. } => input,
                _ => panic!("Expected a text editor"),
            };
            assert_eq!(input.text, "日本");
            assert_eq!(input.cursor, "日".len());
            app.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
                .unwrap();
            app.key(backspace).unwrap();
            app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .unwrap();
        }
    }

    #[test]
    fn o_opens_prs_while_explicit_editor_shortcuts_open_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().into(), false);
        let data = demo_data();
        app.inbox.push(PrSummary {
            body: "Example description".into(),
            id: data.pr.id.clone(),
            title: data.pr.title.clone(),
            author: data.pr.author.clone(),
            draft: false,
        });
        assert!(matches!(app.key(key('o')).unwrap(), Action::LoadPr(id) if id == data.pr.id));
        app.loaded(data).unwrap();
        app.key(key('j')).unwrap();
        app.key(key('j')).unwrap();
        app.review.as_mut().unwrap().focus = Focus::Files;
        assert!(matches!(app.key(key('o')).unwrap(), Action::None));
        match app.key(key('e')).unwrap() {
            Action::OpenEditor(target) => {
                assert_eq!(target.side, Side::Left);
                assert_eq!(target.line, 19);
            }
            _ => panic!("By default a file opens without preparing a workspace"),
        }
        app.settings.editor.open_in = OpenIn::Workspace;
        match app.key(key('e')).unwrap() {
            Action::Prepare {
                open_editor: Some(target),
                ..
            } => {
                assert_eq!(target.side, Side::Left);
                assert_eq!(target.line, 19);
            }
            _ => panic!("Workspace mode should prepare the review workspace first"),
        }
        app.review.as_mut().unwrap().workspace = Some(dir.path().into());
        assert!(matches!(app.key(key('e')).unwrap(), Action::OpenEditor(_)));
        app.busy = true;
        assert!(matches!(app.key(key('e')).unwrap(), Action::None));
    }

    #[test]
    fn comment_toggle_preserves_selection_and_full_threads_remain_available() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().into(), true);
        app.loaded(demo_data()).unwrap();
        app.key(key('j')).unwrap();
        let position = app.review.as_ref().unwrap().position();
        assert!(app.review.as_ref().unwrap().show_comments);
        app.key(key('t')).unwrap();
        assert!(!app.review.as_ref().unwrap().show_comments);
        assert!(app.dialog.is_none());
        assert_eq!(app.review.as_ref().unwrap().position(), position);
        app.key(key('t')).unwrap();
        assert!(app.review.as_ref().unwrap().show_comments);
        app.key(key('T')).unwrap();
        assert!(matches!(app.dialog, Some(Dialog::Threads { .. })));
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        app.key(key('t')).unwrap();
        app.loaded(demo_data()).unwrap();
        assert!(!app.review.as_ref().unwrap().show_comments);
    }

    #[test]
    fn opening_a_number_uses_the_selected_repository_and_keeps_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().into(), false);
        app.repo = Some("owner/repository".into());
        app.key(key('#')).unwrap();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.key(enter).is_err());
        assert!(matches!(app.dialog, Some(Dialog::OpenPr { .. })));
        for digit in "123".chars() {
            app.key(key(digit)).unwrap();
        }
        match app.key(enter).unwrap() {
            Action::LoadPr(id) => {
                assert_eq!(id.repo, "owner/repository");
                assert_eq!(id.number, 123);
            }
            _ => panic!("A PR number must open directly in the current repository"),
        }
        assert!(app.dialog.is_none());
    }

    #[test]
    fn draft_composition_persists_without_network_and_submission_needs_preview() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().into(), true);
        app.loaded(demo_data()).unwrap();
        app.key(key('j')).unwrap();
        app.key(key('c')).unwrap();
        assert!(matches!(app.dialog, Some(Dialog::Compose { .. })));
        for c in "Please check this".chars() {
            app.key(key(c)).unwrap();
        }
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(
            store::load(dir.path(), &app.review.as_ref().unwrap().data.pr)
                .unwrap()
                .len(),
            1
        );
        app.key(key('s')).unwrap();
        for c in "Review summary".chars() {
            app.key(key(c)).unwrap();
        }
        let action = app
            .key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(matches!(action, Action::None));
        assert!(matches!(app.dialog, Some(Dialog::Confirm { .. })));
    }
}
