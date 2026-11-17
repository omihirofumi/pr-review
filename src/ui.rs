use std::collections::{BTreeMap, HashMap, HashSet};

use prpr::{
    analysis::Analysis,
    diff::{DiffLine, LineKind},
    keybindings::{self, View},
    model::{ChangedFile, CheckState, Side},
    settings::{DiffLayout, Provider},
    thread::{Thread, matches_anchor, threads},
};
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
        Wrap,
    },
};

use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::app::{
    ActivityStatus, AiReviewStatus, AiStatus, App, Dialog, Focus, OverviewState, PageTab,
    PreviewTab, ReviewState, TextInput,
};

const BACKGROUND: Color = Color::Rgb(24, 24, 23);
const SIDEBAR: Color = Color::Rgb(29, 29, 28);
const PANEL: Color = Color::Rgb(37, 37, 35);
const BORDER: Color = Color::Rgb(58, 58, 54);
const TEXT: Color = Color::Rgb(218, 216, 207);
const ACCENT: Color = Color::Rgb(221, 185, 126);
const MUTED: Color = Color::Rgb(144, 144, 134);
const GREEN: Color = Color::Rgb(151, 189, 137);
const RED: Color = Color::Rgb(218, 143, 132);
const ADDITION: Color = Color::Rgb(31, 49, 37);
const DELETION: Color = Color::Rgb(55, 34, 32);
const SELECTED: Color = Color::Rgb(62, 57, 46);
const SELECTED_ADDITION: Color = Color::Rgb(48, 74, 52);
const SELECTED_DELETION: Color = Color::Rgb(83, 48, 43);
const COMMENT: Color = Color::Rgb(35, 36, 38);
const COMMENT_ACCENT: Color = Color::Rgb(155, 174, 199);
const DRAFT: Color = Color::Rgb(44, 39, 29);
const CODE_BACKGROUND: Color = Color::Rgb(32, 35, 39);

#[derive(Clone)]
struct ReviewMarkdown;

impl tui_markdown::StyleSheet for ReviewMarkdown {
    fn heading(&self, level: u8) -> Style {
        Style::default()
            .fg(if level <= 2 { ACCENT } else { TEXT })
            .bold()
    }

    fn heading_marker(&self, _: u8) -> &str {
        ""
    }
    fn code_block_fence(&self) -> &str {
        ""
    }
    fn code(&self) -> Style {
        Style::default().fg(COMMENT_ACCENT).bg(CODE_BACKGROUND)
    }
    fn link(&self) -> Style {
        Style::default().fg(COMMENT_ACCENT).underlined()
    }
    fn blockquote(&self) -> Style {
        Style::default().fg(MUTED).italic()
    }
    fn list_marker(&self) -> Style {
        Style::default().fg(ACCENT)
    }
    fn table_header(&self) -> Style {
        Style::default().fg(ACCENT).bold()
    }
    fn table_border(&self) -> Style {
        Style::default().fg(BORDER)
    }
    fn alert_icon(&self, _: tui_markdown::AlertKind) -> &str {
        ""
    }
    fn alert(&self, kind: tui_markdown::AlertKind) -> Style {
        use tui_markdown::AlertKind;
        Style::default().fg(match kind {
            AlertKind::Warning | AlertKind::Caution => RED,
            AlertKind::Tip => GREEN,
            _ => COMMENT_ACCENT,
        })
    }
}

fn wrap_styled(
    line: &Line<'_>,
    width: usize,
    continuation: usize,
    preserve_spaces: bool,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let graphemes = line.styled_graphemes(Style::default()).collect::<Vec<_>>();
    let mut rows = Vec::new();
    let mut start = 0;
    while start < graphemes.len() {
        let indent = if start == 0 {
            0
        } else {
            continuation.min(width.saturating_sub(2))
        };
        let mut used = indent;
        let mut end = start;
        let mut space = None;
        while end < graphemes.len() {
            let next = Span::raw(graphemes[end].symbol).width();
            if used + next > width && end > start {
                break;
            }
            if graphemes[end].is_whitespace() && end > start {
                space = Some(end);
            }
            used += next;
            end += 1;
        }
        if !preserve_spaces
            && end < graphemes.len()
            && let Some(space) = space
        {
            end = space;
        }
        let mut spans = vec![Span::raw(" ".repeat(indent))];
        for grapheme in &graphemes[start..end] {
            if let Some(last) = spans.last_mut()
                && last.style == grapheme.style
            {
                last.content.to_mut().push_str(grapheme.symbol);
            } else {
                spans.push(Span::styled(grapheme.symbol.to_owned(), grapheme.style));
            }
        }
        rows.push(Line::from(spans).style(line.style));
        start = end;
        if !preserve_spaces {
            while start < graphemes.len() && graphemes[start].is_whitespace() {
                start += 1;
            }
        }
    }
    if rows.is_empty() {
        rows.push(Line::from("").style(line.style));
    }
    rows
}

fn expand_tabs(line: Line<'_>) -> Line<'static> {
    let mut column = 0;
    let spans = line
        .spans
        .into_iter()
        .map(|span| {
            let mut expanded = String::new();
            for part in span.content.split_inclusive('\t') {
                let text = part.strip_suffix('\t').unwrap_or(part);
                expanded.push_str(text);
                column += Span::raw(text).width();
                if part.ends_with('\t') {
                    let padding = 4 - column % 4;
                    expanded.push_str(&" ".repeat(padding));
                    column += padding;
                }
            }
            Span::styled(expanded, span.style)
        })
        .collect::<Vec<_>>();
    Line::from(spans).style(line.style)
}

/// Removes HTML comments outside fenced code, matching GitHub's rendered view of PR templates.
fn strip_html_comments(body: &str) -> String {
    let mut result = String::with_capacity(body.len());
    let mut fence: Option<&str> = None;
    let mut in_comment = false;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if !in_comment {
            if let Some(marker) = fence {
                if trimmed.starts_with(marker) {
                    fence = None;
                }
                result.push_str(line);
                continue;
            }
            if let Some(marker) = ["```", "~~~"]
                .into_iter()
                .find(|marker| trimmed.starts_with(marker))
            {
                fence = Some(marker);
                result.push_str(line);
                continue;
            }
        }
        let mut kept = String::new();
        let mut rest = line;
        let mut removed = in_comment;
        loop {
            if in_comment {
                match rest.find("-->") {
                    Some(end) => {
                        rest = &rest[end + 3..];
                        in_comment = false;
                    }
                    None => {
                        rest = "";
                        break;
                    }
                }
            } else {
                match rest.find("<!--") {
                    Some(start) => {
                        kept.push_str(&rest[..start]);
                        rest = &rest[start + 4..];
                        in_comment = true;
                        removed = true;
                    }
                    None => break,
                }
            }
        }
        kept.push_str(rest);
        if removed && kept.trim().is_empty() {
            continue;
        }
        result.push_str(&kept);
    }
    result
}

fn markdown_text(body: &str, width: u16) -> Text<'static> {
    let width = usize::from(width.max(1));
    let options = tui_markdown::Options::new(ReviewMarkdown).table_width(width as u16);
    let body = if body.contains("<!--") {
        strip_html_comments(body)
    } else {
        body.to_owned()
    };
    let text = tui_markdown::from_str_with_options(&body, &options);
    let mut rows = Vec::new();
    for line in text.lines {
        let line = if line.spans.iter().any(|span| span.content.contains('\t')) {
            expand_tabs(line)
        } else {
            line
        };
        let code = line.style.bg == Some(CODE_BACKGROUND);
        let plain = line.to_string();
        let leading = plain.len() - plain.trim_start().len();
        let marker = plain[leading..]
            .split_once(' ')
            .map(|(marker, _)| marker)
            .unwrap_or("");
        let list = marker == "-"
            || marker == "*"
            || marker
                .trim_end_matches(['.', ')'])
                .chars()
                .all(|c| c.is_ascii_digit())
                && !marker.is_empty();
        let continuation = if code {
            0
        } else if list || marker == ">" {
            Span::raw(&plain[..leading + marker.len() + 1]).width()
        } else {
            Span::raw(&plain[..leading]).width()
        };
        let content_width = width.saturating_sub(if code { 2 } else { 0 }).max(1);
        for mut row in wrap_styled(&line, content_width, continuation, code) {
            if code {
                row.spans.insert(
                    0,
                    Span::styled("│ ", Style::default().fg(BORDER).bg(CODE_BACKGROUND)),
                );
                let padding = width.saturating_sub(row.width());
                row.spans.push(Span::styled(
                    " ".repeat(padding),
                    Style::default().bg(CODE_BACKGROUND),
                ));
            }
            rows.push(row);
        }
    }
    Text::from(rows)
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(BACKGROUND).fg(TEXT)),
        area,
    );
    if area.width < 48 || area.height < 12 {
        frame.render_widget(
            Paragraph::new("Please resize to at least 48 columns and 12 rows.\nq quits.")
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let [header, body, status, help] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(frame, app, header);
    let (body, chat) = if app.chat_open && app.review.is_some() && area.width >= 120 {
        let width = (body.width * 2 / 5).clamp(48, 90);
        let [body, chat] =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(width)]).areas(body);
        (body, Some(chat))
    } else {
        (body, None)
    };
    let body = if app.overview.is_some() {
        let [tabs, content] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(body);
        draw_tabs(frame, app, tabs);
        content
    } else {
        body
    };
    if app.overview.is_some() && matches!(app.tab, PageTab::Overview) {
        draw_overview(frame, app, body);
    } else if app.overview.is_some() && matches!(app.tab, PageTab::Guide) {
        draw_guide(frame, app, body);
    } else if app.review.is_some() {
        draw_review(frame, app, body);
    } else if app.overview.is_some() {
        draw_diff_placeholder(frame, app, body);
    } else {
        draw_inbox(frame, app, body);
    }
    frame.render_widget(
        Paragraph::new(format!(
            " {}{}",
            if app.busy { "Working · " } else { "" },
            app.status
        ))
        .style(
            Style::default()
                .fg(if app.error {
                    RED
                } else if app.busy {
                    ACCENT
                } else {
                    MUTED
                })
                .bg(SIDEBAR),
        ),
        status,
    );
    let keys = if app.goto_pending {
        if app.pending_keys.is_empty() {
            " Sequence pending · Esc cancels ".into()
        } else {
            format!(
                " {} … · Esc cancels",
                app.pending_keys
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
    } else {
        let hints: &[(&str, &str)] = match app.binding_view() {
            View::Inbox => &[
                ("command_palette", "commands"),
                ("open_pr", "diff"),
                ("toggle_preview", "preview"),
                ("help", "help"),
            ],
            View::Overview => &[
                ("command_palette", "commands"),
                ("guide", "AI guide"),
                ("diff", "diff"),
                ("review_activity", "conversation"),
                ("toggle_chat", "AI chat"),
                ("open_github", "GitHub"),
                ("help", "help"),
            ],
            View::Guide => &[
                ("command_palette", "commands"),
                ("next_reading_step", "next step"),
                ("open_reading_step", "jump"),
                ("ai_guide", "regenerate"),
                ("toggle_chat", "AI chat"),
                ("choose_agent", "agent"),
                ("help", "help"),
            ],
            View::Diff => &[
                ("command_palette", "commands"),
                ("previous_hunk", "prev hunk"),
                ("next_hunk", "next hunk"),
                ("toggle_sidebar", "files"),
                ("ask", "ask"),
                ("explain_diff", "explain"),
                ("toggle_chat", "AI chat"),
                ("help", "help"),
            ],
        };
        format!(
            " {}",
            hints
                .iter()
                .filter_map(|(id, label)| app
                    .settings
                    .keybindings
                    .keys(id)
                    .first()
                    .map(|key| format!("{key} {label}")))
                .collect::<Vec<_>>()
                .join("  ")
        )
    };
    frame.render_widget(
        Paragraph::new(keys).style(Style::default().fg(MUTED).bg(PANEL)),
        help,
    );
    let asking = matches!(app.dialog, Some(Dialog::Ask));
    if let Some(chat) = chat {
        app.chat_scroll = draw_chat(frame, app, chat, asking);
    }
    if asking {
        if chat.is_none() {
            let area = frame.area();
            let rect = Rect::new(
                area.x + 1,
                area.y + 1,
                area.width.saturating_sub(2),
                area.height.saturating_sub(2),
            );
            app.chat_scroll = draw_chat(frame, app, rect, true);
        }
    } else if let Some(Dialog::Description { scroll }) = &app.dialog {
        let actual = draw_description(frame, app, *scroll);
        if let Some(Dialog::Description { scroll }) = &mut app.dialog {
            *scroll = actual;
        }
    } else if let Some(Dialog::AiReview { scroll }) = &app.dialog {
        let actual = draw_ai_review(frame, app, *scroll);
        if let Some(Dialog::AiReview { scroll }) = &mut app.dialog {
            *scroll = actual;
        }
    } else if let Some(dialog) = &app.dialog {
        draw_dialog(frame, app, dialog);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(overview) = &app.overview {
        draw_overview_header(frame, app, overview, area);
        return;
    }
    let context = app.review.as_ref().map_or_else(
        || app.repo.clone().unwrap_or_else(|| "Pull requests".into()),
        |review| format!("{}  #{}", review.data.pr.id.repo, review.data.pr.id.number),
    );
    let mode = if app.demo { "prpr · demo" } else { "prpr" };
    let right = app.review.as_ref().map_or_else(
        || format!("{} pull requests", app.inbox.len()),
        |review| {
            if let Some(selection) = selection_label(review) {
                return selection;
            }
            format!(
                "{} drafts · {} files · {}",
                review.drafts.len(),
                review.data.files.len(),
                &review.data.pr.head_sha[..review.data.pr.head_sha.len().min(8)]
            )
        },
    );
    let [first, second, rule] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let [label, summary] = Layout::horizontal([
        Constraint::Min(20),
        Constraint::Length(if area.width >= 90 { 36 } else { 17 }),
    ])
    .areas(first);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {mode}"), Style::default().fg(ACCENT).bold()),
            Span::styled(format!("   {context}"), Style::default().fg(TEXT)),
        ]))
        .style(Style::default().bg(SIDEBAR)),
        label,
    );
    frame.render_widget(
        Paragraph::new(if area.width >= 90 {
            right
        } else {
            app.review.as_ref().map_or_else(
                || format!("{} PRs", app.inbox.len()),
                |r| selection_label(r).unwrap_or_else(|| format!("{} drafts", r.drafts.len())),
            )
        })
        .right_aligned()
        .style(Style::default().fg(MUTED).bg(SIDEBAR)),
        summary,
    );
    let subtitle = app.review.as_ref().map_or_else(
        || {
            format!(
                " {} · {} opens a review; {} opens a PR by number",
                app.repo.as_deref().unwrap_or("Your review queue"),
                app.settings.keybindings.primary("open_pr"),
                app.settings.keybindings.primary("open_pr_number")
            )
        },
        |review| {
            format!(
                " {}  ·  {}{}",
                review.data.pr.title,
                review.data.pr.author,
                if review.data.pr.draft {
                    " · draft"
                } else {
                    ""
                }
            )
        },
    );
    frame.render_widget(
        Paragraph::new(subtitle).style(Style::default().fg(TEXT).bg(SIDEBAR)),
        second,
    );
    frame.render_widget(
        Paragraph::new("─".repeat(usize::from(rule.width))).style(Style::default().fg(BORDER)),
        rule,
    );
}

