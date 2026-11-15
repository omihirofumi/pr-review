use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    diff::{self, DiffLine, LineKind},
    model::ChangedFile,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FileContext {
    pub base: String,
    pub head: String,
}

#[derive(Default)]
pub struct Expansion {
    pub radius: BTreeMap<usize, usize>,
    pub full: bool,
}

struct Hunk {
    suffix: String,
    old: usize,
    old_end: usize,
    new: usize,
    new_end: usize,
    rows: Vec<DiffLine>,
}

pub struct FullFile {
    base: Vec<String>,
    head: Vec<String>,
    hunks: Vec<Hunk>,
}

pub struct ContextView {
    pub hunk_positions: Vec<usize>,
    pub lines: Vec<DiffLine>,
    pub patch: String,
    pub patch_positions: Vec<Option<usize>>,
    pub groups: Vec<Vec<usize>>,
    pub row_groups: Vec<usize>,
}

impl ContextView {
    pub fn hunks_at(&self, position: usize) -> Vec<usize> {
        self.row_groups
            .get(position)
            .and_then(|group| self.groups.get(*group))
            .cloned()
            .unwrap_or_default()
    }

    pub fn question_focus(
        &self,
        file: &ChangedFile,
        position: usize,
        range: Option<(usize, usize)>,
    ) -> Result<crate::assistant::Focus> {
        let mut file = file.clone();
        file.patch = Some(self.patch.clone());
        let position = self
            .patch_positions
            .iter()
            .skip(position)
            .find_map(|position| *position)
            .or_else(|| {
                self.patch_positions
                    .iter()
                    .take(position)
                    .rev()
                    .find_map(|position| *position)
            })
            .unwrap_or(0);
        let range = range
            .map(|(start, end)| {
                let positions = self.patch_positions[start..=end]
                    .iter()
                    .filter_map(|position| *position)
                    .collect::<Vec<_>>();
                positions
                    .first()
                    .zip(positions.last())
                    .map(|(&start, &end)| (start, end))
                    .ok_or_else(|| anyhow::anyhow!("Select numbered source lines to ask about"))
            })
            .transpose()?;
        let rows = diff::parse_patch(&self.patch);
        let (first, last) = range.unwrap_or((position, position));
        let hunk = (0..=first)
            .rev()
            .find(|&index| rows[index].kind == LineKind::Hunk)
            .ok_or_else(|| anyhow::anyhow!("Choose a source hunk"))?;
        let end = (hunk + 1..rows.len())
            .find(|&index| rows[index].kind == LineKind::Hunk)
            .unwrap_or(rows.len());
        if last >= end {
            return crate::assistant::Focus::from_file(&file, position, range);
        }
        let mut start = first.saturating_sub(20).max(hunk + 1);
        let end = last.saturating_add(21).min(end);
        while start < end && rows[start].anchor().is_none() {
            start += 1;
        }
        ensure!(start < end, "Choose numbered source lines to ask about");
        let ((old, old_count), (new, new_count)) = diff::parse_header(&rows[hunk].text)
            .ok_or_else(|| anyhow::anyhow!("Invalid source hunk"))?;
        let old = if old_count == 0 { old } else { old - 1 } as usize
            + rows[hunk + 1..start]
                .iter()
                .filter(|row| row.old_line.is_some())
                .count();
        let new = if new_count == 0 { new } else { new - 1 } as usize
            + rows[hunk + 1..start]
                .iter()
                .filter(|row| row.new_line.is_some())
                .count();
        let old_count = rows[start..end]
            .iter()
            .filter(|row| row.old_line.is_some())
            .count();
        let new_count = rows[start..end]
            .iter()
            .filter(|row| row.new_line.is_some())
            .count();
        let header = format!(
            "@@ -{},{} +{},{} @@",
            old + usize::from(old_count > 0),
            old_count,
            new + usize::from(new_count > 0),
            new_count
        );
        file.patch = Some(
            std::iter::once(header)
                .chain(rows[start..end].iter().map(|row| row.text.clone()))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let position = position.saturating_sub(start) + 1;
        let range = range.map(|(first, last)| {
            (
                first.saturating_sub(start) + 1,
                last.saturating_sub(start) + 1,
            )
        });
        crate::assistant::Focus::from_file(&file, position, range)
    }

    pub fn position_for_hunk(&self, hunk: usize) -> usize {
        self.lines
            .iter()
            .enumerate()
            .find(|(index, row)| {
                row.kind == LineKind::Hunk && self.hunks_at(*index).contains(&hunk)
            })
            .map_or(0, |(index, _)| index)
    }
}

#[derive(Default)]
pub struct FileState {
    pub full_file: Option<FullFile>,
    pub expansion: Expansion,
    pub view: Option<ContextView>,
    pub loading: bool,
}

impl FullFile {
    pub fn new(file: &ChangedFile, source: &FileContext) -> Result<Self> {
        ensure!(
            !source.base.contains('\0') && !source.head.contains('\0'),
            "Binary files cannot be expanded"
        );
        let base = source.base.lines().map(str::to_owned).collect::<Vec<_>>();
        let head = source.head.lines().map(str::to_owned).collect::<Vec<_>>();
        ensure!(
            base.len() <= 100_000 && head.len() <= 100_000,
            "File context exceeds 100000 lines; open it in Helix"
        );
        let rows = diff::parse_patch(file.patch.as_deref().unwrap_or_default());
        let mut hunks = Vec::new();
        let mut index = 0;
        while index < rows.len() {
            ensure!(
                rows[index].kind == LineKind::Hunk,
                "The patch contains unsupported metadata; open the file in Helix"
            );
            let ((old, old_count), (new, new_count)) = diff::parse_header(&rows[index].text)
                .ok_or_else(|| anyhow::anyhow!("The patch has an incomplete hunk"))?;
            let suffix = rows[index]
                .text
                .split_once(" @@")
                .map_or("", |(_, suffix)| suffix)
                .to_owned();
            let old = if old_count == 0 { old } else { old - 1 } as usize;
            let new = if new_count == 0 { new } else { new - 1 } as usize;
            let old_end = old + old_count as usize;
            let new_end = new + new_count as usize;
            ensure!(
                old_end <= base.len() && new_end <= head.len(),
                "Patch coordinates do not match the reviewed file revisions"
            );
            index += 1;
            let start = index;
            let (mut left, mut right) = (old, new);
            while index < rows.len() && rows[index].kind != LineKind::Hunk {
                let row = &rows[index];
                let text = row.text.get(1..).unwrap_or("");
                match row.kind {
                    LineKind::Context => {
                        ensure!(
                            row.old_line == Some(left as u32 + 1)
                                && row.new_line == Some(right as u32 + 1)
                                && base.get(left).map(String::as_str) == Some(text)
                                && head.get(right).map(String::as_str) == Some(text),
                            "Patch context does not match the reviewed file revisions"
                        );
                        left += 1;
                        right += 1;
                    }
                    LineKind::Deletion => {
                        ensure!(
                            row.old_line == Some(left as u32 + 1)
                                && base.get(left).map(String::as_str) == Some(text),
                            "Removed lines do not match the diff base"
                        );
                        left += 1;
                    }
                    LineKind::Addition => {
                        ensure!(
                            row.new_line == Some(right as u32 + 1)
                                && head.get(right).map(String::as_str) == Some(text),
                            "Added lines do not match the PR head"
                        );
                        right += 1;
                    }
                    LineKind::Meta if row.text == "\\ No newline at end of file" => {}
                    _ => anyhow::bail!(
                        "An incomplete patch cannot be expanded safely; open the file in Helix"
                    ),
                }
                index += 1;
            }
            ensure!(
                left == old_end && right == new_end,
                "The patch has an incomplete hunk"
            );
            hunks.push(Hunk {
                suffix,
                old,
                old_end,
                new,
                new_end,
                rows: rows[start..index].to_vec(),
            });
        }
        let (mut old, mut new) = (0, 0);
        for hunk in &hunks {
            ensure!(
                hunk.old >= old && hunk.new >= new && base[old..hunk.old] == head[new..hunk.new],
                "The patch omits changes between hunks; refresh or open the file in Helix"
            );
            old = hunk.old_end;
            new = hunk.new_end;
        }
        ensure!(
            base[old..] == head[new..],
            "The patch omits trailing changes; refresh or open the file in Helix"
        );
        if hunks.is_empty() {
            hunks.push(Hunk {
                suffix: String::new(),
                old: 0,
                old_end: 0,
                new: 0,
                new_end: 0,
                rows: Vec::new(),
            });
        }
        Ok(Self { base, head, hunks })
    }

