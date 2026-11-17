use crate::model::{ChangedFile, DraftComment, Side};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Hunk,
    Context,
    Addition,
    Deletion,
    Meta,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub text: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub kind: LineKind,
}

impl DiffLine {
    pub fn anchor(&self) -> Option<(Side, u32)> {
        match self.kind {
            LineKind::Deletion => self.old_line.map(|line| (Side::Left, line)),
            LineKind::Addition | LineKind::Context => self.new_line.map(|line| (Side::Right, line)),
            LineKind::Hunk | LineKind::Meta => None,
        }
    }
}

pub fn parse_patch(patch: &str) -> Vec<DiffLine> {
    let mut result = Vec::new();
    let mut hunk = Vec::new();
    for text in patch.lines() {
        if text.starts_with("@@") {
            append_hunk(&mut result, &hunk);
            hunk.clear();
        }
        if !hunk.is_empty() || text.starts_with("@@") {
            hunk.push(text);
        } else {
            result.push(display_line(text));
        }
    }
    append_hunk(&mut result, &hunk);
    result
}

pub fn validate_comment(file: &ChangedFile, comment: &DraftComment) -> bool {
    if file.filename != comment.path || comment.line == 0 {
        return false;
    }
    let start = match (comment.start_line, comment.start_side.as_ref()) {
        (None, None) => comment.line,
        (Some(start), Some(side)) if start > 0 && start < comment.line && side == &comment.side => {
            start
        }
        _ => return false,
    };
    file.patch.as_deref().is_some_and(|patch| {
        parse_patch(patch)
            .split(|line| line.kind == LineKind::Hunk)
            .any(|hunk| {
                let mut expected = start;
                for number in hunk.iter().filter_map(|line| match comment.side {
                    Side::Left => line.old_line,
                    Side::Right => line.new_line,
                }) {
                    if number < start {
                        continue;
                    }
                    if number != expected {
                        return false;
                    }
                    if number == comment.line {
                        return true;
                    }
                    expected += 1;
                }
                false
            })
    })
}

fn display_line(text: &str) -> DiffLine {
    DiffLine {
        text: text.to_owned(),
        old_line: None,
        new_line: None,
        kind: LineKind::Meta,
    }
}

fn append_hunk(result: &mut Vec<DiffLine>, raw: &[&str]) {
    let Some(header) = raw.first() else {
        return;
    };
    let start = result.len();
    result.extend(raw.iter().map(|text| display_line(text)));
    result[start].kind = LineKind::Hunk;
    let Some(((mut old, old_count), (mut new, new_count))) = parse_header(header) else {
        return;
    };
    let mut old_remaining = old_count;
    let mut new_remaining = new_count;
    let mut valid = true;
    let mut has_content = false;
    for line in &mut result[start + 1..] {
        if line.text == "\\ No newline at end of file" {
            valid &= has_content;
            continue;
        }
        has_content = true;
        match line.text.as_bytes().first() {
            Some(b' ') if old_remaining > 0 && new_remaining > 0 => {
                line.kind = LineKind::Context;
                line.old_line = Some(old);
                line.new_line = Some(new);
                old_remaining -= 1;
                new_remaining -= 1;
                old = old.saturating_add(1);
                new = new.saturating_add(1);
            }
            Some(b'-') if old_remaining > 0 => {
                line.kind = LineKind::Deletion;
                line.old_line = Some(old);
                old_remaining -= 1;
                old = old.saturating_add(1);
            }
            Some(b'+') if new_remaining > 0 => {
                line.kind = LineKind::Addition;
                line.new_line = Some(new);
                new_remaining -= 1;
                new = new.saturating_add(1);
            }
            _ => valid = false,
        }
    }
    if !valid || old_remaining != 0 || new_remaining != 0 {
        for line in &mut result[start + 1..] {
            line.old_line = None;
            line.new_line = None;
        }
    }
}

type HunkRange = (u32, u32);

pub(crate) fn parse_header(header: &str) -> Option<(HunkRange, HunkRange)> {
    let (ranges, suffix) = header.strip_prefix("@@ ")?.split_once(" @@")?;
    if !suffix.is_empty() && !suffix.starts_with(' ') {
        return None;
    }
    let (old, new) = ranges.split_once(' ')?;
    Some((parse_range(old, '-')?, parse_range(new, '+')?))
}

fn parse_range(range: &str, prefix: char) -> Option<HunkRange> {
    let range = range.strip_prefix(prefix)?;
    let (start, count) = match range.split_once(',') {
        Some((start, count)) => (parse_number(start)?, parse_number(count)?),
        None => (parse_number(range)?, 1),
    };
    if count > 0 {
        if start == 0 {
            return None;
        }
        start.checked_add(count - 1)?;
    }
    Some((start, count))
}