fn draw_overview_header(frame: &mut Frame, app: &App, overview: &OverviewState, area: Rect) {
    let metadata = &overview.metadata;
    let [first, title, rule] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let [identity, status] = Layout::horizontal([
        Constraint::Min(20),
        Constraint::Length(if area.width >= 90 { 36 } else { 17 }),
    ])
    .areas(first);
    let label = if app.demo { "prpr · demo" } else { "prpr" };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {label}"), Style::default().fg(ACCENT).bold()),
            Span::styled(
                if app.local_spec.is_some() {
                    "   local review".into()
                } else {
                    format!("   {} #{}", metadata.id.repo, metadata.id.number)
                },
                Style::default().fg(TEXT),
            ),
        ]))
        .style(Style::default().bg(SIDEBAR)),
        identity,
    );
    let selection = app
        .review
        .as_ref()
        .filter(|_| matches!(app.tab, PageTab::Diff))
        .and_then(selection_label);
    let summary = selection.unwrap_or_else(|| {
        if area.width >= 90 {
            format!(
                "{} files · +{} -{} · {}",
                metadata.changed_files,
                metadata.additions,
                metadata.deletions,
                &metadata.head_sha[..metadata.head_sha.len().min(8)]
            )
        } else {
            format!("{} files", metadata.changed_files)
        }
    });
    frame.render_widget(
        Paragraph::new(summary)
            .right_aligned()
            .style(Style::default().fg(MUTED).bg(SIDEBAR)),
        status,
    );
    frame.render_widget(
        Paragraph::new(format!(" {}", metadata.title)).style(Style::default().fg(TEXT).bg(SIDEBAR)),
        title,
    );
    frame.render_widget(
        Paragraph::new("─".repeat(usize::from(rule.width))).style(Style::default().fg(BORDER)),
        rule,
    );
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let keys = &app.settings.keybindings;
    let diff = if app.diff_loading {
        "Diff · loading"
    } else if app.diff_error.is_some() {
        "Diff · error"
    } else if app.comments_loading {
        "Diff · comments"
    } else {
        "Diff"
    };
    let guide = match app.overview.as_ref().map(|overview| &overview.ai) {
        Some(AiStatus::Loading) => "AI Guide · loading",
        Some(AiStatus::Error(_)) => "AI Guide · error",
        Some(AiStatus::Ready(_)) => "AI Guide · ready",
        Some(AiStatus::Disabled) => "AI Guide · off",
        _ => "AI Guide",
    };
    let selected = |active| {
        Style::default()
            .fg(if active { ACCENT } else { MUTED })
            .bg(if active { PANEL } else { SIDEBAR })
            .bold()
    };
    let mut spans = Vec::new();
    for (tab, id, label) in [
        (PageTab::Overview, "overview", "Overview"),
        (PageTab::Guide, "guide", guide),
        (PageTab::Diff, "diff", diff),
    ] {
        spans.push(Span::styled(
            format!(" {} {label} ", keys.primary(id)),
            selected(app.tab == tab),
        ));
        spans.push(Span::raw(" "));
    }
    if area.width >= 100 {
        spans.push(Span::styled(
            format!(
                "   {} / {} switches views",
                keys.primary("next_view"),
                keys.primary("previous_view")
            ),
            Style::default().fg(MUTED),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(SIDEBAR)),
        area,
    );
}

fn draw_diff_placeholder(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = vec![Line::from("")];
    if let Some(error) = &app.diff_error {
        lines.push(Line::from(Span::styled(
            "  The diff could not be loaded.",
            Style::default().fg(RED).bold(),
        )));
        lines.push(Line::from(""));
        lines.extend(
            wrap_comment(error, usize::from(area.width).saturating_sub(4).max(1))
                .into_iter()
                .map(|line| Line::from(format!("  {line}"))),
        );
        lines.push(Line::from(""));
        lines.push(Line::from("  r retries · 1 returns to the overview"));
    } else {
        lines.push(Line::from(Span::styled(
            "  Loading changed files…",
            Style::default().fg(ACCENT).bold(),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from("  PR details are available in 1 Overview."));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(TEXT))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_overview(frame: &mut Frame, app: &mut App, area: Rect) {
    let local = app.local_spec.is_some();
    let Some(overview) = &mut app.overview else {
        return;
    };
    let metadata = &overview.metadata;
    let identity = if local {
        " Local changes".to_owned()
    } else {
        format!(
            " {} · {}{}",
            metadata.author,
            metadata.state.to_lowercase(),
            if metadata.draft { " · draft" } else { "" }
        )
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(identity, Style::default().fg(TEXT)),
        Span::styled(
            format!(
                " · {} changed files · +{} -{}",
                metadata.changed_files, metadata.additions, metadata.deletions
            ),
            Style::default().fg(MUTED),
        ),
    ])];
    if !metadata.url.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" {}", metadata.url),
            Style::default().fg(MUTED),
        )));
    }
    lines.extend(activity_lines(&overview.activity));
    let [details, content] = Layout::vertical([
        Constraint::Length(lines.len() as u16 + 1),
        Constraint::Min(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(lines), details);
    let body = overview_markdown(&metadata.body, &overview.activity, local);
    overview.scroll = markdown(frame, &body, content, overview.scroll);
}

fn activity_lines(activity: &ActivityStatus) -> Vec<Line<'static>> {
    let label = |text: &str| Span::styled(format!(" {text:<7}"), Style::default().fg(MUTED));
    match activity {
        ActivityStatus::Unavailable => Vec::new(),
        ActivityStatus::Loading => vec![
            Line::from(vec![
                label("Labels"),
                Span::styled("loading…", Style::default().fg(MUTED)),
            ]),
            Line::from(vec![
                label("CI"),
                Span::styled("loading…", Style::default().fg(MUTED)),
            ]),
        ],
        ActivityStatus::Error(error) => vec![Line::from(vec![
            label("GitHub"),
            Span::styled(
                format!("Labels, CI, and conversation unavailable: {error}"),
                Style::default().fg(RED),
            ),
        ])],
        ActivityStatus::Ready(activity) => {
            let mut labels = vec![label("Labels")];
            if activity.labels.is_empty() {
                labels.push(Span::styled("none", Style::default().fg(MUTED)));
            }
            for name in &activity.labels {
                labels.push(Span::styled(
                    format!(" {name} "),
                    Style::default().fg(COMMENT_ACCENT).bg(PANEL),
                ));
                labels.push(Span::raw(" "));
            }
            let mut checks = vec![label("CI")];
            if activity.checks.is_empty() {
                checks.push(Span::styled(
                    "no checks reported",
                    Style::default().fg(MUTED),
                ));
            }
            let mut first = true;
            for (state, word, color) in [
                (CheckState::Failed, "failed", RED),
                (CheckState::Pending, "pending", ACCENT),
                (CheckState::Passed, "passed", GREEN),
                (CheckState::Skipped, "skipped", MUTED),
            ] {
                let count = activity.count(state);
                if count == 0 {
                    continue;
                }
                if !first {
                    checks.push(Span::styled(" · ", Style::default().fg(MUTED)));
                }
                first = false;
                checks.push(Span::styled(
                    format!("{count} {word}"),
                    Style::default().fg(color).bold(),
                ));
            }
            vec![Line::from(labels), Line::from(checks)]
        }
    }
}

fn overview_markdown(body: &str, activity: &ActivityStatus, local: bool) -> String {
    let mut text = String::new();
    if let ActivityStatus::Ready(activity) = activity {
        let attention = activity
            .checks
            .iter()
            .filter(|check| matches!(check.state, CheckState::Failed | CheckState::Pending))
            .collect::<Vec<_>>();
        if !attention.is_empty() {
            text.push_str("### Checks needing attention\n\n");
            for check in attention {
                text.push_str(&format!("- **{}** · {}\n", check.detail, check.name));
            }
            text.push('\n');
        }
    }
    text.push_str(if local {
        "### Change description\n\n"
    } else {
        "### Author description\n\n"
    });
    text.push_str(if body.trim().is_empty() {
        "_No description._"
    } else {
        body.trim_end()
    });
    text.push_str("\n\n");
    match activity {
        ActivityStatus::Ready(activity) => {
            text.push_str(&format!(
                "### Conversation · {}\n\n",
                activity.conversation.len()
            ));
            if activity.conversation.is_empty() {
                text.push_str("_No conversation comments or reviews._\n");
            }
            for entry in &activity.conversation {
                text.push_str(&format!(
                    "#### @{} · {} · {}\n\n",
                    entry.author,
                    entry.action,
                    entry.time()
                ));
                if !entry.body.trim().is_empty() {
                    text.push_str(entry.body.trim_end());
                    text.push_str("\n\n");
                }
            }
        }
        ActivityStatus::Loading => text.push_str("### Conversation\n\n_Loading…_\n"),
        ActivityStatus::Error(_) | ActivityStatus::Unavailable => {}
    }
    text
}

fn draw_guide(frame: &mut Frame, app: &mut App, area: Rect) {
    let keys = &app.settings.keybindings;
    let local = app.local_spec.is_some();
    let Some(overview) = &mut app.overview else {
        return;
    };
    let [state, content] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    let (caption, color) = match &overview.ai {
        AiStatus::Disabled => (
            format!(
                " AI guide not configured · {} chooses an agent",
                keys.primary("choose_agent")
            ),
            MUTED,
        ),
        AiStatus::Waiting => (
            if app.diff_loading {
                " Waiting for the diff to prepare the AI guide…".to_owned()
            } else {
                format!(
                    " AI guide is ready to generate · {} explains this {}",
                    keys.primary("ai_guide"),
                    if local { "change" } else { "PR" }
                )
            },
            ACCENT,
        ),
        AiStatus::Loading => (
            format!(" Preparing AI guide with {}…", app.settings.ai.provider),
            ACCENT,
        ),
        AiStatus::Ready(_) => (
            format!(
                " AI GUIDE · {} · {} regenerates · {}/{} reading order · {} jumps",
                app.settings.ai.provider,
                keys.primary("ai_guide"),
                keys.primary("next_reading_step"),
                keys.primary("previous_reading_step"),
                keys.primary("open_reading_step")
            ),
            ACCENT,
        ),
        AiStatus::Error(_) => (
            format!(
                " AI guide unavailable · {} retries · {} chooses an agent",
                keys.primary("ai_guide"),
                keys.primary("choose_agent")
            ),
            RED,
        ),
    };
    frame.render_widget(
        Paragraph::new(caption)
            .style(Style::default().fg(color))
            .wrap(Wrap { trim: false }),
        state,
    );
    match &overview.ai {
        AiStatus::Ready(analysis) => {
            let wide = content.width >= 110;
            let [narrative, guide] = if wide {
                Layout::horizontal([
                    Constraint::Length(content.width.saturating_sub(38).min(98)),
                    Constraint::Length(38),
                ])
                .flex(Flex::Center)
                .areas(content)
            } else {
                [content, Rect::default()]
            };
            let narrative = Rect {
                width: narrative.width.min(98),
                ..narrative
            };
            let rows = overview_rows(analysis, narrative.width, !wide);
            let scroll = draw_overview_rows(frame, &rows, overview, narrative);
            if wide {
                draw_reading_guide(frame, analysis, overview.selected_step, guide);
            }
            overview.guide_scroll = scroll;
        }
        status => {
            let message = match status {
                AiStatus::Error(error) => format!("> Guide unavailable: {error}"),
                AiStatus::Disabled => format!(
                    "No AI agent is configured. Press **{}** to choose Codex or Claude. The guide summarizes the change, its intent, risks, and a reading order through the diff.",
                    keys.primary("choose_agent")
                ),
                AiStatus::Loading => "The guide appears here when the agent finishes. Overview and Diff stay usable meanwhile.".to_owned(),
                _ => format!(
                    "Press **{}** to generate a structured reading guide for this change.",
                    keys.primary("ai_guide")
                ),
            };
            overview.guide_scroll = markdown(frame, &message, content, overview.guide_scroll);
        }
    }
}

struct OverviewRow {
    line: Line<'static>,
    step: Option<usize>,
}

fn overview_gap(rows: &mut Vec<OverviewRow>) {
    if rows
        .last()
        .is_some_and(|row| !row.line.to_string().trim().is_empty())
    {
        rows.push(OverviewRow {
            line: Line::from(""),
            step: None,
        });
    }
}

fn overview_heading(rows: &mut Vec<OverviewRow>, heading: &str) {
    overview_gap(rows);
    let (title, hint) = heading.split_once(" · ").unwrap_or((heading, ""));
    let mut spans = vec![
        Span::styled("─ ", Style::default().fg(BORDER)),
        Span::styled(title.to_owned(), Style::default().fg(ACCENT).bold()),
    ];
    if !hint.is_empty() {
        spans.push(Span::styled(
            format!("  {hint}"),
            Style::default().fg(MUTED),
        ));
    }
    rows.push(OverviewRow {
        line: Line::from(spans),
        step: None,
    });
    rows.push(OverviewRow {
        line: Line::from(""),
        step: None,
    });
}

fn overview_text(
    rows: &mut Vec<OverviewRow>,
    text: &str,
    width: u16,
    indent: usize,
    color: Color,
    step: Option<usize>,
) {
    let available = usize::from(width.min(96)).saturating_sub(indent + 1).max(1) as u16;
    for mut line in markdown_text(text, available).lines {
        line.spans.insert(0, Span::raw(" ".repeat(indent)));
        line.style = line.style.patch(Style::default().fg(color));
        if color == ACCENT {
            line.style = line.style.bold();
        }
        rows.push(OverviewRow { line, step });
    }
}

fn overview_literal(
    rows: &mut Vec<OverviewRow>,
    text: &str,
    width: u16,
    indent: usize,
    color: Color,
    step: Option<usize>,
) {
    let available = usize::from(width.min(96)).saturating_sub(indent + 1).max(1);
    for mut line in wrap_styled(&Line::from(text), available, 0, false) {
        line.spans.insert(0, Span::raw(" ".repeat(indent)));
        line.style = Style::default().fg(color);
        rows.push(OverviewRow { line, step });
    }
}

fn reading_step_label(index: usize, step: &prpr::analysis::ReadingStep) -> String {
    format!(
        "{}. {}{} · {}",
        index + 1,
        step.path,
        step.line
            .map_or_else(String::new, |line| format!(":{line}")),
        side_label(&step.side)
    )
}

fn overview_reading_order(rows: &mut Vec<OverviewRow>, analysis: &Analysis, width: u16) {
    overview_heading(rows, "Reading order");
    if analysis.reading_order.is_empty() {
        overview_text(
            rows,
            "No reading order was provided.",
            width,
            2,
            MUTED,
            None,
        );
    }
    for (index, step) in analysis.reading_order.iter().enumerate() {
        overview_literal(
            rows,
            &reading_step_label(index, step),
            width,
            2,
            COMMENT_ACCENT,
            Some(index),
        );
        overview_text(rows, &step.reason, width, 4, TEXT, Some(index));
        rows.push(OverviewRow {
            line: Line::from(""),
            step: None,
        });
    }
}