    pub fn render(&self, expansion: &Expansion) -> ContextView {
        struct Window {
            old: usize,
            old_end: usize,
            new: usize,
            new_end: usize,
            hunks: Vec<usize>,
        }
        let mut windows: Vec<Window> = Vec::new();
        if expansion.full {
            windows.push(Window {
                old: 0,
                old_end: self.base.len(),
                new: 0,
                new_end: self.head.len(),
                hunks: (0..self.hunks.len()).collect(),
            });
        } else {
            for (index, hunk) in self.hunks.iter().enumerate() {
                let radius = expansion.radius.get(&index).copied().unwrap_or(0);
                let before = if index == 0 {
                    hunk.old
                } else {
                    hunk.old - self.hunks[index - 1].old_end
                };
                let after = self
                    .hunks
                    .get(index + 1)
                    .map_or(self.base.len(), |next| next.old)
                    - hunk.old_end;
                let above = radius.min(before);
                let below = radius.min(after);
                let next = Window {
                    old: hunk.old - above,
                    old_end: hunk.old_end + below,
                    new: hunk.new - above,
                    new_end: hunk.new_end + below,
                    hunks: vec![index],
                };
                if let Some(last) = windows.last_mut()
                    && next.old <= last.old_end
                    && next.new <= last.new_end
                {
                    last.old_end = last.old_end.max(next.old_end);
                    last.new_end = last.new_end.max(next.new_end);
                    last.hunks.push(index);
                } else {
                    windows.push(next);
                }
            }
        }
        let groups = windows.iter().map(|window| window.hunks.clone()).collect();
        let mut view = ContextView {
            hunk_positions: Vec::new(),
            lines: Vec::new(),
            patch: String::new(),
            patch_positions: Vec::new(),
            groups,
            row_groups: Vec::new(),
        };
        let mut patch_rows = Vec::new();
        let mut previous = 0;
        for (group, window) in windows.iter().enumerate() {
            if window.old > previous {
                view.gap(window.old - previous, group);
            }
            let old_count = window.old_end - window.old;
            let new_count = window.new_end - window.new;
            let old = window.old + usize::from(old_count > 0);
            let new = window.new + usize::from(new_count > 0);
            view.push(
                DiffLine {
                    text: format!(
                        "@@ -{old},{old_count} +{new},{new_count} @@{}",
                        if expansion.full {
                            " full file"
                        } else {
                            &self.hunks[window.hunks[0]].suffix
                        }
                    ),
                    old_line: None,
                    new_line: None,
                    kind: LineKind::Hunk,
                },
                group,
                &mut patch_rows,
            );
            let header_position = view.lines.len() - 1;
            let (mut old, mut new) = (window.old, window.new);
            for &index in &window.hunks {
                let hunk = &self.hunks[index];
                while old < hunk.old {
                    view.push(self.unchanged(old, new), group, &mut patch_rows);
                    old += 1;
                    new += 1;
                }
                if !hunk.rows.is_empty() {
                    view.hunk_positions
                        .push(if !expansion.full && index == window.hunks[0] {
                            header_position
                        } else {
                            view.lines.len()
                        });
                }
                for row in &hunk.rows {
                    view.push(row.clone(), group, &mut patch_rows);
                }
                old = hunk.old_end;
                new = hunk.new_end;
            }
            while old < window.old_end {
                view.push(self.unchanged(old, new), group, &mut patch_rows);
                old += 1;
                new += 1;
            }
            previous = window.old_end;
        }
        if previous < self.base.len() {
            view.gap(self.base.len() - previous, windows.len().saturating_sub(1));
        }
        view.patch = patch_rows.join("\n");
        view
    }

