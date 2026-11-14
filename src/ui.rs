use std::collections::{BTreeMap, HashSet};

use pr_review::{
    analysis::Analysis,
    diff::{DiffLine, LineKind},
    model::{ChangedFile, Side},
    settings::Provider,
    thread::{Thread, matches_anchor, threads},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::{AiStatus, App, Dialog, Focus, OverviewState, PageTab, ReviewState, TextInput};

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
        " g: g top · e end · f open · Esc cancel "
    } else if app.overview.is_some() && matches!(app.tab, PageTab::Overview) {
        if area.width < 76 {
            " j/k scroll  n/p guide  a explain  A agent  ? keys "
        } else {
            " Tab views  j/k scroll  n/p guide  Enter diff  a explain  A agent  b description  ? keys "
        }
    } else if app.overview.is_some() && app.review.is_none() {
        " 1 Overview  2 Diff  Tab views  r retry  ? keys "
    } else if app.review.is_some() && area.width < 76 {
        " v select  x/X lines  c note  o open  ? keys "
    } else if app.review.is_some() && area.width < 105 {
        " v select  x/X lines  ; collapse  g goto  c note  o open  V reviews  ? keys "
    } else if app.review.is_some() {
        " v select  x/X lines  ; collapse  gg/ge top/end  gf/o open  c note  t toggle  T threads  V reviews  s submit  ? keys "
    } else if area.width < 76 {
        " o open  # open PR  / filter  ? keys "
    } else {
        " j/k select  o open  # open PR  / filter  r refresh  ? keys  q quit "
    };
    frame.render_widget(
        Paragraph::new(keys).style(Style::default().fg(MUTED).bg(PANEL)),
        help,
    );
    if let Some(dialog) = &app.dialog {
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
    let mode = if app.demo {
        "gh-preview · demo"
    } else {
        "gh-preview"
    };
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
                " {} · o opens a review; # opens a PR by number",
                app.repo.as_deref().unwrap_or("Your review queue")
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
    let label = if app.demo {
        "gh-preview · demo"
    } else {
        "gh-preview"
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {label}"), Style::default().fg(ACCENT).bold()),
            Span::styled(
                format!("   {} #{}", metadata.id.repo, metadata.id.number),
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
    let diff = if app.diff_loading {
        " 2 Diff · loading "
    } else if app.diff_error.is_some() {
        " 2 Diff · error "
    } else if app.comments_loading {
        " 2 Diff · comments "
    } else {
        " 2 Diff "
    };
    let selected = |active| {
        Style::default()
            .fg(if active { ACCENT } else { MUTED })
            .bg(if active { PANEL } else { SIDEBAR })
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " 1 Overview ",
                selected(matches!(app.tab, PageTab::Overview)).bold(),
            ),
            Span::raw(" "),
            Span::styled(diff, selected(matches!(app.tab, PageTab::Diff)).bold()),
            Span::styled(
                if area.width >= 85 {
                    "    Tab / Shift-Tab switches views"
                } else {
                    ""
                },
                Style::default().fg(MUTED),
            ),
        ]))
        .style(Style::default().bg(SIDEBAR)),
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
    let Some(overview) = &mut app.overview else {
        return;
    };
    let metadata = &overview.metadata;
    let body = if metadata.body.trim().is_empty() {
        "No author description."
    } else {
        metadata.body.as_str()
    };
    let wrapped = wrap_comment(body, usize::from(area.width).saturating_sub(4).max(1));
    let preview = if area.height >= 22 { 3 } else { 2 };
    let description_height = wrapped.len().min(preview) as u16 + 1;
    let [details, description, state, content] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(description_height),
        Constraint::Length(2),
        Constraint::Min(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    format!(
                        " {} · {}{}",
                        metadata.author,
                        metadata.state.to_lowercase(),
                        if metadata.draft { " · draft" } else { "" }
                    ),
                    Style::default().fg(TEXT),
                ),
                Span::styled(
                    format!(" · {} changed files", metadata.changed_files),
                    Style::default().fg(MUTED),
                ),
            ]),
            Line::from(Span::styled(
                format!(" {}", metadata.url),
                Style::default().fg(MUTED),
            )),
        ]),
        details,
    );
    let mut description_lines = vec![Line::from(Span::styled(
        " AUTHOR DESCRIPTION · b opens full text",
        Style::default().fg(MUTED),
    ))];
    description_lines.extend(
        wrapped
            .into_iter()
            .take(preview)
            .map(|line| Line::from(format!("  {line}"))),
    );
    frame.render_widget(Paragraph::new(description_lines), description);
    let (caption, color) = match &overview.ai {
        AiStatus::Disabled => (
            " AI guide not configured · a chooses an agent".to_owned(),
            MUTED,
        ),
        AiStatus::Waiting => (
            if app.diff_loading {
                " Waiting for the diff to prepare the AI guide…".to_owned()
            } else {
                " AI guide is ready to generate · a explains this PR".to_owned()
            },
            ACCENT,
        ),
        AiStatus::Loading => (
            format!(" Preparing AI guide with {}…", app.settings.ai.provider),
            ACCENT,
        ),
        AiStatus::Ready(_) => (
            format!(
                " AI GUIDE · {} · a regenerates · n/p reading order · Enter jumps",
                app.settings.ai.provider
            ),
            ACCENT,
        ),
        AiStatus::Error(_) => (
            " AI guide unavailable · a retries · A chooses an agent".to_owned(),
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
                Layout::horizontal([Constraint::Min(50), Constraint::Length(38)]).areas(content)
            } else {
                [content, Rect::default()]
            };
            let rows = overview_rows(analysis, narrative.width, !wide);
            let scroll = draw_overview_rows(frame, &rows, overview, narrative);
            if wide {
                draw_reading_guide(frame, analysis, overview.selected_step, guide);
            }
            overview.scroll = scroll;
        }
        status => {
            let mut rows = Vec::new();
            match status {
                AiStatus::Disabled => {
                    overview_heading(&mut rows, "SET UP YOUR AI GUIDE");
                    overview_text(
                        &mut rows,
                        "Choose Codex or Claude with a or A. PR details and the Diff tab remain available without an AI agent.",
                        content.width,
                        2,
                        TEXT,
                        None,
                    );
                    overview_text(
                        &mut rows,
                        &format!(
                            "Model: {} · Language: {}",
                            app.settings.ai.model.as_deref().unwrap_or("agent default"),
                            app.settings.ai.language
                        ),
                        content.width,
                        2,
                        MUTED,
                        None,
                    );
                }
                AiStatus::Waiting => {
                    overview_heading(&mut rows, "PR DETAILS ARE READY");
                    overview_text(
                        &mut rows,
                        if app.diff_loading {
                            "The guide will use the PR description and changed code when the diff arrives. You can read the description now or switch to 2 Diff."
                        } else {
                            "Press a to prepare the guide from the PR description and changed code, or switch to 2 Diff to start reviewing."
                        },
                        content.width,
                        2,
                        TEXT,
                        None,
                    );
                }
                AiStatus::Loading => {
                    overview_heading(&mut rows, "PREPARING THE REVIEW GUIDE");
                    overview_text(
                        &mut rows,
                        "The explanation is being prepared. You can switch to 2 Diff and start reviewing while it runs.",
                        content.width,
                        2,
                        TEXT,
                        None,
                    );
                }
                AiStatus::Error(error) => {
                    overview_heading(&mut rows, "AGENT DETAILS");
                    overview_text(&mut rows, error, content.width, 2, RED, None);
                    overview_text(
                        &mut rows,
                        "Press a to try again, A to choose another agent, or 2 to inspect the diff.",
                        content.width,
                        2,
                        TEXT,
                        None,
                    );
                }
                AiStatus::Ready(_) => unreachable!(),
            }
            overview.scroll = draw_overview_rows(frame, &rows, overview, content);
        }
    }
}