fn overview_rows(analysis: &Analysis, width: u16, include_guide: bool) -> Vec<OverviewRow> {
    let mut rows = Vec::new();
    overview_heading(&mut rows, "Summary");
    overview_text(&mut rows, &analysis.summary, width, 2, TEXT, None);
    let basis = match analysis.intent.basis.as_str() {
        "author_description" => "from author description",
        "inferred_from_diff" => "inferred from diff",
        _ => "unknown basis",
    };
    overview_heading(&mut rows, &format!("Intent · {basis}"));
    overview_text(&mut rows, &analysis.intent.text, width, 2, TEXT, None);
    overview_heading(&mut rows, "Main changes");
    if analysis.changes.is_empty() {
        overview_text(
            &mut rows,
            "No changes were summarized.",
            width,
            2,
            MUTED,
            None,
        );
    }
    for change in &analysis.changes {
        overview_gap(&mut rows);
        overview_text(&mut rows, &change.title, width, 2, ACCENT, None);
        overview_text(&mut rows, &change.detail, width, 4, TEXT, None);
        if !change.files.is_empty() {
            overview_literal(
                &mut rows,
                &format!("Files: {}", change.files.join(", ")),
                width,
                4,
                MUTED,
                None,
            );
        }
    }
    if include_guide {
        overview_reading_order(&mut rows, analysis, width);
    }
    overview_heading(&mut rows, "Review risks");
    if analysis.risks.is_empty() {
        overview_text(&mut rows, "No risks were listed.", width, 2, MUTED, None);
    }
    for risk in &analysis.risks {
        overview_gap(&mut rows);
        overview_text(&mut rows, &risk.title, width, 2, ACCENT, None);
        overview_text(&mut rows, &risk.detail, width, 4, TEXT, None);
        if !risk.files.is_empty() {
            overview_literal(
                &mut rows,
                &format!("Files: {}", risk.files.join(", ")),
                width,
                4,
                MUTED,
                None,
            );
        }
    }
    overview_heading(&mut rows, "Verification");
    overview_text(&mut rows, "Existing tests", width, 2, GREEN, None);
    if analysis.verification.existing_tests.is_empty() {
        overview_text(
            &mut rows,
            "None identified in the supplied context.",
            width,
            4,
            MUTED,
            None,
        );
    }
    for test in &analysis.verification.existing_tests {
        overview_literal(
            &mut rows,
            &format!("- {test}"),
            width,
            4,
            COMMENT_ACCENT,
            None,
        );
    }
    overview_text(&mut rows, "Suggested checks", width, 2, GREEN, None);
    if analysis.verification.suggested_checks.is_empty() {
        overview_text(
            &mut rows,
            "No additional checks were listed.",
            width,
            4,
            MUTED,
            None,
        );
    }
    for check in &analysis.verification.suggested_checks {
        overview_text(&mut rows, &format!("- {check}"), width, 4, TEXT, None);
    }
    overview_heading(&mut rows, "Unknowns");
    if analysis.verification.unknowns.is_empty() {
        overview_text(&mut rows, "No unknowns were listed.", width, 2, MUTED, None);
    }
    for unknown in &analysis.verification.unknowns {
        overview_text(&mut rows, &format!("- {unknown}"), width, 2, TEXT, None);
    }
    overview_heading(&mut rows, "Context coverage");
    overview_text(
        &mut rows,
        &format!(
            "{} files included · {} omitted · {} without a text diff",
            analysis.coverage.included_files.len(),
            analysis.coverage.omitted_files.len(),
            analysis.coverage.unavailable_files.len()
        ),
        width,
        2,
        MUTED,
        None,
    );
    for file in &analysis.coverage.omitted_files {
        overview_literal(
            &mut rows,
            &format!("Omitted: {file}"),
            width,
            2,
            MUTED,
            None,
        );
    }
    for file in &analysis.coverage.unavailable_files {
        overview_literal(
            &mut rows,
            &format!("No text diff: {file}"),
            width,
            2,
            MUTED,
            None,
        );
    }
    rows
}

fn draw_overview_rows(
    frame: &mut Frame,
    rows: &[OverviewRow],
    overview: &OverviewState,
    area: Rect,
) -> u16 {
    if area.width == 0 || area.height == 0 {
        return 0;
    }
    let height = usize::from(area.height);
    let requested = if overview.focus_guide {
        rows.iter()
            .position(|row| row.step == Some(overview.selected_step))
            .map_or(usize::from(overview.guide_scroll), |row| {
                row.saturating_sub(2)
            })
    } else {
        usize::from(overview.guide_scroll)
    };
    let start = requested.min(rows.len().saturating_sub(height));
    for (offset, row) in rows.iter().skip(start).take(height).enumerate() {
        let selected = row.step == Some(overview.selected_step);
        frame.render_widget(
            Paragraph::new(row.line.clone()).style(Style::default().bg(if selected {
                PANEL
            } else {
                BACKGROUND
            })),
            Rect::new(area.x, area.y + offset as u16, area.width, 1),
        );
    }
    draw_scrollbar(frame, area, rows.len(), start);
    start.min(u16::MAX as usize) as u16
}

fn draw_reading_guide(frame: &mut Frame, analysis: &Analysis, selected_step: usize, area: Rect) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(SIDEBAR));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [heading, list] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
    frame.render_widget(
        Paragraph::new(" Reading order\n Choose a step to open in Diff")
            .style(Style::default().fg(ACCENT)),
        heading,
    );
    if analysis.reading_order.is_empty() {
        frame.render_widget(
            Paragraph::new(" No reading order provided.").style(Style::default().fg(MUTED)),
            list,
        );
        return;
    }
    let items = analysis
        .reading_order
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let mut lines = wrap_comment(
                &reading_step_label(index, step),
                usize::from(list.width).saturating_sub(3).max(1),
            )
            .into_iter()
            .map(|line| {
                Line::from(Span::styled(
                    format!(" {line}"),
                    Style::default().fg(COMMENT_ACCENT),
                ))
            })
            .collect::<Vec<_>>();
            lines.extend(
                markdown_text(&step.reason, list.width.saturating_sub(3).max(1))
                    .lines
                    .into_iter()
                    .map(|mut line| {
                        line.spans.insert(0, Span::raw(" "));
                        line
                    }),
            );
            lines.push(Line::from(""));
            ListItem::new(lines)
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(selected_step));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(Style::default().bg(PANEL))
            .highlight_symbol("›"),
        list,
        &mut state,
    );
}

fn selection_label(review: &ReviewState) -> Option<String> {
    let (start, end) = review.selected_range()?;
    let count = end - start + 1;
    let lines = if count == 1 { "line" } else { "lines" };
    Some(
        if review
            .selection
            .as_ref()
            .is_some_and(|selection| selection.extending)
        {
            format!("SELECT {count} {lines}")
        } else {
            format!("{count} {lines} selected")
        },
    )
}

fn markdown_area(area: Rect) -> Rect {
    let inset = if area.width >= 8 { 1 } else { 0 };
    Rect::new(
        area.x + inset,
        area.y,
        area.width.saturating_sub(inset + 1).min(96),
        area.height,
    )
}

fn draw_scrollbar(frame: &mut Frame, area: Rect, content: usize, position: usize) {
    if content <= usize::from(area.height) || area.width == 0 || area.height == 0 {
        return;
    }
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .thumb_symbol("┃")
        .track_symbol(Some("│"))
        .thumb_style(Style::default().fg(ACCENT))
        .track_style(Style::default().fg(BORDER));
    let mut state = ScrollbarState::new(content)
        .position(position)
        .viewport_content_length(usize::from(area.height));
    frame.render_stateful_widget(scrollbar, area, &mut state);
}

fn markdown(frame: &mut Frame, body: &str, area: Rect, scroll: u16) -> u16 {
    let content = markdown_area(area);
    let text = markdown_text(body, content.width);
    let total = text.lines.len();
    let limit = total
        .saturating_sub(usize::from(content.height))
        .min(u16::MAX as usize) as u16;
    let scroll = scroll.min(limit);
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(TEXT))
            .scroll((scroll, 0)),
        content,
    );
    draw_scrollbar(frame, area, total, usize::from(scroll));
    scroll
}

fn draw_inbox(frame: &mut Frame, app: &mut App, area: Rect) {
    let selected = app
        .visible_prs()
        .get(app.selected_pr)
        .map(|pr| (*pr).clone());
    let bottom = app.preview.bottom.unwrap_or(area.width < 110);
    app.preview.actual_bottom = bottom;
    let [list_area, preview_area] = if !app.preview.shown {
        [area, Rect::default()]
    } else if bottom {
        Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area)
    } else {
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area)
    };
    let title = if app.filtering {
        format!(" PULL REQUESTS · /{}_ ", app.filter)
    } else if !app.filter.is_empty() {
        format!(" PULL REQUESTS · {} ", app.filter)
    } else {
        " PULL REQUESTS ".into()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(if !app.preview.focused { ACCENT } else { BORDER }));
    let inner = block.inner(list_area);
    frame.render_widget(block, list_area);
    let prs = app.visible_prs();
    if prs.is_empty() {
        frame.render_widget(
            Paragraph::new(if app.busy {
                "Loading pull requests…"
            } else {
                "No matching pull requests.
# opens a PR · / filters · r refreshes"
            })
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(MUTED)),
            inner,
        );
    } else {
        let rows = prs.iter().map(|pr| {
            Row::new(vec![
                Cell::from(if pr.draft { "D" } else { " " }),
                Cell::from(format!("#{}", pr.id.number)).style(Style::default().fg(ACCENT)),
                Cell::from(pr.title.clone()),
                Cell::from(pr.author.clone()).style(Style::default().fg(MUTED)),
            ])
        });
        let table = Table::new(
            rows,
            [
                Constraint::Length(1),
                Constraint::Length(6),
                Constraint::Min(12),
                Constraint::Length(if inner.width >= 50 { 12 } else { 0 }),
            ],
        )
        .header(
            Row::new(["", "PR", "TITLE", "AUTHOR"])
                .style(Style::default().fg(MUTED))
                .bottom_margin(1),
        )
        .column_spacing(1)
        .row_highlight_style(Style::default().bg(PANEL).fg(TEXT))
        .highlight_symbol("› ");
        frame.render_stateful_widget(
            table,
            inner,
            &mut TableState::default().with_selected(Some(app.selected_pr)),
        );
    }
    if preview_area.width == 0 || preview_area.height == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " PREVIEW · {}/{} focus · {} hides · {} position ",
            app.settings.keybindings.primary("focus_pr_list"),
            app.settings.keybindings.primary("focus_preview"),
            app.settings.keybindings.primary("toggle_preview"),
            app.settings.keybindings.primary("preview_position")
        ))
        .border_style(Style::default().fg(if app.preview.focused { ACCENT } else { BORDER }));
    let inner = block.inner(preview_area);
    frame.render_widget(block, preview_area);
    let Some(pr) = selected else {
        return;
    };
    let [tabs, title, content] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(1),
    ])
    .areas(inner);
    let style = |tab| {
        Style::default().fg(if app.preview.tab == tab {
            ACCENT
        } else {
            MUTED
        })
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" 1 Description ", style(PreviewTab::Description)),
            Span::styled(
                match app
                    .preview
                    .guides
                    .get(&crate::app::preview_key(&pr.id))
                    .map(|guide| &guide.status)
                {
                    Some(AiStatus::Loading) => " 2 AI guide · preparing ",
                    Some(AiStatus::Ready(_)) => " 2 AI guide · ready ",
                    Some(AiStatus::Error(_)) => " 2 AI guide · error ",
                    _ => " 2 AI guide ",
                },
                style(PreviewTab::Guide),
            ),
        ])),
        tabs,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                pr.title.clone(),
                Style::default().fg(TEXT).bold(),
            )),
            Line::from(Span::styled(
                format!(
                    "#{} · {}{}",
                    pr.id.number,
                    pr.author,
                    if pr.draft { " · draft" } else { "" }
                ),
                Style::default().fg(MUTED),
            )),
            Line::from(""),
        ]),
        title,
    );
    let body = if pr.body.trim().is_empty() {
        "No author description."
    } else {
        &pr.body
    };
    let guide = app.preview.guides.get(&crate::app::preview_key(&pr.id));
    let scroll = if app.preview.tab == PreviewTab::Description {
        markdown(frame, body, content, app.preview.scroll)
    } else if let Some(crate::app::PreviewGuide {
        status: AiStatus::Ready(analysis),
        revision,
    }) = guide
    {
        let mut rows = Vec::new();
        overview_text(
            &mut rows,
            &format!(
                "{} · revision {}",
                app.settings.ai.provider,
                &revision[..revision.len().min(8)]
            ),
            content.width,
            0,
            MUTED,
            None,
        );
        rows.extend(overview_rows(analysis, content.width, true));
        let start = usize::from(app.preview.scroll)
            .min(rows.len().saturating_sub(usize::from(content.height)));
        for (offset, row) in rows
            .iter()
            .skip(start)
            .take(usize::from(content.height))
            .enumerate()
        {
            frame.render_widget(
                Paragraph::new(row.line.clone()),
                Rect::new(content.x, content.y + offset as u16, content.width, 1),
            );
        }
        draw_scrollbar(frame, content, rows.len(), start);
        start.min(u16::MAX as usize) as u16
    } else {
        let hint = match guide.map(|guide| &guide.status) {
            Some(AiStatus::Loading) => format!("*Preparing with {}…*", app.settings.ai.provider),
            Some(AiStatus::Error(error)) => {
                format!("**Guide unavailable:** {error}\n\nUse the command palette to retry.")
            }
            _ => "`a`: prepare guide · `A`: choose agent".into(),
        };
        markdown(
            frame,
            &format!("{hint}\n\n{body}"),
            content,
            app.preview.scroll,
        )
    };
    app.preview.scroll = scroll;
}