    fn unchanged(&self, old: usize, new: usize) -> DiffLine {
        DiffLine {
            text: format!(" {}", self.base[old]),
            old_line: Some(old as u32 + 1),
            new_line: Some(new as u32 + 1),
            kind: LineKind::Context,
        }
    }
}

impl ContextView {
    fn push(&mut self, row: DiffLine, group: usize, patch: &mut Vec<String>) {
        self.patch_positions.push(Some(patch.len()));
        self.row_groups.push(group);
        patch.push(row.text.clone());
        self.lines.push(row);
    }

    fn gap(&mut self, count: usize, group: usize) {
        self.lines.push(DiffLine {
            text: format!("… {count} unchanged lines · + expands · F full file"),
            old_line: None,
            new_line: None,
            kind: LineKind::Meta,
        });
        self.patch_positions.push(None);
        self.row_groups.push(group);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Side;

    pub(super) fn fixture() -> (ChangedFile, FileContext) {
        let base = (1..=60)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let head = (1..=60)
            .map(|line| match line {
                10 => "changed 10\nextra 10\n".into(),
                40 => "changed 40\n".into(),
                _ => format!("line {line}\n"),
            })
            .collect();
        let file = ChangedFile { filename: "src/file.rs".into(), previous_filename: None, status: "modified".into(), additions: 3, deletions: 2,
            patch: Some("@@ -9,3 +9,4 @@ function_one\n line 9\n-line 10\n+changed 10\n+extra 10\n line 11\n@@ -39,3 +40,3 @@ function_two\n line 39\n-line 40\n+changed 40\n line 41".into()) };
        (file, FileContext { base, head })
    }

    #[test]
    fn expansion_and_full_file_preserve_changes_coordinates_and_complete_patch_ranges() {
        let (file, source) = fixture();
        let full = FullFile::new(&file, &source).unwrap();
        let original = diff::parse_patch(file.patch.as_deref().unwrap());
        let mut expansion = Expansion::default();
        let collapsed = full.render(&expansion);
        assert_eq!(
            collapsed
                .lines
                .iter()
                .filter(|line| line.anchor().is_some())
                .count(),
            original
                .iter()
                .filter(|line| line.anchor().is_some())
                .count()
        );
        assert!(collapsed.lines[0].text.contains("8 unchanged lines"));
        expansion.radius.insert(0, 20);
        let expanded = full.render(&expansion);
        assert!(
            expanded
                .lines
                .iter()
                .any(|line| line.old_line == Some(1) && line.new_line == Some(1))
        );
        assert!(
            expanded
                .lines
                .iter()
                .any(|line| line.old_line == Some(30) && line.new_line == Some(31))
        );
        for row in original.iter().filter(|row| row.anchor().is_some()) {
            assert!(expanded.lines.iter().any(|line| line == row));
        }
        expansion.radius.insert(1, 20);
        let merged = full.render(&expansion);
        assert_eq!(merged.groups, vec![vec![0, 1]]);
        assert_eq!(
            merged
                .lines
                .iter()
                .filter(|line| line.old_line == Some(9))
                .count(),
            1
        );
        expansion.full = true;
        let view = full.render(&expansion);
        assert_eq!(
            view.lines
                .iter()
                .filter(|row| row.kind == LineKind::Hunk)
                .count(),
            1
        );
        assert!(
            !view
                .lines
                .iter()
                .any(|row| row.text.contains("unchanged lines"))
        );
        assert_eq!(view.lines.last().unwrap().anchor(), Some((Side::Right, 61)));
        let reparsed = diff::parse_patch(&view.patch);
        assert_eq!(reparsed, view.lines);
        assert_eq!(
            reparsed
                .iter()
                .filter(|row| row.kind == LineKind::Addition)
                .count(),
            3
        );
        assert_eq!(
            reparsed
                .iter()
                .filter(|row| row.kind == LineKind::Deletion)
                .count(),
            2
        );
        let raw = file.patch.unwrap();
        let original_file = ChangedFile {
            patch: Some(raw),
            ..file
        };
        assert!(diff::validate_comment(
            &original_file,
            &crate::model::DraftComment {
                path: original_file.filename.clone(),
                line: 10,
                side: Side::Right,
                start_line: None,
                start_side: None,
                body: "review".into()
            }
        ));
        assert!(!diff::validate_comment(
            &original_file,
            &crate::model::DraftComment {
                path: original_file.filename.clone(),
                line: 1,
                side: Side::Right,
                start_line: None,
                start_side: None,
                body: "outside patch".into()
            }
        ));
    }

    #[test]
    fn zero_length_sides_empty_files_and_no_final_newline_are_preserved() {
        for (base, head, patch) in [
            ("tail\n", "new\ntail\n", "@@ -0,0 +1 @@\n+new"),
            ("keep\nold\n", "keep\n", "@@ -2 +1,0 @@\n-old"),
            ("", "new\n", "@@ -0,0 +1 @@\n+new"),
            ("old\n", "", "@@ -1 +0,0 @@\n-old"),
            (
                "old",
                "new",
                "@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file",
            ),
            ("", "", ""),
        ] {
            let (mut file, _) = fixture();
            file.patch = (!patch.is_empty()).then(|| patch.into());
            let source = FileContext {
                base: base.into(),
                head: head.into(),
            };
            let full = FullFile::new(&file, &source).unwrap();
            let view = full.render(&Expansion {
                full: true,
                ..Default::default()
            });
            assert_eq!(diff::parse_patch(&view.patch), view.lines, "{patch}");
        }
    }

    #[test]
    fn expansion_refuses_incomplete_patches_wrong_revisions_and_unreported_changes() {
        let (file, source) = fixture();
        let mut wrong = source.clone();
        wrong.head = wrong.head.replacen("line 1\n", "unreported\n", 1);
        assert!(FullFile::new(&file, &wrong).is_err());
        let mut wrong = source.clone();
        wrong.base = wrong.base.replacen("line 10\n", "wrong base\n", 1);
        assert!(FullFile::new(&file, &wrong).is_err());
        let mut truncated = file.clone();
        truncated.patch = Some("@@ -9,3 +9,4 @@\n line 9\n-line 10\n+changed 10".into());
        assert!(FullFile::new(&truncated, &source).is_err());
        let mut missing = file;
        missing.patch = None;
        assert!(FullFile::new(&missing, &source).is_err());
        assert!(
            FullFile::new(
                &missing,
                &FileContext {
                    base: "binary\0".into(),
                    head: "binary\0".into()
                }
            )
            .is_err()
        );
    }
    #[test]
    fn ai_focus_on_a_large_full_file_keeps_only_nearby_source_with_exact_coordinates() {
        let (mut file, _) = fixture();
        let base = (1..=5000)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let head = base.replace("line 2500\n", "changed 2500\n");
        file.patch = Some("@@ -2500 +2500 @@\n-line 2500\n+changed 2500".into());
        let full = FullFile::new(&file, &FileContext { base, head }).unwrap();
        let view = full.render(&Expansion {
            full: true,
            ..Default::default()
        });
        let index = view
            .lines
            .iter()
            .position(|row| row.new_line == Some(4000))
            .unwrap();
        let focus = view
            .question_focus(&file, index, Some((index, index)))
            .unwrap();
        assert_eq!(focus.selected_lines[0].old_line, Some(4000));
        assert_eq!(focus.selected_lines[0].new_line, Some(4000));
        assert!(focus.patch.len() < 2000);
        assert!(focus.patch.contains("line 3980") && focus.patch.contains("line 4020"));
        assert!(!focus.patch.contains("line 3900"));
        assert!(
            diff::parse_patch(&focus.patch)
                .iter()
                .filter(|row| row.kind != LineKind::Hunk)
                .all(|row| row.anchor().is_some())
        );
    }
}