struct OverviewRow {
    line: Line<'static>,
    step: Option<usize>,
}

fn overview_heading(rows: &mut Vec<OverviewRow>, heading: &str) {
    if !rows.is_empty() {
        rows.push(OverviewRow {
            line: Line::from(""),
            step: None,
        });
    }
    rows.push(OverviewRow {
        line: Line::from(Span::styled(
            format!(" {heading}"),
            Style::default().fg(ACCENT).bold(),
        )),
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
    for line in wrap_comment(text, usize::from(width).saturating_sub(indent + 1).max(1)) {
        rows.push(OverviewRow {
            line: Line::from(Span::styled(
                format!("{}{line}", " ".repeat(indent)),
                Style::default().fg(color),
            )),
            step,
        });
    }
}

fn reading_step_label(index: usize, step: &pr_review::analysis::ReadingStep) -> String {
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
    overview_heading(rows, "READING ORDER · n/p chooses · Enter opens diff");
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
        overview_text(
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
    overview_heading(&mut rows, "SUMMARY");
    overview_text(&mut rows, &analysis.summary, width, 2, TEXT, None);
    let basis = match analysis.intent.basis.as_str() {
        "author_description" => "from author description",
        "inferred_from_diff" => "inferred from diff",
        _ => "unknown basis",
    };
    overview_heading(&mut rows, &format!("INTENT · {basis}"));
    overview_text(&mut rows, &analysis.intent.text, width, 2, TEXT, None);
    overview_heading(&mut rows, "MAIN CHANGES");
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
        overview_text(&mut rows, &change.title, width, 2, ACCENT, None);
        overview_text(&mut rows, &change.detail, width, 4, TEXT, None);
        if !change.files.is_empty() {
            overview_text(
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
    overview_heading(&mut rows, "REVIEW RISKS");
    if analysis.risks.is_empty() {
        overview_text(&mut rows, "No risks were listed.", width, 2, MUTED, None);
    }
    for risk in &analysis.risks {
        overview_text(&mut rows, &risk.title, width, 2, ACCENT, None);
        overview_text(&mut rows, &risk.detail, width, 4, TEXT, None);
        if !risk.files.is_empty() {
            overview_text(
                &mut rows,
                &format!("Files: {}", risk.files.join(", ")),
                width,
                4,
                MUTED,
                None,
            );
        }
    }
    overview_heading(&mut rows, "VERIFICATION");
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
        overview_text(&mut rows, &format!("- {test}"), width, 4, TEXT, None);
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
    overview_heading(&mut rows, "UNKNOWNS");
    if analysis.verification.unknowns.is_empty() {
        overview_text(&mut rows, "No unknowns were listed.", width, 2, MUTED, None);
    }
    for unknown in &analysis.verification.unknowns {
        overview_text(&mut rows, &format!("- {unknown}"), width, 2, TEXT, None);
    }
    overview_heading(&mut rows, "CONTEXT COVERAGE");
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
        overview_text(
            &mut rows,
            &format!("Omitted: {file}"),
            width,
            2,
            MUTED,
            None,
        );
    }
    for file in &analysis.coverage.unavailable_files {
        overview_text(
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
            .map_or(usize::from(overview.scroll), |row| row.saturating_sub(2))
    } else {
        usize::from(overview.scroll)
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
        Paragraph::new(" READING ORDER\n n/p chooses · Enter jumps")
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
                wrap_comment(
                    &step.reason,
                    usize::from(list.width).saturating_sub(3).max(1),
                )
                .into_iter()
                .map(|line| Line::from(format!(" {line}"))),
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

fn draw_inbox(frame: &mut Frame, app: &App, area: Rect) {
    let prs = app.visible_prs();
    let title = if app.filtering {
        format!(" Filter: {}_", app.filter)
    } else if app.filter.is_empty() {
        " PULL REQUESTS".into()
    } else {
        format!(" Filter: {}", app.filter)
    };
    let [heading, list] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(title).style(Style::default().fg(MUTED)),
        heading,
    );
    if prs.is_empty() {
        frame.render_widget(Paragraph::new(if app.busy { "  Loading pull requests..." } else { "  No matching pull requests.\n\n  # opens a PR by number.\n  / filters this list.\n  r refreshes the queue." }).style(Style::default().fg(MUTED)).wrap(Wrap { trim: false }), list);
        return;
    }
    let items = prs
        .iter()
        .map(|pr| {
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!("{} #{}", pr.id.repo, pr.id.number),
                        Style::default().fg(ACCENT),
                    ),
                    Span::styled(
                        format!("  {}{}", pr.author, if pr.draft { " · draft" } else { "" }),
                        Style::default().fg(MUTED),
                    ),
                ]),
                Line::from(Span::styled(pr.title.clone(), Style::default().fg(TEXT))),
                Line::from(""),
            ])
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.selected_pr));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(Style::default().bg(PANEL))
            .highlight_symbol(" › "),
        list,
        &mut state,
    );
}