/// Conversation with the AI agent, docked beside the review or drawn over it when the
/// terminal is too narrow; returns the transcript scroll actually used.
fn draw_chat(frame: &mut Frame, app: &App, area: Rect, focused: bool) -> u16 {
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .title(format!(" AI chat · {} ", app.settings.ai.provider))
        .border_style(Style::default().fg(if focused { ACCENT } else { BORDER }))
        .style(Style::default().fg(TEXT).bg(PANEL));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let scroll = app.chat_scroll;
    let [scope, transcript, editor_label, editor] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(2),
        Constraint::Length(1),
        Constraint::Length(4),
    ])
    .areas(inner);
    let attached = match app.chat_focus() {
        Ok(Some(focus)) => format!("Attached: {}", focus.label),
        Ok(None) => format!(
            "Attached: entire changeset · open Diff ({}) to attach lines",
            app.shortcut("diff")
        ),
        Err(error) => format!("Nothing attached: {error}"),
    };
    frame.render_widget(
        Paragraph::new(attached)
            .style(Style::default().fg(ACCENT))
            .wrap(Wrap { trim: false }),
        scope,
    );
    let mut text = String::new();
    let mut latest_start = 0;
    for turn in &app.chat.turns {
        text.push_str(&format!(
            "**You** · {}\n\n{}\n\n",
            turn.scope, turn.question
        ));
        latest_start = text.len();
        text.push_str(&format!(
            "**{}**\n\n{}\n\n",
            app.settings.ai.provider, turn.response.answer
        ));
        for reference in &turn.response.references {
            text.push_str(&format!(
                "- `{}{} · {:?}` — {}\n",
                reference.path,
                reference
                    .line
                    .map_or(String::new(), |line| format!(":{line}")),
                reference.side,
                reference.reason
            ));
        }
        if !turn.response.unknowns.is_empty() {
            text.push_str("\n**Unknowns**\n");
        }
        for unknown in &turn.response.unknowns {
            text.push_str(&format!("- {unknown}\n"));
        }
        text.push_str(&format!(
            "\n*Context: {} files included · {} omitted · {} unavailable.*\n\n---\n\n",
            turn.response.coverage.included_files.len(),
            turn.response.coverage.omitted_files.len(),
            turn.response.coverage.unavailable_files.len()
        ));
    }
    if let Some((question, scope)) = &app.chat.pending {
        latest_start = text.len();
        text.push_str(&format!(
            "**You** · {scope}\n\n{question}\n\n*{} is thinking…*",
            app.settings.ai.provider
        ));
    }
    if let Some(error) = &app.chat.error {
        latest_start = text.len();
        text.push_str(&format!(
            "\n\n**Request failed:** {error}\n\nYou can edit and resend your question."
        ));
    }
    if text.is_empty() {
        text.push_str("Ask about the behavior, intent, risks, or tests. On the Diff tab the line under the cursor, or the selection, is attached when you send, along with its surrounding hunks.\n\nFollow-up questions keep the earlier turns. Leave the panel, select other lines, and ask again to bring them into the same thread.\n\nCtrl-d / Ctrl-u scroll the conversation.");
    }
    let requested = if scroll == u16::MAX {
        markdown_text(&text[..latest_start], markdown_area(transcript).width)
            .lines
            .len()
            .min(u16::MAX as usize) as u16
    } else {
        scroll
    };
    let actual = markdown(frame, &text, transcript, requested);
    let hint = if !focused {
        format!(
            "{} type a question · {} hide",
            app.shortcut("ask"),
            app.shortcut("toggle_chat")
        )
    } else if app.chat.pending.is_some() {
        "Thinking… · Ctrl-x cancels · Esc back to review".into()
    } else {
        "Ctrl-s send · Ctrl-n new thread · Esc back to review".into()
    };
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(MUTED)),
        editor_label,
    );
    if focused {
        draw_input(frame, &app.chat_input, editor);
    } else {
        frame.render_widget(
            Paragraph::new(app.chat_input.text.as_str())
                .style(Style::default().fg(MUTED))
                .wrap(Wrap { trim: false }),
            editor,
        );
    }
    actual
}

struct SidebarRow {
    line: Line<'static>,
    file: Option<usize>,
}

struct DirectorySegment {
    path: String,
    label: String,
}

/// Groups each file's parent directories into displayed segments, joining chains of
/// directories that contain only one subdirectory so deep package paths stay shallow.
fn directory_segments(filenames: &[&str]) -> Vec<Vec<DirectorySegment>> {
    let mut children = HashMap::<String, HashSet<&str>>::new();
    for filename in filenames {
        let parts = filename.split('/').collect::<Vec<_>>();
        for depth in 0..parts.len() {
            children
                .entry(parts[..depth].join("/"))
                .or_default()
                .insert(parts[depth]);
        }
    }
    filenames
        .iter()
        .map(|filename| {
            let parts = filename.split('/').collect::<Vec<_>>();
            let directories = parts.len().saturating_sub(1);
            let mut segments = Vec::new();
            let mut depth = 0;
            while depth < directories {
                let start = depth;
                while depth + 1 < directories
                    && children
                        .get(&parts[..=depth].join("/"))
                        .is_some_and(|names| names.len() == 1)
                {
                    depth += 1;
                }
                segments.push(DirectorySegment {
                    path: parts[..=depth].join("/"),
                    label: parts[start..=depth].join("/"),
                });
                depth += 1;
            }
            segments
        })
        .collect()
}

fn sidebar_indent(depth: usize, width: u16) -> String {
    " ".repeat((depth * 2).min(usize::from(width) / 3))
}

fn sidebar_rows(review: &ReviewState, width: u16) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    let filenames = review
        .data
        .files
        .iter()
        .map(|file| file.filename.as_str())
        .collect::<Vec<_>>();
    let mut open = Vec::<String>::new();
    for ((index, file), segments) in review
        .data
        .files
        .iter()
        .enumerate()
        .zip(directory_segments(&filenames))
    {
        let shared = open
            .iter()
            .zip(&segments)
            .take_while(|(open, segment)| **open == segment.path)
            .count();
        open.truncate(shared);
        for segment in &segments[shared..] {
            let prefix = format!(" {}▾ ", sidebar_indent(open.len(), width));
            let room = usize::from(width).saturating_sub(Line::from(prefix.as_str()).width());
            rows.push(SidebarRow {
                file: None,
                line: Line::from(Span::styled(
                    format!("{prefix}{}", truncate_path(&segment.label, room)),
                    Style::default().fg(MUTED),
                )),
            });
            open.push(segment.path.clone());
        }
        let parts = file.filename.split('/').collect::<Vec<_>>();
        let indentation = sidebar_indent(segments.len(), width);
        let drafts = review
            .drafts
            .iter()
            .filter(|draft| draft.path == file.filename)
            .count();
        let status = match file.status.as_str() {
            "added" => "A",
            "removed" => "D",
            "renamed" => "R",
            _ => "M",
        };
        let status_color = match file.status.as_str() {
            "added" => GREEN,
            "removed" => RED,
            _ => ACCENT,
        };
        let counts = if width >= 27 {
            format!(" +{} -{}", file.additions, file.deletions)
        } else {
            String::new()
        };
        let draft_marker = if drafts > 0 {
            format!(" *{drafts}")
        } else {
            String::new()
        };
        let prefix = format!(" {indentation}{status} ");
        let remaining = usize::from(width).saturating_sub(
            Line::from(prefix.as_str()).width() + counts.len() + draft_marker.len(),
        );
        let filename = truncate(parts.last().copied().unwrap_or(&file.filename), remaining);
        rows.push(SidebarRow {
            file: Some(index),
            line: Line::from(vec![
                Span::styled(prefix, Style::default().fg(status_color)),
                Span::styled(filename, Style::default().fg(TEXT)),
                Span::styled(draft_marker, Style::default().fg(ACCENT)),
                Span::styled(counts, Style::default().fg(MUTED)),
            ]),
        });
    }
    rows
}

fn draw_review(frame: &mut Frame, app: &App, area: Rect) {
    let review = app.review.as_ref().unwrap();
    let width = if area.width >= 160 {
        40
    } else if area.width >= 100 {
        31
    } else if area.width >= 70 {
        23
    } else {
        16
    };
    let [files_area, diff_area] = if app.file_sidebar {
        Layout::horizontal([Constraint::Length(width), Constraint::Min(20)]).areas(area)
    } else {
        [Rect::default(), area]
    };
    if app.file_sidebar {
        let sidebar = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(BORDER))
            .style(Style::default().bg(SIDEBAR));
        let inner = sidebar.inner(files_area);
        frame.render_widget(sidebar, files_area);
        let [files_header, files_body] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
        frame.render_widget(
            Paragraph::new(format!(
                " {}CHANGES · {}",
                if review.focus == Focus::Files {
                    "› "
                } else {
                    ""
                },
                review.data.files.len()
            ))
            .style(Style::default().fg(if review.focus == Focus::Files {
                ACCENT
            } else {
                MUTED
            })),
            files_header,
        );
        let rows = sidebar_rows(review, files_body.width);
        let selected = rows
            .iter()
            .position(|row| row.file == Some(review.selected_file))
            .unwrap_or(0);
        let mut state = ListState::default().with_selected(Some(selected));
        frame.render_stateful_widget(
            List::new(
                rows.into_iter()
                    .map(|row| ListItem::new(row.line))
                    .collect::<Vec<_>>(),
            )
            .highlight_style(Style::default().bg(PANEL))
            .highlight_symbol(""),
            files_body,
            &mut state,
        );
    }
    let [file_header, content] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(diff_area);
    draw_file_header(frame, review, app.comments_loading, file_header);
    let split = content.width >= 60
        && app.diff_split.unwrap_or(match app.settings.diff.layout {
            DiffLayout::Unified => false,
            DiffLayout::Split => true,
            DiffLayout::Auto => frame.area().width >= 160,
        });
    app.split_shown.set(split);
    review.split.set(split);
    let rows = review_rows(review, content.width, split);
    let height = usize::from(content.height);
    let selected = selected_row(&rows, review.position());
    let viewport = &review.viewport;
    let previous = viewport.tops.borrow().get(&review.selected_file).copied();
    let start = match previous {
        Some(top) => scroll_viewport(
            top,
            selected,
            rows.len(),
            height,
            app.settings.editor.scrolloff,
            viewport
                .keep_offset
                .replace(false)
                .then(|| viewport.cursor_offset.get()),
        ),
        None => {
            viewport.keep_offset.set(false);
            viewport_start(&rows, review.position(), height)
        }
    };
    viewport
        .tops
        .borrow_mut()
        .insert(review.selected_file, start);
    viewport.cursor_offset.set(selected.saturating_sub(start));
    for (offset, row) in rows
        .into_iter()
        .skip(start)
        .take(usize::from(content.height))
        .enumerate()
    {
        frame.render_widget(
            Paragraph::new(row.line).style(Style::default().bg(row.background).fg(TEXT)),
            Rect::new(content.x, content.y + offset as u16, content.width, 1),
        );
    }
}

fn draw_file_header(frame: &mut Frame, review: &ReviewState, comments_loading: bool, area: Rect) {
    let Some(file) = review.current_file() else {
        return;
    };
    let title = file.previous_filename.as_ref().map_or_else(
        || file.filename.clone(),
        |old| format!("{old} → {}", file.filename),
    );
    let comments = threads(&review.comments())
        .iter()
        .filter(|thread| thread.matches_file(file))
        .map(|thread| thread.replies.len() + 1)
        .sum::<usize>();
    let drafts = review
        .drafts
        .iter()
        .filter(|draft| draft.path == file.filename)
        .count();
    let [heading, metadata] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let stats_width = format!(" +{} -{} ", file.additions, file.deletions)
        .len()
        .min(usize::from(area.width.saturating_sub(8))) as u16;
    let [path, stats] =
        Layout::horizontal([Constraint::Min(8), Constraint::Length(stats_width)]).areas(heading);
    frame.render_widget(Block::default().style(Style::default().bg(PANEL)), area);
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!(" {title}"),
            Style::default().fg(TEXT).bold(),
        )),
        path,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" +{}", file.additions), Style::default().fg(GREEN)),
            Span::styled(format!(" -{} ", file.deletions), Style::default().fg(RED)),
        ]))
        .right_aligned(),
        stats,
    );
    let context = &review.contexts[review.selected_file];
    let context_label = if context.loading {
        " · context loading…"
    } else if context.expansion.full {
        " · full file"
    } else if context.expansion.radius.values().any(|radius| *radius > 0) {
        " · expanded"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(if review.data.source.is_local() {
            format!(
                " {comments} local notes · {}{context_label}",
                if review.show_comments {
                    "shown"
                } else {
                    "hidden"
                }
            )
        } else {
            format!(
                " {} · {drafts} draft{} · {}{context_label}",
                if comments_loading {
                    "comments loading".into()
                } else {
                    format!("{comments} comments")
                },
                if drafts == 1 { "" } else { "s" },
                if review.show_comments {
                    "shown"
                } else {
                    "hidden"
                }
            )
        })
        .style(Style::default().fg(MUTED)),
        metadata,
    );
}

struct ViewRow {
    line: Line<'static>,
    source: Option<usize>,
    /// Right-hand line index when a split row shows two different unified lines.
    alt_source: Option<usize>,
    background: Color,
}

impl ViewRow {
    fn text(text: impl Into<String>, background: Color, foreground: Color) -> Self {
        Self {
            line: Line::from(Span::styled(text.into(), Style::default().fg(foreground))),
            source: None,
            alt_source: None,
            background,
        }
    }
}

fn review_rows(review: &ReviewState, width: u16, split: bool) -> Vec<ViewRow> {
    let mut rows = Vec::new();
    let Some(file) = review.current_file() else {
        return rows;
    };
    let lines = review.current_lines();
    let comments = review.comments();
    let all_threads = threads(&comments);
    let mut anchored = BTreeMap::<usize, Vec<&Thread<'_>>>::new();
    let mut unmatched = Vec::new();
    for thread in all_threads
        .iter()
        .filter(|thread| thread.matches_file(file))
    {
        let location = thread.anchor().and_then(|(side, number)| {
            lines
                .iter()
                .position(|line| matches_anchor(line, &side, number))
        });
        if let Some(location) = location {
            anchored.entry(location).or_default().push(thread);
        } else {
            unmatched.push(thread);
        }
    }
    let mut drafts = BTreeMap::new();
    let mut unmatched_drafts = Vec::new();
    for draft in review
        .drafts
        .iter()
        .filter(|draft| draft.path == file.filename)
    {
        if let Some(location) = lines
            .iter()
            .position(|line| matches_anchor(line, &draft.side, draft.line))
        {
            drafts.entry(location).or_insert_with(Vec::new).push(draft);
        } else {
            unmatched_drafts.push(draft);
        }
    }
    if lines.is_empty() {
        rows.push(ViewRow::text(
            " No text diff available for this file.",
            BACKGROUND,
            MUTED,
        ));
        if !review.show_comments || (unmatched.is_empty() && unmatched_drafts.is_empty()) {
            rows.push(ViewRow::text(
                " Source unavailable here · use the command palette",
                BACKGROUND,
                MUTED,
            ));
        }
    }
    let digits = lines
        .iter()
        .flat_map(|line| [line.old_line, line.new_line])
        .flatten()
        .max()
        .unwrap_or(0)
        .to_string()
        .len()
        .max(3);
    let state = |index: usize, line: &DiffLine| RowState {
        selected: review
            .selected_range()
            .map_or(index == review.position(), |(start, end)| {
                (start..=end).contains(&index)
            }),
        cursor: index == review.position(),
        has_thread: anchored.contains_key(&index),
        has_draft: drafts.contains_key(&index),
        mark: mark_for(&review.marks, &file.filename, line),
    };
    let row = |index: usize, gutter: Gutter| {
        let line = &lines[index];
        let mut row = code_row(
            line,
            file,
            state(index, line),
            review.horizontal,
            digits,
            gutter,
        );
        row.source = Some(index);
        row
    };
    let groups = if split {
        prpr::diff::split_pairs(lines)
    } else {
        (0..lines.len()).map(|index| (Some(index), None)).collect()
    };
    for (left, right) in groups {
        let first = left.or(right).unwrap_or_default();
        if lines[first].kind == LineKind::Hunk && first > 0 {
            rows.push(ViewRow::text("", BACKGROUND, MUTED));
        }
        let full_width = !split || matches!(lines[first].kind, LineKind::Hunk | LineKind::Meta);
        rows.push(if full_width {
            row(first, Gutter::Both)
        } else {
            split_row(
                left.map(|index| row(index, Gutter::Old)),
                right.map(|index| row(index, Gutter::New)),
                width,
            )
        });
        if !review.show_comments {
            continue;
        }
        let mut indices = vec![first];
        if let Some(right) = right.filter(|right| *right != first) {
            indices.push(right);
        }
        for index in indices {
            if let Some(comments) = anchored.get(&index) {
                for thread in comments {
                    append_thread(&mut rows, thread, width, None);
                }
            }
            if let Some(comments) = drafts.get(&index) {
                for draft in comments {
                    append_card(
                        &mut rows,
                        &format!(
                            "draft · {}:{} · local",
                            side_label(&draft.side),
                            draft.line_label()
                        ),
                        &draft.body,
                        width,
                        true,
                        false,
                    );
                }
            }
        }
    }
    if review.show_comments && !unmatched.is_empty() {
        if !lines.is_empty() {
            rows.push(ViewRow::text("", BACKGROUND, MUTED));
        }
        rows.push(ViewRow::text(" Threads outside this diff", PANEL, MUTED));
        for thread in unmatched {
            let label = if thread.root.line.is_none() {
                "outdated"
            } else {
                "outside displayed diff"
            };
            append_thread(&mut rows, thread, width, Some(label));
        }
    }
    if review.show_comments && !unmatched_drafts.is_empty() {
        rows.push(ViewRow::text(" Drafts outside this diff", PANEL, MUTED));
        for draft in unmatched_drafts {
            append_card(
                &mut rows,
                &format!(
                    "unmatched draft · {}:{}",
                    side_label(&draft.side),
                    draft.line_label()
                ),
                &draft.body,
                width,
                true,
                false,
            );
        }
    }
    rows
}