fn parse_number(number: &str) -> Option<u32> {
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

/// Unified line indices shown side by side: runs of deletions pair with the additions that
/// follow, unchanged lines fill both sides, and hunk headers span the row alone.
pub fn split_pairs(lines: &[DiffLine]) -> Vec<(Option<usize>, Option<usize>)> {
    let mut pairs = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        match lines[index].kind {
            LineKind::Deletion => {
                let deletions = index;
                while index < lines.len() && lines[index].kind == LineKind::Deletion {
                    index += 1;
                }
                let additions = index;
                while index < lines.len() && lines[index].kind == LineKind::Addition {
                    index += 1;
                }
                let removed = additions - deletions;
                let added = index - additions;
                for offset in 0..removed.max(added) {
                    pairs.push((
                        (offset < removed).then_some(deletions + offset),
                        (offset < added).then_some(additions + offset),
                    ));
                }
            }
            LineKind::Addition => {
                pairs.push((None, Some(index)));
                index += 1;
            }
            LineKind::Hunk | LineKind::Meta => {
                pairs.push((Some(index), None));
                index += 1;
            }
            LineKind::Context => {
                pairs.push((Some(index), Some(index)));
                index += 1;
            }
        }
    }
    pairs
}

/// Line indices in the order split view visits them: row by row, the left half before the
/// right, matching Hunk so the cursor never jumps back up within a replaced block.
pub fn split_order(lines: &[DiffLine]) -> Vec<usize> {
    split_pairs(lines)
        .into_iter()
        .flat_map(|(left, right)| [left, right.filter(|right| Some(*right) != left)])
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(patch: Option<&str>) -> ChangedFile {
        ChangedFile {
            filename: "new.rs".into(),
            previous_filename: None,
            status: "modified".into(),
            additions: 1,
            deletions: 1,
            patch: patch.map(str::to_owned),
        }
    }

    fn comment(path: &str, side: Side, line: u32) -> DraftComment {
        DraftComment {
            start_line: None,
            start_side: None,
            path: path.into(),
            line,
            side,
            body: "Please check this invariant.".into(),
        }
    }

    #[test]
    fn replacement_preserves_both_coordinates() {
        let lines = parse_patch("@@ -3,3 +3,4 @@ fn main()\n first\n-old\n+new\n+extra\n last");
        assert_eq!(lines[1].old_line, Some(3));
        assert_eq!(lines[1].new_line, Some(3));
        assert_eq!(lines[2].anchor(), Some((Side::Left, 4)));
        assert_eq!(lines[3].anchor(), Some((Side::Right, 4)));
        assert_eq!(lines[4].anchor(), Some((Side::Right, 5)));
        assert_eq!(lines[5].old_line, Some(5));
        assert_eq!(lines[5].new_line, Some(6));
    }

    #[test]
    fn insertion_into_empty_file_has_no_left_anchor() {
        let lines = parse_patch("@@ -0,0 +1,2 @@\n+one\n+two\n");
        assert_eq!(lines[1].old_line, None);
        assert_eq!(lines[1].anchor(), Some((Side::Right, 1)));
        assert_eq!(lines[2].anchor(), Some((Side::Right, 2)));
    }

    #[test]
    fn deleted_file_has_no_right_anchor() {
        let lines = parse_patch("@@ -1,2 +0,0 @@\n-one\n-two");
        assert_eq!(lines[1].anchor(), Some((Side::Left, 1)));
        assert_eq!(lines[2].anchor(), Some((Side::Left, 2)));
        assert!(lines.iter().all(|line| line.new_line.is_none()));
    }

    #[test]
    fn multiple_hunks_reset_coordinates_and_skip_no_newline_markers() {
        let lines = parse_patch(
            "@@ -1 +1 @@\n-old\n+new\n@@ -50 +52 @@\n-end\n\\ No newline at end of file\n+fin\n\\ No newline at end of file",
        );
        assert_eq!(lines[4].anchor(), Some((Side::Left, 50)));
        assert_eq!(lines[5].anchor(), None);
        assert_eq!(lines[6].anchor(), Some((Side::Right, 52)));
        assert_eq!(lines[7].anchor(), None);
    }

    #[test]
    fn context_can_be_commented_on_either_side() {
        let changed = file(Some("@@ -10 +12 @@\n context"));
        assert!(validate_comment(
            &changed,
            &comment("new.rs", Side::Left, 10)
        ));
        assert!(validate_comment(
            &changed,
            &comment("new.rs", Side::Right, 12)
        ));
        assert!(!validate_comment(
            &changed,
            &comment("new.rs", Side::Left, 12)
        ));
        assert!(!validate_comment(
            &changed,
            &comment("new.rs", Side::Right, 10)
        ));
    }

    #[test]
    fn rename_uses_current_filename_for_both_sides() {
        let mut changed = file(Some("@@ -1 +1 @@\n-old\n+new"));
        changed.status = "renamed".into();
        changed.previous_filename = Some("old.rs".into());
        assert!(validate_comment(
            &changed,
            &comment("new.rs", Side::Left, 1)
        ));
        assert!(!validate_comment(
            &changed,
            &comment("old.rs", Side::Left, 1)
        ));
    }

    #[test]
    fn absent_patch_and_unseen_lines_cannot_be_commented_on() {
        assert!(!validate_comment(
            &file(None),
            &comment("new.rs", Side::Right, 1)
        ));
        let changed = file(Some("@@ -5 +5 @@\n-old\n+new"));
        assert!(!validate_comment(
            &changed,
            &comment("new.rs", Side::Right, 1)
        ));
        assert!(!validate_comment(
            &changed,
            &comment("new.rs", Side::Right, 0)
        ));
        assert!(!validate_comment(
            &changed,
            &comment("other.rs", Side::Right, 5)
        ));
    }

    #[test]
    fn truncated_or_malformed_hunks_expose_no_coordinates() {
        for patch in [
            "@@ -1,2 +1,2 @@\n-old\n+new",
            "@@ -1 +1 @@\n-old\n+new\n+extra",
            "@@ broken @@\n-old\n+new",
            "@@ -1 +1 @@@\n-old\n+new",
            "@@ -+1 +1 @@\n-old\n+new",
            "@@ -0 +1 @@\n-old\n+new",
            "@@ -1 +4294967295,2 @@\n-old\n+new\n+extra",
            "@@ -1 +1 @@\n\\ No newline at end of file\n-old\n+new",
        ] {
            let lines = parse_patch(patch);
            assert!(!lines.is_empty());
            assert!(
                lines
                    .iter()
                    .all(|line| line.old_line.is_none() && line.new_line.is_none()),
                "{patch}"
            );
        }
    }

    #[test]
    fn valid_hunk_after_truncation_still_has_accurate_anchors() {
        let lines = parse_patch("@@ -1,2 +1,2 @@\n-old\n+new\n@@ -9 +9 @@\n-nine\n+ninth");
        assert_eq!(lines[1].anchor(), None);
        assert_eq!(lines[4].anchor(), Some((Side::Left, 9)));
        assert_eq!(lines[5].anchor(), Some((Side::Right, 9)));
    }

    #[test]
    fn file_headers_are_not_commentable() {
        let lines = parse_patch(
            "diff --git a/new.rs b/new.rs\n--- a/new.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-old\n+new",
        );
        assert!(lines[..3].iter().all(|line| line.anchor().is_none()));
        assert_eq!(lines[4].anchor(), Some((Side::Left, 1)));
        assert_eq!(lines[5].anchor(), Some((Side::Right, 1)));
    }

    fn range(side: Side, start: u32, end: u32) -> DraftComment {
        DraftComment {
            path: "new.rs".into(),
            line: end,
            side: side.clone(),
            start_line: Some(start),
            start_side: Some(side),
            body: "This whole block needs a regression test.".into(),
        }
    }

    #[test]
    fn multiline_ranges_allow_interleaved_changes_on_the_other_side() {
        let changed = file(Some(
            "@@ -10,4 +10,5 @@\n context\n-old b\n-old c\n+new b\n+new c\n+extra\n last",
        ));
        assert!(validate_comment(&changed, &range(Side::Right, 10, 14)));
        assert!(validate_comment(&changed, &range(Side::Left, 10, 13)));
        assert!(validate_comment(&changed, &range(Side::Right, 11, 12)));
    }

    #[test]
    fn multiline_ranges_cannot_cross_hunks_even_when_numbers_are_adjacent() {
        for patch in [
            "@@ -1 +1 @@\n-old\n+new\n@@ -2 +2 @@\n-old\n+new",
            "@@ -1 +1 @@\n-old\n+new\n@@ -9 +9 @@\n-old\n+new",
        ] {
            let changed = file(Some(patch));
            for end in [2, 9] {
                assert!(!validate_comment(&changed, &range(Side::Right, 1, end)));
                assert!(!validate_comment(&changed, &range(Side::Left, 1, end)));
            }
        }
    }

    #[test]
    fn multiline_range_requires_paired_increasing_coordinates_on_one_side() {
        let changed = file(Some("@@ -1,3 +1,3 @@\n a\n b\n c"));
        let mut invalid = range(Side::Right, 1, 3);
        invalid.start_side = None;
        assert!(!validate_comment(&changed, &invalid));
        invalid.start_line = None;
        invalid.start_side = Some(Side::Right);
        assert!(!validate_comment(&changed, &invalid));
        invalid.start_line = Some(1);
        invalid.start_side = Some(Side::Left);
        assert!(!validate_comment(&changed, &invalid));
        for (start, end) in [(0, 3), (3, 1), (2, 2), (1, 4)] {
            assert!(!validate_comment(&changed, &range(Side::Right, start, end)));
        }
    }

    #[test]
    fn multiline_range_rejects_a_truncated_hunk() {
        let changed = file(Some("@@ -1,4 +1,4 @@\n a\n b\n c"));
        assert!(!validate_comment(&changed, &range(Side::Right, 1, 3)));
    }
}