struct SidebarRow {
    line: Line<'static>,
    file: Option<usize>,
}

fn sidebar_rows(review: &ReviewState, width: u16) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    let mut directories = HashSet::new();
    for (index, file) in review.data.files.iter().enumerate() {
        let parts = file.filename.split('/').collect::<Vec<_>>();
        for depth in 0..parts.len().saturating_sub(1) {
            let path = parts[..=depth].join("/");
            if directories.insert(path) {
                let prefix = "  ".repeat(depth.min(3));
                rows.push(SidebarRow {
                    file: None,
                    line: Line::from(Span::styled(
                        truncate(&format!(" {prefix}▾ {}", parts[depth]), usize::from(width)),
                        Style::default().fg(MUTED),
                    )),
                });
            }
        }
        let indentation = "  ".repeat(parts.len().saturating_sub(1).min(3));
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
    let width = if area.width >= 100 {
        31
    } else if area.width >= 70 {
        23
    } else {
        16
    };
    let [files_area, diff_area] =
        Layout::horizontal([Constraint::Length(width), Constraint::Min(20)]).areas(area);
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

    let [file_header, content] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(diff_area);
    draw_file_header(frame, review, app.comments_loading, file_header);
    let rows = review_rows(review, content.width);
    let start = viewport_start(&rows, review.position(), usize::from(content.height));
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
    let comments = threads(&review.data.comments)
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
    frame.render_widget(
        Paragraph::new(format!(
            " {} · {drafts} draft{} · {}",
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
        ))
        .style(Style::default().fg(MUTED)),
        metadata,
    );
}