struct RowState {
    selected: bool,
    cursor: bool,
    has_thread: bool,
    has_draft: bool,
    /// Highlighted character range in source columns; `(1, u32::MAX)` covers the whole line.
    mark: Option<(u32, u32)>,
}

const MARK: Color = Color::Rgb(96, 76, 24);

fn mark_for(marks: &[prpr::model::Mark], path: &str, line: &DiffLine) -> Option<(u32, u32)> {
    marks
        .iter()
        .filter(|mark| mark.path == path)
        .find(|mark| {
            (mark.line..=mark.end_line)
                .any(|number| prpr::thread::matches_anchor(line, &mark.side, number))
        })
        .map(|mark| {
            (
                mark.start_column.unwrap_or(1),
                mark.end_column.unwrap_or(u32::MAX),
            )
        })
}

/// Converts 1-based inclusive source columns to the displayed character range, accounting for
/// four-column tab expansion and horizontal scrolling.
fn display_range(source: &str, (start, end): (u32, u32), horizontal: u16) -> (usize, usize) {
    let mut display = 0;
    let mut range = (usize::MAX, usize::MAX);
    for (column, character) in source.chars().enumerate() {
        let column = column as u32 + 1;
        if column == start {
            range.0 = display;
        }
        display += if character == '\t' { 4 } else { 1 };
        if column == end {
            range.1 = display;
        }
    }
    if range.0 == usize::MAX {
        return (0, 0);
    }
    if range.1 == usize::MAX {
        range.1 = display;
    }
    let shift = usize::from(horizontal);
    (range.0.saturating_sub(shift), range.1.saturating_sub(shift))
}

fn highlight_spans(spans: Vec<Span<'static>>, (start, end): (usize, usize)) -> Vec<Span<'static>> {
    let mut result = Vec::new();
    let mut position = 0;
    for span in spans {
        let characters = span.content.chars().collect::<Vec<_>>();
        let length = characters.len();
        let from = start.clamp(position, position + length) - position;
        let to = end.clamp(position, position + length) - position;
        for (range, marked) in [(0..from, false), (from..to, true), (to..length, false)] {
            if range.is_empty() {
                continue;
            }
            let style = if marked {
                span.style.bg(MARK)
            } else {
                span.style
            };
            result.push(Span::styled(
                characters[range].iter().collect::<String>(),
                style,
            ));
        }
        position += length;
    }
    result
}

#[derive(Clone, Copy)]
enum Gutter {
    Both,
    Old,
    New,
}

fn code_row(
    line: &DiffLine,
    file: &ChangedFile,
    state: RowState,
    horizontal: u16,
    digits: usize,
    gutter_numbers: Gutter,
) -> ViewRow {
    let background = match (line.kind, state.selected) {
        (LineKind::Addition, true) => SELECTED_ADDITION,
        (LineKind::Deletion, true) => SELECTED_DELETION,
        (_, true) => SELECTED,
        (LineKind::Addition, false) => ADDITION,
        (LineKind::Deletion, false) => DELETION,
        (LineKind::Hunk, false) => PANEL,
        _ => BACKGROUND,
    };
    let gutter = Style::default()
        .fg(if state.cursor {
            ACCENT
        } else if state.selected {
            TEXT
        } else {
            MUTED
        })
        .bg(background);
    if matches!(line.kind, LineKind::Hunk | LineKind::Meta) {
        return ViewRow {
            source: None,
            alt_source: None,
            background,
            line: Line::from(vec![
                Span::styled(
                    if state.cursor {
                        "› "
                    } else if state.selected {
                        "│ "
                    } else {
                        "  "
                    },
                    gutter,
                ),
                Span::styled(
                    line.text.clone(),
                    Style::default().fg(if line.kind == LineKind::Hunk {
                        MUTED
                    } else {
                        TEXT
                    }),
                ),
            ]),
        };
    }
    let old = line
        .old_line
        .map_or_else(String::new, |number| number.to_string());
    let new = line
        .new_line
        .map_or_else(String::new, |number| number.to_string());
    let sign = match line.kind {
        LineKind::Addition => "+",
        LineKind::Deletion => "-",
        _ => " ",
    };
    let sign_color = match line.kind {
        LineKind::Addition => GREEN,
        LineKind::Deletion => RED,
        _ => MUTED,
    };
    let marker = if state.cursor {
        "›"
    } else if state.has_draft {
        "*"
    } else if state.has_thread {
        "+"
    } else if state.selected {
        "│"
    } else {
        " "
    };
    let code = line
        .text
        .get(1..)
        .unwrap_or(&line.text)
        .replace('\t', "    ");
    let code = code
        .chars()
        .skip(usize::from(horizontal))
        .collect::<String>();
    let mut spans = vec![
        Span::styled(
            match gutter_numbers {
                Gutter::Both => format!("{marker}{old:>digits$} {new:>digits$} "),
                Gutter::Old => format!("{marker}{old:>digits$} "),
                Gutter::New => format!("{marker}{new:>digits$} "),
            },
            gutter,
        ),
        Span::styled(format!("{sign} "), Style::default().fg(sign_color)),
    ];
    let syntax = syntax_spans(&code, &file.filename);
    spans.extend(match state.mark {
        Some(columns) => highlight_spans(
            syntax,
            display_range(line.text.get(1..).unwrap_or_default(), columns, horizontal),
        ),
        None => syntax,
    });
    ViewRow {
        line: Line::from(spans),
        source: None,
        alt_source: None,
        background,
    }
}

/// Truncates or pads a half row to `width` display columns, painting `background` behind
/// spans that do not set their own.
fn fit_spans(spans: Vec<Span<'static>>, width: usize, background: Color) -> Vec<Span<'static>> {
    let mut fitted = Vec::new();
    let mut used = 0;
    for span in spans {
        let style = if span.style.bg.is_none() {
            span.style.bg(background)
        } else {
            span.style
        };
        let mut text = String::new();
        for character in span.content.chars() {
            let next = Span::raw(character.to_string()).width();
            if used + next > width {
                break;
            }
            text.push(character);
            used += next;
        }
        if !text.is_empty() {
            fitted.push(Span::styled(text, style));
        }
        if used >= width {
            break;
        }
    }
    if used < width {
        fitted.push(Span::styled(
            " ".repeat(width - used),
            Style::default().bg(background),
        ));
    }
    fitted
}

fn split_row(left: Option<ViewRow>, right: Option<ViewRow>, width: u16) -> ViewRow {
    let content = usize::from(width.saturating_sub(1));
    let halves = [content / 2, content - content / 2];
    let mut spans = Vec::new();
    let mut sources = [None, None];
    for (side, row) in [left, right].into_iter().enumerate() {
        if side == 1 {
            spans.push(Span::styled(
                "│",
                Style::default().fg(BORDER).bg(BACKGROUND),
            ));
        }
        let (line, background) = match row {
            Some(row) => {
                sources[side] = row.source;
                (row.line, row.background)
            }
            None => (Line::default(), BACKGROUND),
        };
        spans.extend(fit_spans(line.spans, halves[side], background));
    }
    ViewRow {
        line: Line::from(spans),
        source: sources[0].or(sources[1]),
        alt_source: sources[1].filter(|_| sources[0].is_some()),
        background: BACKGROUND,
    }
}

fn syntax_spans(code: &str, filename: &str) -> Vec<Span<'static>> {
    let extension = filename.rsplit('.').next().unwrap_or("");
    if !matches!(
        extension,
        "rs" | "go"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "py"
            | "rb"
            | "c"
            | "cpp"
            | "h"
            | "java"
            | "kt"
            | "swift"
            | "json"
            | "toml"
            | "yaml"
            | "yml"
            | "sh"
    ) {
        return vec![Span::styled(code.to_owned(), Style::default().fg(TEXT))];
    }
    let mut spans = Vec::new();
    let mut rest = code;
    while let Some(first) = rest.chars().next() {
        let (length, color) = if rest.starts_with("//")
            || (rest.starts_with('#')
                && matches!(extension, "py" | "rb" | "toml" | "yaml" | "yml" | "sh"))
        {
            (rest.len(), MUTED)
        } else if first == '"' || (first == '\'' && rest[first.len_utf8()..].contains('\'')) {
            let mut escaped = false;
            let mut end = first.len_utf8();
            for (index, character) in rest[first.len_utf8()..].char_indices() {
                end = first.len_utf8() + index + character.len_utf8();
                if !escaped && character == first {
                    break;
                }
                escaped = !escaped && character == '\\';
            }
            (end, Color::Rgb(178, 194, 142))
        } else if first.is_alphanumeric() || first == '_' {
            let end = rest
                .char_indices()
                .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
                .map_or(rest.len(), |(index, _)| index);
            let word = &rest[..end];
            let color = if first.is_ascii_digit() {
                Color::Rgb(209, 164, 118)
            } else if matches!(
                word,
                "pub"
                    | "fn"
                    | "let"
                    | "mut"
                    | "impl"
                    | "self"
                    | "Self"
                    | "return"
                    | "if"
                    | "else"
                    | "match"
                    | "for"
                    | "in"
                    | "while"
                    | "loop"
                    | "use"
                    | "mod"
                    | "struct"
                    | "enum"
                    | "trait"
                    | "const"
                    | "static"
                    | "async"
                    | "await"
                    | "move"
                    | "ref"
                    | "true"
                    | "false"
                    | "None"
                    | "Some"
                    | "class"
                    | "function"
                    | "import"
                    | "from"
                    | "export"
                    | "def"
                    | "try"
                    | "catch"
                    | "throw"
                    | "new"
                    | "var"
                    | "package"
                    | "func"
                    | "type"
                    | "interface"
            ) {
                Color::Rgb(186, 164, 202)
            } else if first.is_uppercase() {
                Color::Rgb(143, 185, 188)
            } else if rest[end..].trim_start().starts_with(['(', '!']) {
                Color::Rgb(214, 192, 150)
            } else {
                TEXT
            };
            (end, color)
        } else {
            (first.len_utf8(), TEXT)
        };
        spans.push(Span::styled(
            rest[..length].to_owned(),
            Style::default().fg(color),
        ));
        rest = &rest[length..];
    }
    spans
}

fn append_thread(rows: &mut Vec<ViewRow>, thread: &Thread<'_>, width: u16, location: Option<&str>) {
    let anchor = thread.anchor().map_or_else(
        || {
            format!(
                "originally {}:{}",
                thread.root.side,
                thread
                    .root
                    .original_line
                    .map_or_else(|| "?".into(), |line| line.to_string())
            )
        },
        |(side, _)| format!("{}:{}", side_label(&side), thread.root.line_label()),
    );
    let replies = if thread.replies.is_empty() {
        String::new()
    } else {
        format!(
            " · {} {}",
            thread.replies.len(),
            if thread.replies.len() == 1 {
                "reply"
            } else {
                "replies"
            }
        )
    };
    let title = if let Some(label) = location {
        format!("{label} · {}{replies} · {anchor}", thread.root.author)
    } else if thread.root.line_label().contains('–') {
        format!("{anchor} · {}{replies}", thread.root.author)
    } else {
        format!("{}{replies} · {anchor}", thread.root.author)
    };
    append_card(
        rows,
        &title,
        &thread.root.body,
        width,
        false,
        !thread.replies.is_empty(),
    );
    for (index, reply) in thread.replies.iter().enumerate() {
        append_card(
            rows,
            &format!("↳ {} · reply", reply.author),
            &reply.body,
            width,
            false,
            index + 1 < thread.replies.len(),
        );
    }
}

fn append_card(
    rows: &mut Vec<ViewRow>,
    title: &str,
    body: &str,
    width: u16,
    draft: bool,
    continues: bool,
) {
    let background = if draft { DRAFT } else { COMMENT };
    let accent = if draft { ACCENT } else { COMMENT_ACCENT };
    let available = usize::from(width).saturating_sub(5).max(1);
    rows.push(ViewRow::text(
        format!("  ╭ {}", truncate(title, available)),
        background,
        accent,
    ));
    let wrapped = wrap_comment(body, available);
    for line in wrapped.iter().take(6) {
        rows.push(ViewRow::text(format!("  │ {line}"), background, TEXT));
    }
    if wrapped.len() > 6 {
        rows.push(ViewRow::text(
            if draft {
                "  │ … open saved drafts"
            } else {
                "  │ … open full thread"
            },
            background,
            MUTED,
        ));
    }
    rows.push(ViewRow::text(
        if continues { "  │" } else { "  ╰" },
        background,
        accent,
    ));
}

fn side_label(side: &Side) -> &'static str {
    if *side == Side::Left { "LEFT" } else { "RIGHT" }
}

fn selected_row(rows: &[ViewRow], source: usize) -> usize {
    rows.iter()
        .position(|row| row.source == Some(source) || row.alt_source == Some(source))
        .unwrap_or(0)
}

/// First view of a file: the cursor sits a few rows below the top.
fn viewport_start(rows: &[ViewRow], source: usize, height: usize) -> usize {
    selected_row(rows, source)
        .saturating_sub((height / 4).min(3))
        .min(rows.len().saturating_sub(height))
}

/// Scrolls only as far as needed to keep `scrolloff` rows around the cursor; a cursor that
/// jumped out of view is centered, and `keep_offset` preserves its screen row for paging.
fn scroll_viewport(
    previous_top: usize,
    selected: usize,
    total: usize,
    height: usize,
    scrolloff: usize,
    keep_offset: Option<usize>,
) -> usize {
    if total <= height || height == 0 {
        return 0;
    }
    let margin = scrolloff.min(height.saturating_sub(1) / 2);
    let top = if let Some(offset) = keep_offset {
        selected.saturating_sub(offset)
    } else if selected < previous_top || selected >= previous_top + height {
        selected.saturating_sub(height / 2)
    } else if selected < previous_top + margin {
        selected.saturating_sub(margin)
    } else if selected + margin >= previous_top + height {
        selected + margin + 1 - height
    } else {
        previous_top
    };
    top.min(total - height)
}

fn truncate(text: &str, width: usize) -> String {
    if Line::from(text).width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for character in text.chars() {
        let next = Line::from(character.to_string()).width();
        if used + next >= width {
            break;
        }
        result.push(character);
        used += next;
    }
    result.push('…');
    result
}

/// Shortens a directory chain while keeping its first and last names, which tell sibling
/// branches such as `main/…` and `test/…` apart.
fn truncate_path(path: &str, width: usize) -> String {
    if Line::from(path).width() <= width {
        return path.to_owned();
    }
    if let (Some((first, _)), Some((_, last))) = (path.split_once('/'), path.rsplit_once('/')) {
        let middle = format!("{first}/…/{last}");
        if Line::from(middle.as_str()).width() <= width {
            return middle;
        }
    }
    truncate(path, width)
}

fn wrap_comment(text: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    for logical in text.lines() {
        let mut rest = logical;
        loop {
            let mut used = 0;
            let mut end = 0;
            let mut space = None;
            for (index, character) in rest.char_indices() {
                let next = Line::from(character.to_string()).width();
                if used + next > width && end > 0 {
                    break;
                }
                used += next;
                end = index + character.len_utf8();
                if character.is_whitespace() {
                    space = Some(index);
                }
            }
            if end == rest.len() {
                result.push(rest.to_owned());
                break;
            }
            let split = space.filter(|&index| index > 0).unwrap_or(end);
            result.push(rest[..split].trim_end().to_owned());
            rest = rest[split..].trim_start();
        }
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

fn popup(frame: &mut Frame, title: String, width: u16, height: u16) -> Rect {
    let area = frame.area();
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2));
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(title)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().fg(TEXT).bg(PANEL));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

fn draw_ai_review(frame: &mut Frame, app: &App, scroll: u16) -> u16 {
    let status = app.review.as_ref().and_then(|review| {
        app.ai_reviews
            .get(&crate::app::ai_review_key(&review.data.pr))
    });
    let (title, body) = match status {
        Some(AiReviewStatus::Ready {
            provider,
            text,
            notes,
            unanchored,
            anchored,
        }) => (
            format!(
                " {provider} review / {notes} findings as notes, {unanchored} text-only / r reruns / Esc closes "
            ),
            if *anchored {
                text.clone()
            } else {
                format!(
                    "> Your checkout was not at the PR head, so line numbers below may refer to other code and no notes were placed. Press w in Diff to check it out, then r here to rerun.\n\n{text}"
                )
            },
        ),
        Some(AiReviewStatus::Error(error)) => (
            " AI review failed / r retries / Esc closes ".to_owned(),
            format!("> {error}"),
        ),
        Some(AiReviewStatus::Running(provider)) => (
            format!(" {provider} review / running / Esc closes "),
            "The review is still running.".to_owned(),
        ),
        None => (
            " AI review / Esc closes ".to_owned(),
            "No review has run for this PR head.".to_owned(),
        ),
    };
    let inner = popup(frame, title, 110, frame.area().height.saturating_sub(4));
    markdown(frame, &body, inner, scroll)
}

fn draw_description(frame: &mut Frame, app: &App, scroll: u16) -> u16 {
    let inner = popup(
        frame,
        " PR description / j,k scroll / Esc closes ".into(),
        100,
        frame.area().height.saturating_sub(4),
    );
    let details = app
        .overview
        .as_ref()
        .map(|state| {
            let pr = &state.metadata;
            (&pr.title, &pr.url, &pr.author, pr.draft, &pr.body)
        })
        .or_else(|| {
            app.review.as_ref().map(|state| {
                let pr = &state.data.pr;
                (&pr.title, &pr.url, &pr.author, pr.draft, &pr.body)
            })
        });
    let body = details.map_or_else(
        || "No pull request is selected.".into(),
        |(title, url, author, draft, body)| {
            format!(
                "# {title}\n\nby {author}{}\n\n{url}\n\n{body}",
                if draft { " (draft)" } else { "" }
            )
        },
    );
    markdown(frame, &body, inner, scroll)
}

fn draw_dialog(frame: &mut Frame, app: &App, dialog: &Dialog) {
    match dialog {
        Dialog::Palette { input, selected } => {
            let inner = popup(frame, " Command palette · Esc closes ".into(), 92, 23);
            let [search, _, choices, count, hint] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            let [prompt, field] =
                Layout::horizontal([Constraint::Length(2), Constraint::Min(0)]).areas(search);
            frame.render_widget(
                Paragraph::new("> ").style(Style::default().fg(ACCENT)),
                prompt,
            );
            if input.text.is_empty() {
                frame.render_widget(
                    Paragraph::new("Type to filter commands…").style(Style::default().fg(MUTED)),
                    field,
                );
                if field.width > 0 && field.height > 0 {
                    frame.set_cursor_position((field.x, field.y));
                }
            } else {
                draw_input(frame, input, field);
            }
            let commands = crate::app::commands::available(app, &input.text);
            let selected = (*selected).min(commands.len().saturating_sub(1));
            let items = commands
                .iter()
                .map(|command| {
                    let label_width =
                        usize::from(choices.width).saturating_sub(command.shortcut.len() + 4);
                    let label = truncate(command.label, label_width);
                    let padding = label_width.saturating_sub(Line::from(label.as_str()).width());
                    ListItem::new(Line::from(vec![
                        Span::raw(label),
                        Span::raw(" ".repeat(padding + 2)),
                        Span::styled(
                            command.shortcut.clone(),
                            Style::default().fg(if command.disabled.is_some() {
                                MUTED
                            } else {
                                ACCENT
                            }),
                        ),
                    ]))
                    .style(Style::default().fg(
                        if command.disabled.is_some() {
                            MUTED
                        } else {
                            TEXT
                        },
                    ))
                })
                .collect::<Vec<_>>();
            if items.is_empty() {
                frame.render_widget(
                    Paragraph::new("No matching commands.").style(Style::default().fg(MUTED)),
                    choices,
                );
            } else {
                let mut state = ListState::default().with_selected(Some(selected));
                frame.render_stateful_widget(
                    List::new(items)
                        .highlight_style(Style::default().bg(SELECTED))
                        .highlight_symbol("› "),
                    choices,
                    &mut state,
                );
            }
            frame.render_widget(
                Paragraph::new(format!(
                    "{} command{} · ↑/↓ select · Enter run",
                    commands.len(),
                    if commands.len() == 1 { "" } else { "s" }
                ))
                .style(Style::default().fg(MUTED)),
                count,
            );
            let reason = commands.get(selected).and_then(|command| command.disabled);
            frame.render_widget(
                Paragraph::new(reason.unwrap_or("Esc closes · Ctrl-u clears the search"))
                    .style(Style::default().fg(if reason.is_some() { ACCENT } else { MUTED })),
                hint,
            );
        }
        Dialog::Ask => {}
        Dialog::AgentPicker { selected } => {
            let inner = popup(
                frame,
                " Explanation agent / Enter saves / Esc cancels ".into(),
                80,
                17,
            );
            let roomy = inner.height >= 12;
            let [intro, choices, details, footer] = Layout::vertical([
                Constraint::Length(if roomy { 2 } else { 1 }),
                Constraint::Length(if roomy { 6 } else { 3 }),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            frame.render_widget(
                Paragraph::new("Choose the agent used to explain PRs.")
                    .style(Style::default().fg(TEXT)),
                intro,
            );
            let items = [
                (Provider::Codex, "Codex", "Uses your installed Codex CLI"),
                (Provider::Claude, "Claude", "Uses your installed Claude CLI"),
                (
                    Provider::None,
                    "No AI guide",
                    "Keep PR details and code review available",
                ),
            ]
            .into_iter()
            .map(|(provider, name, description)| {
                let mut lines = vec![Line::from(Span::styled(
                    format!(
                        "{name}{}",
                        if app.settings.ai.provider == provider {
                            " · current"
                        } else {
                            ""
                        }
                    ),
                    Style::default().fg(ACCENT),
                ))];
                if roomy {
                    lines.push(Line::from(Span::styled(
                        description,
                        Style::default().fg(MUTED),
                    )));
                }
                ListItem::new(lines)
            })
            .collect::<Vec<_>>();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_style(Style::default().bg(SELECTED))
                    .highlight_symbol("› "),
                choices,
                &mut state,
            );
            let mut text = vec![Line::from(format!(
                "Model: {} · Language: {}",
                app.settings.ai.model.as_deref().unwrap_or("agent default"),
                app.settings.ai.language
            ))];
            if roomy {
                text.push(Line::from(format!(
                    "Automatic explanations: {}",
                    if app.settings.ai.auto_explain {
                        "on"
                    } else {
                        "off"
                    }
                )));
                if let Some(path) = &app.config_path {
                    text.push(Line::from(format!("Config: {}", path.display())));
                }
            }
            frame.render_widget(
                Paragraph::new(text).style(Style::default().fg(MUTED)),
                details,
            );
            frame.render_widget(
                Paragraph::new("Enter saves · Esc cancels").style(Style::default().fg(ACCENT)),
                footer,
            );
        }
        Dialog::OpenPr { input } => {
            let inner = popup(
                frame,
                " Open pull request / Enter opens / Esc cancels ".into(),
                64,
                7,
            );
            let [label, editor] =
                Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
            frame.render_widget(
                Paragraph::new(format!(
                    "Repository: {}\nPR number:",
                    app.repo.as_deref().unwrap_or("No repository selected")
                ))
                .style(Style::default().fg(MUTED)),
                label,
            );
            draw_input(frame, input, editor);
        }
        Dialog::Status { scroll } => {
            let inner = popup(
                frame,
                " Operation details / j,k scroll / Esc closes ".into(),
                110,
                frame.area().height.saturating_sub(4),
            );
            frame.render_widget(
                Paragraph::new(app.status.as_str())
                    .wrap(Wrap { trim: false })
                    .scroll((*scroll, 0)),
                inner,
            );
        }
        Dialog::Activity { text, scroll } => {
            let inner = popup(
                frame,
                " PR conversation and reviews / j,k scroll / Esc closes ".into(),
                110,
                frame.area().height.saturating_sub(4),
            );
            frame.render_widget(
                Paragraph::new(text.as_str())
                    .wrap(Wrap { trim: false })
                    .scroll((*scroll, 0)),
                inner,
            );
        }
        Dialog::Recovery { attempt } => {
            let inner = popup(
                frame,
                " Resolve previous submission / Esc cancels ".into(),
                100,
                20,
            );
            let body = format!(
                "The previous submission may already have reached GitHub.\n\nAction: {}\nCommit: {}\nInline comments: {}\nSummary: {}\n\nBefore choosing, inspect published reviews with V and comments with T\n(close this dialog with Esc to do so).\n\nIf you verified it was published: y clears matching local drafts.\nIf you verified it was NOT published: n enables a new submission.\n\nNeither choice posts anything to GitHub.",
                attempt.event,
                attempt.head_sha,
                attempt.comments.len(),
                attempt.body
            );
            frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), inner);
        }
        Dialog::Help { scroll } => {
            let inner = popup(
                frame,
                " Keyboard guide / j,k scroll / Esc closes ".into(),
                84,
                32,
            );
            let mut text = String::from("NORMAL-VIEW SHORTCUTS\n\n");
            for (scope, title) in keybindings::HELP_GROUPS {
                text.push_str(&format!("{title}\n"));
                for binding in keybindings::SPECS
                    .iter()
                    .filter(|binding| binding.scope == *scope)
                {
                    text.push_str(&format!(
                        "  {:<22} {}\n",
                        app.shortcut(binding.id),
                        binding.label
                    ));
                }
                text.push('\n');
            }
            text.push_str("DIALOG CONTROLS (fixed)\n  Esc              Close / cancel\n  Ctrl-s           Save, send question, or preview review\n  Ctrl-h           Backspace in text editors\n  Up/Down          Select command or dialog item\n  Ctrl-n/Ctrl-p    Select a command in the palette\n  Ctrl-u           Clear palette search\n  Enter            Run selected command or confirm preview\n  j/k, Ctrl-d/u    Scroll read-only dialogs\n\n  Ctrl-c always exits immediately.\n\nConfigure shortcuts in the [keybindings] section of your config.\nUse --print-keybindings to inspect effective action names and keys.\n\nGitHub review submission still requires its final confirmation.\nLocal notes and agent replies stay on this computer.");
            frame.render_widget(
                Paragraph::new(text)
                    .style(Style::default().fg(TEXT))
                    .wrap(Wrap { trim: false })
                    .scroll((*scroll, 0)),
                inner,
            );
        }
        Dialog::Description { scroll } => {
            draw_description(frame, app, *scroll);
        }
        Dialog::AiReview { scroll } => {
            draw_ai_review(frame, app, *scroll);
        }
        Dialog::Threads { scroll } => {
            let review = app.review.as_ref().unwrap();
            let inner = popup(
                frame,
                " Existing review comments / j,k scroll / Esc closes ".into(),
                110,
                frame.area().height.saturating_sub(4),
            );
            let mut lines = Vec::new();
            for thread in threads(&review.comments()) {
                let location = thread.anchor().map_or_else(
                    || {
                        format!(
                            "outdated; originally {}:{}",
                            thread.root.side,
                            thread
                                .root
                                .original_line
                                .map_or_else(|| "?".into(), |number| number.to_string())
                        )
                    },
                    |(side, _)| format!("{}:{}", side_label(&side), thread.root.line_label()),
                );
                lines.push(Line::from(Span::styled(
                    format!("{}  {location}", thread.root.path),
                    Style::default().fg(ACCENT),
                )));
                lines.push(Line::from(Span::styled(
                    format!("by {}", thread.root.author),
                    Style::default().fg(MUTED),
                )));
                lines.extend(
                    thread
                        .root
                        .body
                        .lines()
                        .map(|line| Line::from(line.to_owned())),
                );
                for reply in thread.replies {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        format!("  ↳ {}", reply.author),
                        Style::default().fg(COMMENT_ACCENT),
                    )));
                    lines.extend(reply.body.lines().map(|line| Line::from(line.to_owned())));
                }
                lines.push(Line::from(""));
            }
            if lines.is_empty() {
                lines.push(Line::from("No inline review comments yet."));
            }
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .scroll((*scroll, 0)),
                inner,
            );
        }
        Dialog::Drafts { selected } => {
            let review = app.review.as_ref().unwrap();
            let inner = popup(
                frame,
                " Saved drafts / c edit / d delete / Esc closes ".into(),
                100,
                frame.area().height.saturating_sub(4),
            );
            let items: Vec<ListItem> = review
                .drafts
                .iter()
                .map(|d| {
                    ListItem::new(vec![
                        Line::from(Span::styled(
                            format!("{}:{} ({:?})", d.path, d.line_label(), d.side),
                            Style::default().fg(ACCENT),
                        )),
                        Line::from(d.body.replace('\n', " / ")),
                        Line::from(""),
                    ])
                })
                .collect();
            if items.is_empty() {
                frame.render_widget(
                    Paragraph::new("No local drafts. Select a diff line and press c."),
                    inner,
                );
            } else {
                let mut state = ListState::default().with_selected(Some(*selected));
                frame.render_stateful_widget(
                    List::new(items)
                        .highlight_style(Style::default().bg(PANEL))
                        .highlight_symbol("> "),
                    inner,
                    &mut state,
                );
            }
        }
        Dialog::Note {
            anchor,
            reply_to,
            input,
        } => {
            let inner = popup(
                frame,
                format!(
                    " Local {} {}:{} / Ctrl-s save / Esc cancel ",
                    if reply_to.is_some() { "reply" } else { "note" },
                    anchor.path,
                    anchor.line_label()
                ),
                100,
                17,
            );
            draw_input(frame, input, inner);
        }
        Dialog::Notes { selected } => {
            let inner = popup(
                frame,
                " Local notes / c reply / o jump / d delete thread / Esc closes ".into(),
                110,
                frame.area().height.saturating_sub(4),
            );
            let mut lines = Vec::new();
            if let Some(review) = &app.review {
                for (index, note) in review.notes.iter().enumerate() {
                    lines.push(Line::styled(
                        format!(
                            "{} {} · {}:{} {:?} · {}{}",
                            if index == *selected { ">" } else { " " },
                            note.id,
                            note.anchor.path,
                            note.anchor.line_label(),
                            note.anchor.side,
                            note.author,
                            if note.outdated { " · outdated" } else { "" }
                        ),
                        Style::default()
                            .fg(if index == *selected { ACCENT } else { TEXT })
                            .bold(),
                    ));
                    if let Some(parent) = &note.reply_to {
                        lines.push(Line::from(format!("  reply to {parent}")));
                    }
                    lines.extend(markdown_text(&note.anchor.body, inner.width).lines);
                    lines.push(Line::default());
                }
                if review.notes.is_empty() {
                    lines.push(Line::from(
                        "Select lines and press c (local diff) or C (PR) to leave a local note.",
                    ));
                }
            }
            let text = Text::from(lines);
            let selected_line = app.review.as_ref().map_or(0, |review| {
                review
                    .notes
                    .iter()
                    .take(*selected)
                    .map(|note| {
                        2 + markdown_text(&note.anchor.body, inner.width).lines.len()
                            + usize::from(note.reply_to.is_some())
                    })
                    .sum::<usize>()
            });
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .scroll((selected_line.min(u16::MAX as usize) as u16, 0)),
                inner,
            );
        }
        Dialog::Compose { draft, input, .. } => {
            let inner = popup(
                frame,
                format!(
                    " Draft {}:{} {:?} / Ctrl-s save / Esc cancel ",
                    draft.path,
                    draft.line_label(),
                    draft.side
                ),
                100,
                17,
            );
            draw_input(frame, input, inner);
        }
        Dialog::Submit { event, input } => {
            let inner = popup(
                frame,
                " Review summary / Tab changes action / Ctrl-s previews / Esc cancel ".into(),
                100,
                18,
            );
            let [kind, editor] =
                Layout::vertical([Constraint::Length(3), Constraint::Min(2)]).areas(inner);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        format!("Action: {}", event.as_str()),
                        Style::default().fg(ACCENT).bold(),
                    )),
                    Line::from(
                        "Write a summary below. Comments stay local until final confirmation.",
                    ),
                ]),
                kind,
            );
            draw_input(frame, input, editor);
        }
        Dialog::Confirm {
            event,
            body,
            scroll,
        } => {
            let review = app.review.as_ref().unwrap();
            let inner = popup(
                frame,
                " Final preview / Enter submits to GitHub / Esc cancels / j,k scroll ".into(),
                110,
                frame.area().height.saturating_sub(4),
            );
            let mut lines = vec![
                Line::from(Span::styled(
                    format!(
                        "{} on {} #{}",
                        event.as_str(),
                        review.data.pr.id.repo,
                        review.data.pr.id.number
                    ),
                    Style::default().fg(ACCENT).bold(),
                )),
                Line::from(format!("Commit: {}", review.data.pr.head_sha)),
                Line::from(""),
                Line::from(body.clone()),
                Line::from(""),
                Line::from(format!("{} inline comment(s)", review.drafts.len())),
                Line::from(""),
            ];
            for draft in &review.drafts {
                lines.push(Line::from(Span::styled(
                    format!("{}:{} {:?}", draft.path, draft.line_label(), draft.side),
                    Style::default().fg(ACCENT),
                )));
                lines.extend(draft.body.lines().map(|s| Line::from(s.to_owned())));
                lines.push(Line::from(""));
            }
            if app.demo {
                lines.push(Line::from(Span::styled(
                    "OFFLINE DEMO: nothing will be posted.",
                    Style::default().fg(Color::Yellow),
                )));
            }
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .scroll((*scroll, 0)),
                inner,
            );
        }
        Dialog::Quit => {
            let inner = popup(frame, " Leave review? ".into(), 66, 7);
            frame.render_widget(Paragraph::new("Draft comments are saved locally for this revision.\n\nEnter or q exits. Esc resumes reviewing.").wrap(Wrap { trim: false }), inner);
        }
    }
}