struct ViewRow {
    line: Line<'static>,
    source: Option<usize>,
    background: Color,
}

impl ViewRow {
    fn text(text: impl Into<String>, background: Color, foreground: Color) -> Self {
        Self {
            line: Line::from(Span::styled(text.into(), Style::default().fg(foreground))),
            source: None,
            background,
        }
    }
}

fn review_rows(review: &ReviewState, width: u16) -> Vec<ViewRow> {
    let mut rows = Vec::new();
    let Some(file) = review.current_file() else {
        return rows;
    };
    let lines = review.current_lines();
    let all_threads = threads(&review.data.comments);
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
                " o opens source · T shows all comments",
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
    for (index, line) in lines.iter().enumerate() {
        if line.kind == LineKind::Hunk && index > 0 {
            rows.push(ViewRow::text("", BACKGROUND, MUTED));
        }
        let selected = review
            .selected_range()
            .map_or(index == review.position(), |(start, end)| {
                (start..=end).contains(&index)
            });
        let mut row = code_row(
            line,
            file,
            RowState {
                selected,
                cursor: index == review.position(),
                has_thread: anchored.contains_key(&index),
                has_draft: drafts.contains_key(&index),
            },
            review.horizontal,
            digits,
        );
        row.source = Some(index);
        rows.push(row);
        if review.show_comments {
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
        rows.push(ViewRow::text(
            " Threads outside this diff · T opens all",
            PANEL,
            MUTED,
        ));
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
        rows.push(ViewRow::text(
            " Drafts outside this diff · D opens all",
            PANEL,
            MUTED,
        ));
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
}

fn code_row(
    line: &DiffLine,
    file: &ChangedFile,
    state: RowState,
    horizontal: u16,
    digits: usize,
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
        Span::styled(format!("{marker}{old:>digits$} {new:>digits$} "), gutter),
        Span::styled(format!("{sign} "), Style::default().fg(sign_color)),
    ];
    spans.extend(syntax_spans(&code, &file.filename));
    ViewRow {
        line: Line::from(spans),
        source: None,
        background,
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
                "  │ … D then c reads full draft"
            } else {
                "  │ … T opens the full thread"
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

fn viewport_start(rows: &[ViewRow], source: usize, height: usize) -> usize {
    let selected = rows
        .iter()
        .position(|row| row.source == Some(source))
        .unwrap_or(0);
    selected
        .saturating_sub((height / 4).min(3))
        .min(rows.len().saturating_sub(height))
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

fn draw_dialog(frame: &mut Frame, app: &App, dialog: &Dialog) {
    match dialog {
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
                " Published reviews / j,k scroll / Esc closes ".into(),
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
            frame.render_widget(Paragraph::new("NAVIGATE\n  j/k, arrows     Move through PRs, files, or diff lines\n  Tab / Shift-Tab Overview / Diff views\n  1 / 2           Jump to Overview / Diff\n  h / l           Focus file tree / diff pane\n  n / p           Next / previous file\n  ] / [           Next / previous hunk\n  gg / ge         First / last item\n  gf / o          Open the selected file in Helix\n  Ctrl-d / Ctrl-u Half-page down / up\n  Normal j/k      Continue into adjacent files\n  Left / Right    Scroll diff horizontally\n\nOVERVIEW\n  j/k             Scroll explanation\n  n/p             Choose reading-guide step\n  Enter / o / gf  Open selected step in Diff\n  a               Generate or regenerate AI guide\n  A               Choose Codex, Claude, or no AI\n  b               Full author description\n\nSELECT\n  v               Toggle selection extension\n  x               Select line; repeat to grow downward\n  X               Extend selection to whole-line bounds\n  ;               Collapse selection to the cursor\n  Esc             Clear selection before leaving review\n\nREVIEW\n  b               PR description\n  t               Toggle inline comments and drafts\n  T               Full comment threads and replies\n  o               Open selected PR, file, or draft\n  Ctrl-h          Backspace in text editors\n  #               Open a PR by number\n  V               Published review summaries\n  u               Recover an uncertain submission\n  c               Comment on selected diff lines\n  D               Inspect, edit, or delete saved drafts\n  s               Write summary, preview, and submit review\n  * / +           Line has a local draft / existing comment\n\nLOCAL CODE\n  w               Prepare isolated Jujutsu workspace\n  o / gf          Prepare if needed, then open file in Helix\n                  Deleted lines open a base-revision snapshot\n\n  !               Full operation details\n\n  r refresh       Esc inbox       q quit\n  Ctrl-c exits immediately; saved drafts remain on disk").style(Style::default().fg(TEXT)).wrap(Wrap { trim: false }).scroll((*scroll, 0)), inner);
        }
        Dialog::Description { scroll } => {
            let inner = popup(
                frame,
                " PR description / j,k scroll / Esc closes ".into(),
                100,
                frame.area().height.saturating_sub(4),
            );
            let body = if let Some(overview) = &app.overview {
                let metadata = &overview.metadata;
                format!(
                    "{}\n{}\nby {}{}\n\n{}",
                    metadata.title,
                    metadata.url,
                    metadata.author,
                    if metadata.draft { " (draft)" } else { "" },
                    metadata.body
                )
            } else if let Some(review) = &app.review {
                let pr = &review.data.pr;
                format!(
                    "{}\n{}\nby {}{}\n\n{}",
                    pr.title,
                    pr.url,
                    pr.author,
                    if pr.draft { " (draft)" } else { "" },
                    pr.body
                )
            } else {
                "No pull request is selected.".into()
            };
            frame.render_widget(
                Paragraph::new(body)
                    .wrap(Wrap { trim: false })
                    .scroll((*scroll, 0)),
                inner,
            );
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
            for thread in threads(&review.data.comments) {
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
    use pr_review::model::{DraftComment, ReviewComment};
    use ratatui::{Terminal, backend::TestBackend};

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
                event: pr_review::model::ReviewEvent::Comment,
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
            let rows = review_rows(review, width);
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
        let rows = review_rows(review, 57);
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
        let rows = review_rows(review, 32);
        let last_source = rows.iter().rposition(|row| row.source.is_some()).unwrap();
        let outdated = rows
            .iter()
            .position(|row| row_text(row).contains("outdated ·"))
            .unwrap();
        assert!(outdated > last_source);
        assert!(rows[outdated].source.is_none());
        review.data.files[0].patch = None;
        review.lines[0].clear();
        let rows = review_rows(review, 32);
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
                .any(|row| row_text(row).contains("T opens the full thread"))
        );
        for line in wrap_comment(&text, 27) {
            assert!(Line::from(line).width() <= 27);
        }
        let mut drafts = Vec::new();
        append_card(&mut drafts, "draft", &text, 32, true, false);
        assert!(
            drafts
                .iter()
                .any(|row| row_text(row).contains("D then c reads full draft"))
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
        let rows = review_rows(review, 89);
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
        app.goto_pending = true;
        let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("g: g top · e end · f open"));
        app.loaded(demo_data()).unwrap();
        app.goto_pending = true;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("g: g top · e end · f open"));
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
        let rows = review_rows(review, 32);
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
                event: pr_review::model::ReviewEvent::Comment,
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

    fn overview_metadata() -> pr_review::model::PrOverview {
        let data = demo_data();
        pr_review::model::PrOverview {
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
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let output = screen(&terminal);
                assert!(output.contains("Keep request retries within the deadline"));
                assert!(output.contains("Bound retry waits by the remaining deadline."));
                assert!(output.contains("https://github.com/example/service/pull/42"));
                assert!(output.contains("1 Overview"));
                assert!(output.contains("2 Diff"));
                assert!(output.contains(message));
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
    fn structured_overview_includes_all_sections_and_keeps_reading_steps_visible() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().into(), true);
        app.settings.ai.provider = Provider::Codex;
        app.opened(overview_metadata());
        app.overview.as_mut().unwrap().ai = AiStatus::Ready(Box::new(analysis_fixture()));
        let rows = overview_rows(&analysis_fixture(), 80, true);
        let text = rows
            .iter()
            .flat_map(|row| row.line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<String>();
        for section in [
            "SUMMARY",
            "INTENT",
            "MAIN CHANGES",
            "READING ORDER",
            "REVIEW RISKS",
            "VERIFICATION",
            "UNKNOWNS",
            "CONTEXT COVERAGE",
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
            app.overview.as_mut().unwrap().scroll = u16::MAX;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let output = screen(&terminal);
            assert!(output.contains("CONTEXT COVERAGE"));
            assert!(output.contains("diagram.png"));
            let end = app.overview.as_ref().unwrap().scroll;
            assert!(end > 0 && end < u16::MAX);
            app.key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('k'),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert_eq!(app.overview.as_ref().unwrap().scroll, end - 1);
            app.overview.as_mut().unwrap().scroll = 0;
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
}