fn draw_input(frame: &mut Frame, input: &TextInput, area: Rect) {
    let before = &input.text[..input.cursor];
    let row = before.bytes().filter(|&c| c == b'\n').count();
    let last_line = before.rsplit('\n').next().unwrap_or_default();
    let col = Line::from(last_line).width();
    let vertical = row.saturating_sub(usize::from(area.height.saturating_sub(1)));
    let horizontal = col.saturating_sub(usize::from(area.width.saturating_sub(1)));
    frame.render_widget(
        Paragraph::new(Text::raw(input.text.as_str())).scroll((
            vertical.min(u16::MAX as usize) as u16,
            horizontal.min(u16::MAX as usize) as u16,
        )),
        area,
    );
    if area.width > 0 && area.height > 0 {
        frame.set_cursor_position((
            area.x + (col - horizontal) as u16,
            area.y + (row - vertical) as u16,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Selection, demo_data};
    use prpr::model::{DraftComment, ReviewComment};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn split_pairs_deletion_runs_with_following_additions() {
        let lines = prpr::diff::parse_patch(
            "@@ -1,5 +1,5 @@\n keep\n-old one\n-old two\n-old three\n+new one\n+new two\n tail\n+added",
        );
        assert_eq!(
            prpr::diff::split_pairs(&lines),
            [
                (Some(0), None),
                (Some(1), Some(1)),
                (Some(2), Some(5)),
                (Some(3), Some(6)),
                (Some(4), None),
                (Some(7), Some(7)),
                (None, Some(8)),
            ]
        );
    }

    #[test]
    fn split_rows_show_both_sides_and_track_the_cursor_on_either_half() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        let mut data = demo_data();
        data.files.truncate(1);
        data.files[0].patch = Some("@@ -1,2 +1,2 @@\n keep\n-before\n+after".into());
        app.loaded(data).unwrap();
        let review = app.review.as_mut().unwrap();
        review.show_comments = false;
        review.positions[0] = 3;
        let rows = review_rows(app.review.as_ref().unwrap(), 80, true);
        let paired = rows
            .iter()
            .find(|row| row_text(row).contains("before"))
            .unwrap();
        let text = row_text(paired);
        assert!(
            text.contains("after"),
            "deletion and addition share a row: {text}"
        );
        assert!(text.find("before").unwrap() < text.find("after").unwrap());
        assert_eq!((paired.source, paired.alt_source), (Some(2), Some(3)));
        assert_eq!(selected_row(&rows, 3), selected_row(&rows, 2));
        assert_eq!(paired.line.width(), 80);
    }

    #[test]
    fn marks_map_source_columns_through_tabs_and_horizontal_scroll() {
        assert_eq!(display_range("let value = 1;", (5, 9), 0), (4, 9));
        assert_eq!(display_range("\tvalue", (2, 6), 0), (4, 9));
        assert_eq!(display_range("let value = 1;", (5, 9), 2), (2, 7));
        assert_eq!(display_range("abc", (1, u32::MAX), 0), (0, 3));
        let spans = highlight_spans(vec![Span::raw("let "), Span::raw("value = 1;")], (4, 9));
        let marked = spans
            .iter()
            .filter(|span| span.style.bg == Some(MARK))
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(marked, "value");
    }

    #[test]
    fn viewport_scrolls_only_near_edges_centers_jumps_and_keeps_paging_offset() {
        let scroll = |top, selected, keep| scroll_viewport(top, selected, 100, 20, 5, keep);
        assert_eq!(
            scroll(10, 20, None),
            10,
            "moving inside the view keeps it still"
        );
        assert_eq!(scroll(10, 25, None), 11, "bottom margin scrolls by one row");
        assert_eq!(scroll(10, 14, None), 9, "top margin scrolls by one row");
        assert_eq!(scroll(10, 60, None), 50, "a jump out of view is centered");
        assert_eq!(
            scroll(10, 33, Some(8)),
            25,
            "paging keeps the cursor's screen row"
        );
        assert_eq!(scroll(10, 99, None), 80, "the view never passes the end");
        assert_eq!(
            scroll_viewport(3, 2, 10, 20, 5, None),
            0,
            "short files never scroll"
        );
        assert_eq!(
            scroll_viewport(0, 3, 100, 4, 5, None),
            1,
            "scrolloff shrinks in tiny viewports"
        );
    }

    #[test]
    fn directory_segments_join_single_child_chains_but_keep_branches_separate() {
        let files = [
            "services/api/src/main/kotlin/com/example/billing/Invoice.kt",
            "services/api/src/main/kotlin/com/example/billing/retry/Policy.kt",
            "services/api/src/main/kotlin/com/example/checkout/Charge.kt",
            "services/api/src/test/kotlin/com/example/billing/InvoiceTest.kt",
            "README.md",
        ];
        let labels = directory_segments(&files)
            .into_iter()
            .map(|segments| {
                segments
                    .into_iter()
                    .map(|segment| segment.label)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                vec!["services/api/src", "main/kotlin/com/example", "billing"],
                vec![
                    "services/api/src",
                    "main/kotlin/com/example",
                    "billing",
                    "retry"
                ],
                vec!["services/api/src", "main/kotlin/com/example", "checkout"],
                vec!["services/api/src", "test/kotlin/com/example/billing"],
                vec![],
            ]
        );
    }

    #[test]
    fn html_comments_are_hidden_except_inside_code_fences() {
        let body = "Summary\n<!-- template hint -->\nKeep <!-- inline --> this\n<!--\nmulti\nline\n-->\n```html\n<!-- literal -->\n```\nEnd\n";
        assert_eq!(
            strip_html_comments(body),
            "Summary\nKeep  this\n```html\n<!-- literal -->\n```\nEnd\n"
        );
    }

    #[test]
    fn truncate_path_keeps_the_first_and_last_directory_names() {
        assert_eq!(truncate_path("main/kotlin/com/acme", 12), "main/…/acme");
        assert_eq!(truncate_path("test/kotlin/com/acme", 12), "test/…/acme");
        assert_eq!(truncate_path("domain", 10), "domain");
        assert_eq!(truncate_path("averyveryverylongname/x", 8), "averyve…");
    }

    fn row_text(row: &ViewRow) -> String {
        row.line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn review_comments_and_dialogs_render_at_supported_sizes() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.loaded(demo_data()).unwrap();
        for (width, height) in [(120, 36), (80, 24), (48, 12)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for shown in [true, false] {
                app.review.as_mut().unwrap().show_comments = shown;
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let output = screen(&terminal);
                assert!(output.contains("2 comments"));
                assert!(output.contains(if shown { "shown" } else { "hidden" }));
                assert!(output.contains("client.rs"));
            }
            app.dialog = Some(Dialog::Confirm {
                event: prpr::model::ReviewEvent::Comment,
                body: "Looks good".into(),
                scroll: 0,
            });
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            app.dialog = Some(Dialog::OpenPr {
                input: TextInput::default(),
            });
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            app.dialog = None;
        }
    }

    #[test]
    fn inserted_threads_and_drafts_keep_source_coordinates_and_selected_line_visible() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.loaded(demo_data()).unwrap();
        let review = app.review.as_mut().unwrap();
        review.drafts.push(DraftComment {
            path: "src/client.rs".into(),
            side: Side::Left,
            line: 19,
            start_line: None,
            start_side: None,
            body: "Please preserve the existing deadline contract.".into(),
        });
        let selected = review
            .current_lines()
            .iter()
            .position(|line| matches_anchor(line, &Side::Right, 25))
            .unwrap();
        review.positions[0] = selected;
        for width in [32, 57, 89] {
            let rows = review_rows(review, width, false);
            assert_eq!(
                rows.iter().filter(|row| row.source.is_some()).count(),
                review.current_lines().len()
            );
            assert_eq!(
                rows.iter()
                    .filter(|row| row_text(row).contains("sam · 1 reply"))
                    .count(),
                1
            );
            assert_eq!(
                rows.iter()
                    .filter(|row| row_text(row).contains("alex · reply"))
                    .count(),
                1
            );
            assert_eq!(
                rows.iter()
                    .filter(|row| row_text(row).contains("draft · LEFT:19"))
                    .count(),
                1
            );
            for height in [5, 17, 29] {
                let start = viewport_start(&rows, selected, height);
                assert!(
                    rows.iter()
                        .skip(start)
                        .take(height)
                        .any(|row| row.source == Some(selected))
                );
            }
        }
        let expected = review.anchor();
        review.show_comments = false;
        let rows = review_rows(review, 57, false);
        assert!(!rows.iter().any(|row| row_text(row).contains("sam")));
        assert!(!rows.iter().any(|row| row_text(row).contains("draft ·")));
        assert_eq!(review.anchor(), expected);
        assert_eq!(
            rows.iter().filter(|row| row.source.is_some()).count(),
            review.current_lines().len()
        );
    }

    #[test]
    fn outdated_comments_are_labeled_and_never_reuse_original_coordinates() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.loaded(demo_data()).unwrap();
        let review = app.review.as_mut().unwrap();
        review.data.comments = vec![ReviewComment {
            id: 99,
            path: "src/client.rs".into(),
            body: "Historical discussion".into(),
            author: "sam".into(),
            line: None,
            original_line: Some(19),
            side: "LEFT".into(),
            start_line: None,
            start_side: None,
            in_reply_to_id: None,
        }];
        let rows = review_rows(review, 32, false);
        let last_source = rows.iter().rposition(|row| row.source.is_some()).unwrap();
        let outdated = rows
            .iter()
            .position(|row| row_text(row).contains("outdated ·"))
            .unwrap();
        assert!(outdated > last_source);
        assert!(rows[outdated].source.is_none());
        review.data.files[0].patch = None;
        review.lines[0].clear();
        let rows = review_rows(review, 32, false);
        assert!(
            rows.iter()
                .any(|row| row_text(row).contains("Historical discussion"))
        );
        assert!(rows.iter().all(|row| row.source.is_none()));
        let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let output = screen(&terminal);
        assert!(output.contains("outdated"));
        assert!(output.contains("Historical discussion"));
    }

    #[test]
    fn long_comment_cards_wrap_and_offer_full_text_access() {
        let mut rows = Vec::new();
        let text = "日本語の文章と a-long-url-without-any-spaces ".repeat(20);
        append_card(&mut rows, "sam · RIGHT:25", &text, 32, false, false);
        assert_eq!(rows.len(), 9);
        assert!(
            rows.iter()
                .any(|row| row_text(row).contains("open full thread"))
        );
        for line in wrap_comment(&text, 27) {
            assert!(Line::from(line).width() <= 27);
        }
        let mut drafts = Vec::new();
        append_card(&mut drafts, "draft", &text, 32, true, false);
        assert!(
            drafts
                .iter()
                .any(|row| row_text(row).contains("open saved drafts"))
        );
    }

    #[test]
    fn addition_and_deletion_backgrounds_fill_the_entire_diff_width() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.loaded(demo_data()).unwrap();
        app.review.as_mut().unwrap().show_comments = false;
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut additions = 0;
        let mut deletions = 0;
        for y in 5..34 {
            let background = buffer[(119, y)].bg;
            additions += usize::from(background == ADDITION);
            deletions += usize::from(background == DELETION);
        }
        assert!(additions >= 5);
        assert!(deletions >= 2);
        assert_eq!(buffer[(0, 35)].bg, PANEL);
    }

    #[test]
    fn selection_spans_gutter_code_and_trailing_cells_for_each_diff_kind() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.loaded(demo_data()).unwrap();
        app.review.as_mut().unwrap().show_comments = false;
        for (position, background) in [
            (0, SELECTED),
            (1, SELECTED),
            (2, SELECTED_DELETION),
            (4, SELECTED_ADDITION),
        ] {
            app.review.as_mut().unwrap().positions[0] = position;
            let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            let y = (5..34)
                .find(|&y| buffer[(119, y)].bg == background)
                .expect("Selected row must extend through the trailing cells");
            for x in 32..120 {
                assert_eq!(
                    buffer[(x, y)].bg,
                    background,
                    "Selected row has a background gap at column {x}"
                );
            }
        }
        assert_ne!(SELECTED_ADDITION, SELECTED_DELETION);
    }

    #[test]
    fn selected_ranges_tint_all_source_rows_and_show_the_mode_at_supported_sizes() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.loaded(demo_data()).unwrap();
        let review = app.review.as_mut().unwrap();
        review.show_comments = false;
        review.positions[0] = 5;
        review.selection = Some(Selection {
            anchor: 2,
            extending: true,
        });
        let rows = review_rows(review, 89, false);
        for (index, line) in review.current_lines().iter().enumerate() {
            let row = rows.iter().find(|row| row.source == Some(index)).unwrap();
            if (2..=5).contains(&index) {
                let expected = match line.kind {
                    LineKind::Addition => SELECTED_ADDITION,
                    LineKind::Deletion => SELECTED_DELETION,
                    _ => SELECTED,
                };
                assert_eq!(row.background, expected);
            } else {
                assert!(
                    ![SELECTED, SELECTED_ADDITION, SELECTED_DELETION].contains(&row.background)
                );
            }
        }
        assert!(row_text(rows.iter().find(|row| row.source == Some(5)).unwrap()).starts_with('›'));
        for (width, height) in [(48, 12), (80, 24), (120, 36)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(screen(&terminal).contains("SELECT 4 lines"));
        }
        app.review
            .as_mut()
            .unwrap()
            .selection
            .as_mut()
            .unwrap()
            .extending = false;
        let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("4 lines selected"));
    }

    #[test]
    fn goto_prefix_help_is_visible_in_both_inbox_and_review() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('g'),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("g … · Esc cancels"));
        app.loaded(demo_data()).unwrap();
        app.key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('g'),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("g … · Esc cancels"));
    }

    #[test]
    fn multiline_locations_remain_visible_in_inline_cards_and_all_review_dialogs() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.loaded(demo_data()).unwrap();
        let draft = DraftComment {
            path: "src/client.rs".into(),
            line: 20,
            side: Side::Left,
            start_line: Some(19),
            start_side: Some(Side::Left),
            body: "Review this whole range.".into(),
        };
        let review = app.review.as_mut().unwrap();
        review.drafts = vec![draft.clone()];
        review.data.comments = vec![ReviewComment {
            id: 99,
            path: "src/client.rs".into(),
            line: Some(20),
            original_line: Some(20),
            side: "LEFT".into(),
            start_line: Some(19),
            start_side: Some("LEFT".into()),
            body: "Published range comment.".into(),
            author: "sam".into(),
            in_reply_to_id: None,
        }];
        let rows = review_rows(review, 32, false);
        assert_eq!(
            rows.iter()
                .filter(|row| row_text(row).contains("LEFT:19–20"))
                .count(),
            2
        );
        let endpoint = rows.iter().position(|row| row.source == Some(3)).unwrap();
        assert!(row_text(&rows[endpoint + 1]).contains("LEFT:19–20"));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        for dialog in [
            Dialog::Compose {
                draft,
                editing: Some(0),
                input: TextInput::default(),
            },
            Dialog::Drafts { selected: 0 },
            Dialog::Confirm {
                event: prpr::model::ReviewEvent::Comment,
                body: "Summary".into(),
                scroll: 0,
            },
            Dialog::Threads { scroll: 0 },
        ] {
            app.dialog = Some(dialog);
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(screen(&terminal).contains("19–20"));
        }
    }

    fn overview_metadata() -> prpr::model::PrOverview {
        let data = demo_data();
        prpr::model::PrOverview {
            id: data.pr.id,
            title: data.pr.title,
            body: "Bound retry waits by the remaining deadline.".into(),
            url: data.pr.url,
            author: data.pr.author,
            head_sha: data.pr.head_sha,
            base_tip_sha: data.pr.base_sha,
            draft: data.pr.draft,
            state: "OPEN".into(),
            changed_files: 2,
            additions: 11,
            deletions: 2,
        }
    }

    fn analysis_fixture() -> Analysis {
        serde_json::from_value(serde_json::json!({
            "summary": "Retry waits now respect the request deadline.",
            "intent": {"text": "Keep retries inside the caller's deadline.", "basis": "author_description"},
            "changes": [{"title": "Bound the retry delay", "detail": "The remaining budget caps the delay before retrying.", "files": ["src/client.rs"]}],
            "reading_order": [
                {"path": "src/client.rs", "line": 25, "side": "RIGHT", "reason": "Start with the request execution path."},
                {"path": "tests/deadline.rs", "line": 3, "side": "RIGHT", "reason": "Then inspect zero-budget coverage."}
            ],
            "risks": [{"title": "Time spent sending", "detail": "Check whether send also respects the remaining deadline.", "files": ["src/client.rs"]}],
            "verification": {"existing_tests": ["tests/deadline.rs"], "suggested_checks": ["Run the deadline test with a zero budget."], "unknowns": ["Deployment latency is not shown."]},
            "coverage": {"included_files": ["src/client.rs", "tests/deadline.rs"], "omitted_files": ["large-fixture.json"], "unavailable_files": ["diagram.png"]}
        })).unwrap()
    }

    #[test]
    fn overview_metadata_renders_before_any_diff_for_every_ai_state() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.opened(overview_metadata());
        for (state, message) in [
            (AiStatus::Disabled, "AI guide not configured"),
            (AiStatus::Waiting, "Waiting for the diff"),
            (AiStatus::Loading, "Preparing AI guide"),
            (
                AiStatus::Error("Missing codex executable".into()),
                "AI guide unavailable",
            ),
        ] {
            app.overview.as_mut().unwrap().ai = state;
            for (width, height) in [(80, 24), (120, 36)] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                app.tab = PageTab::Overview;
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let output = screen(&terminal);
                assert!(output.contains("Keep request retries within the deadline"));
                assert!(output.contains("Bound retry waits by the remaining deadline."));
                assert!(output.contains("https://github.com/example/service/pull/42"));
                assert!(output.contains("1 Overview"));
                assert!(output.contains("2 AI Guide"));
                assert!(output.contains("3 Diff"));
                assert!(!output.contains(message));
                app.tab = PageTab::Guide;
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                assert!(screen(&terminal).contains(message));
                assert!(app.review.is_none());
            }
        }
        app.overview.as_mut().unwrap().ai = AiStatus::Waiting;
        app.diff_loading = false;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("a explains this PR"));
    }

    #[test]
    fn overview_shows_labels_ci_failures_and_conversation_in_time_order() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.opened(overview_metadata());
        app.overview.as_mut().unwrap().activity =
            ActivityStatus::Ready(crate::app::demo_activity());
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let output = screen(&terminal);
        assert!(output.contains("enhancement"));
        assert!(output.contains("risk:low"));
        assert!(output.contains("1 failed · 1 passed"));
        assert!(output.contains("Checks needing attention"));
        assert!(output.contains("CI / test"));
        assert!(!output.contains("CI / lint"));
        assert!(output.contains("Conversation · 2"));
        let comment = output
            .find("@reviewer · commented · 2026-01-02 09:30 UTC")
            .unwrap();
        let approval = output.find("@maintainer · approved").unwrap();
        assert!(comment < approval);
    }

    #[test]
    fn wide_guide_centers_the_narrative_and_reading_order_together() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.opened(overview_metadata());
        app.tab = PageTab::Guide;
        app.overview.as_mut().unwrap().ai = AiStatus::Ready(Box::new(analysis_fixture()));
        for width in [110_u16, 200] {
            let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let output = screen(&terminal);
            let byte = output
                .find(" Reading order")
                .expect("reading order panel is visible");
            let column = usize::from(output[..byte].chars().count() as u16 % width);
            let narrative = usize::from(width - 38).min(98);
            let margin = (usize::from(width) - narrative - 38) / 2;
            assert_eq!(column, margin + narrative + 1, "width {width}");
        }
    }

    #[test]
    fn structured_overview_includes_all_sections_and_keeps_reading_steps_visible() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.settings.ai.provider = Provider::Codex;
        app.opened(overview_metadata());
        app.tab = PageTab::Guide;
        app.overview.as_mut().unwrap().ai = AiStatus::Ready(Box::new(analysis_fixture()));
        let rows = overview_rows(&analysis_fixture(), 80, true);
        let text = rows
            .iter()
            .flat_map(|row| row.line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<String>();
        for section in [
            "Summary",
            "Intent",
            "Main changes",
            "Reading order",
            "Review risks",
            "Verification",
            "Unknowns",
            "Context coverage",
        ] {
            assert!(text.contains(section), "Missing overview section {section}");
        }
        for (width, height) in [(80, 24), (120, 36)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(screen(&terminal).contains("Retry waits now respect"));
            app.overview.as_mut().unwrap().selected_step = 1;
            app.overview.as_mut().unwrap().focus_guide = true;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(screen(&terminal).contains("tests/deadline.rs:3"));
            app.overview.as_mut().unwrap().focus_guide = false;
            app.overview.as_mut().unwrap().guide_scroll = u16::MAX;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let output = screen(&terminal);
            assert!(output.contains("Context coverage"));
            assert!(output.contains("diagram.png"));
            let end = app.overview.as_ref().unwrap().guide_scroll;
            assert!(end > 0 && end < u16::MAX);
            app.key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('k'),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert_eq!(app.overview.as_ref().unwrap().guide_scroll, end - 1);
            assert_eq!(app.overview.as_ref().unwrap().scroll, 0);
            app.overview.as_mut().unwrap().guide_scroll = 0;
        }
    }

    #[test]
    fn overview_description_and_agent_picker_work_without_review_data() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.opened(overview_metadata());
        app.config_path = Some(temp.path().join("config.toml"));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        app.dialog = Some(Dialog::Description { scroll: 0 });
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("Bound retry waits by the remaining deadline."));
        app.dialog = Some(Dialog::AgentPicker { selected: 1 });
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let output = screen(&terminal);
        for text in [
            "Codex",
            "Claude",
            "No AI guide",
            "Enter saves",
            "agent default",
        ] {
            assert!(output.contains(text));
        }
        assert!(app.review.is_none());
    }

    #[test]
    fn diff_loading_and_error_views_do_not_require_review_data() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.opened(overview_metadata());
        app.tab = PageTab::Diff;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("Loading changed files"));
        app.diff_loading = false;
        app.diff_error = Some("Network connection is unavailable".into());
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let output = screen(&terminal);
        assert!(output.contains("Network connection is unavailable"));
        assert!(output.contains("r retries"));
        assert!(output.contains("Keep request retries within the deadline"));
    }
    #[test]
    fn markdown_has_readable_headings_inline_code_and_indented_code_blocks() {
        use ratatui::style::Modifier;
        let text = markdown_text(
            "# Read this\n\nUse **bounded retries** with `budget`.\n\n```rust\n    sleep(delay);\n```\n\n[Guide](https://example.com)",
            60,
        );
        let plain = text.to_string();
        assert!(plain.contains("Read this") && !plain.contains("# Read"));
        assert!(!plain.contains("**") && !plain.contains("```"));
        assert!(text.lines.iter().any(|line| line.style.fg == Some(ACCENT)
            && line.style.add_modifier.contains(Modifier::BOLD)));
        assert!(
            text.lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content.contains("budget")
                    && span.style.bg == Some(CODE_BACKGROUND))
        );
        let block = text
            .lines
            .iter()
            .find(|line| line.to_string().contains("sleep(delay)"))
            .unwrap();
        assert!(block.to_string().starts_with("│     sleep"));
        assert_eq!(block.width(), 60);
        assert!(
            markdown_text("```rust\n\tlet delay = budget;\n```", 60)
                .to_string()
                .contains("│     let delay")
        );
        assert!(text.lines.iter().flat_map(|line| &line.spans).any(|span| span.content.contains("Guide") && span.style.fg == Some(COMMENT_ACCENT)));
        assert!(markdown_text("\\-\\-\\-", 24).to_string().contains("---"));
    }

    #[test]
    fn markdown_wraps_unicode_lists_and_tables_without_losing_styles_or_content() {
        let text = "説明を読みながら変更箇所を確認するe\u{301}検証";
        let rows = wrap_styled(&Line::from(text), 8, 0, false);
        assert_eq!(
            rows.iter().map(ToString::to_string).collect::<String>(),
            text
        );
        assert!(rows.iter().all(|line| line.width() <= 8));
        let list = markdown_text(
            "- **A lengthy item** continues over several lines with `code` in it.",
            24,
        );
        assert!(list.lines.len() > 2);
        assert!(
            list.lines
                .iter()
                .skip(1)
                .filter(|line| !line.to_string().is_empty())
                .all(|line| line.to_string().starts_with("  "))
        );
        let table = markdown_text(
            "| Behavior | Why |\n| --- | --- |\n| 長い説明の文章がここに入ります | Preserve the deadline |",
            32,
        );
        assert!(table.lines.iter().all(|line| line.width() <= 32));
        assert!(table.to_string().contains("Behavior"));
        assert!(
            table
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.fg == Some(ACCENT))
        );
    }

    #[test]
    fn ai_overview_renders_markdown_and_keeps_file_names_literal() {
        let mut analysis = analysis_fixture();
        analysis.summary = "Review **bounded waits** using `budget`.".into();
        analysis.changes[0].detail = "- Keep the **deadline**.\n- Return an error.\n\n```rust\nreturn Err(Error::DeadlineExceeded);\n```".into();
        analysis.changes[0].files = vec!["src/*literal*.rs".into()];
        let rows = overview_rows(&analysis, 80, true);
        let plain = rows
            .iter()
            .map(|row| row.line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plain.contains("bounded waits") && !plain.contains("**bounded"));
        assert!(!plain.contains("```"));
        assert!(plain.contains("src/*literal*.rs"));
        assert!(
            rows.iter()
                .flat_map(|row| &row.line.spans)
                .any(|span| span.content.contains("budget")
                    && span.style.bg == Some(CODE_BACKGROUND))
        );
        assert!(
            rows.iter()
                .any(|row| row.line.to_string().contains("│ return Err"))
        );
    }

    #[test]
    fn full_markdown_description_clamps_scrolling_and_shows_a_scrollbar() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(directory.path().into(), true);
        let mut metadata = overview_metadata();
        metadata.body =
            "# Description\n\n".to_owned() + &"- A long description item.\n".repeat(150);
        app.opened(metadata);
        app.dialog = Some(Dialog::Description { scroll: u16::MAX });
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let end = match app.dialog {
            Some(Dialog::Description { scroll }) => scroll,
            _ => panic!("Description closed"),
        };
        assert!(end > 0 && end < u16::MAX);
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "┃")
        );
        app.key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('k'),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(matches!(app.dialog, Some(Dialog::Description { scroll }) if scroll == end - 1));
    }
}
