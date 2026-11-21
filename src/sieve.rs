//! Residual Diff Sieve: deterministically subtracts verbatim relocations, cosmetic/whitespace
//! edits, test files, and Oracle-verified lines so only unaccounted production logic ("Dark Diff")
//! demands line-by-line human inspection.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::{
    diff::{DiffLine, LineKind},
    model::{ChangedFile, Side},
    oracle::{Verdict, Witness, is_test_file},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowClass {
    /// Context, hunk header, or metadata row (not a diff change).
    Unchanged,
    /// Production logic change not yet accounted for by move, mechanical rule, test, or Oracle verdict.
    Unaccounted,
    /// Covered by an Oracle witness the human reviewer marked `Intended`.
    Verified,
    /// Verbatim relocation to/from another file or hunk; edited lines inside a moved block stay `Unaccounted`.
    Moved {
        peer_file: String,
        peer_line: u32,
        /// True on the first line of a contiguous moved block so the UI can show provenance once.
        block_start: bool,
    },
    /// Whitespace-only re-indentation, blank diff line, or pure non-directive comment line.
    Mechanical,
    /// Changed line inside a test file or `#[cfg(test)]` / `#[test]` section.
    Test,
}

impl RowClass {
    pub fn is_unaccounted(&self) -> bool {
        matches!(self, Self::Unaccounted)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FileSieve {
    pub rows: Vec<RowClass>,
    pub unaccounted: usize,
    pub verified: usize,
    pub moved: usize,
    pub mechanical: usize,
    pub test: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SieveReport {
    pub files: Vec<FileSieve>,
    pub unaccounted: usize,
    pub verified: usize,
    pub moved: usize,
    pub mechanical: usize,
    pub test: usize,
}

#[derive(Clone)]
struct ChangedEntry {
    file: usize,
    row: usize,
    hunk: usize,
    line_no: u32,
    normalized: String,
}

pub fn analyze(
    files: &[ChangedFile],
    lines: &[Vec<DiffLine>],
    witnesses: &[Witness],
    verdicts: &BTreeMap<String, Verdict>,
) -> SieveReport {
    let mut classes: Vec<Vec<RowClass>> = lines
        .iter()
        .map(|file_lines| {
            file_lines
                .iter()
                .map(|row| match row.kind {
                    LineKind::Addition | LineKind::Deletion => RowClass::Unaccounted,
                    LineKind::Hunk | LineKind::Context | LineKind::Meta => RowClass::Unchanged,
                })
                .collect()
        })
        .collect();

    // 1. Classify test files and inline Rust `#[cfg(test)]` modules.
    for (file_idx, file) in files.iter().enumerate() {
        let Some(rows) = lines.get(file_idx) else {
            continue;
        };
        let whole_file_is_test = file_path_is_test(&file.filename);
        let mut in_test_module = false;
        for (row_idx, row) in rows.iter().enumerate() {
            let trimmed = row.text.get(1..).unwrap_or_default().trim();
            if trimmed.starts_with("#[cfg(test)]") {
                in_test_module = true;
            }
            if (whole_file_is_test || in_test_module)
                && matches!(row.kind, LineKind::Addition | LineKind::Deletion)
            {
                classes[file_idx][row_idx] = RowClass::Test;
            }
        }
    }

    // 2. Classify mechanical changes: blank lines, pure comments, and 1-to-1 whitespace-only replacements.
    for (file_idx, rows) in lines.iter().enumerate() {
        mark_mechanical(rows, &mut classes[file_idx]);
    }

    // 3. Detect exact verbatim moves across files and separate hunks (before Oracle so real moves show provenance).
    mark_verbatim_moves(files, lines, &mut classes);

    // 4. Mark lines covered by human-verified (`Intended`) Oracle witnesses (overrides everything except Unchanged).
    mark_verified(files, lines, witnesses, verdicts, &mut classes);

    let mut report = SieveReport {
        files: Vec::with_capacity(files.len()),
        ..Default::default()
    };
    for file_rows in classes {
        let mut file_sieve = FileSieve {
            rows: file_rows,
            ..Default::default()
        };
        for class in &file_sieve.rows {
            match class {
                RowClass::Unaccounted => file_sieve.unaccounted += 1,
                RowClass::Verified => file_sieve.verified += 1,
                RowClass::Moved { .. } => file_sieve.moved += 1,
                RowClass::Mechanical => file_sieve.mechanical += 1,
                RowClass::Test => file_sieve.test += 1,
                RowClass::Unchanged => {}
            }
        }
        report.unaccounted += file_sieve.unaccounted;
        report.verified += file_sieve.verified;
        report.moved += file_sieve.moved;
        report.mechanical += file_sieve.mechanical;
        report.test += file_sieve.test;
        report.files.push(file_sieve);
    }
    report
}

fn file_path_is_test(path: &str) -> bool {
    is_test_file(path, "")
}

fn is_pure_comment(trimmed: &str) -> bool {
    if !trimmed.starts_with("//") {
        return false;
    }
    let upper = trimmed.to_ascii_uppercase();
    !["TODO", "FIXME", "SAFETY", "HACK", "XXX"]
        .iter()
        .any(|keyword| upper.contains(keyword))
}

fn mark_mechanical(rows: &[DiffLine], classes: &mut [RowClass]) {
    for (idx, row) in rows.iter().enumerate() {
        if classes[idx] != RowClass::Unaccounted {
            continue;
        }
        if matches!(row.kind, LineKind::Addition | LineKind::Deletion) {
            let trimmed = row.text.get(1..).unwrap_or_default().trim();
            if trimmed.is_empty() || is_pure_comment(trimmed) {
                classes[idx] = RowClass::Mechanical;
            }
        }
    }

    let mut index = 0;
    while index < rows.len() {
        if rows[index].kind == LineKind::Deletion {
            let del_start = index;
            while index < rows.len() && rows[index].kind == LineKind::Deletion {
                index += 1;
            }
            let add_start = index;
            while index < rows.len() && rows[index].kind == LineKind::Addition {
                index += 1;
            }
            let add_end = index;
            let mut next_add = add_start;
            for del_idx in del_start..add_start {
                let left = rows[del_idx].text.get(1..).unwrap_or_default().trim();
                if left.is_empty() {
                    continue;
                }
                if let Some(found) = (next_add..add_end)
                    .find(|&add_idx| rows[add_idx].text.get(1..).unwrap_or_default().trim() == left)
                {
                    if classes[del_idx] == RowClass::Unaccounted {
                        classes[del_idx] = RowClass::Mechanical;
                    }
                    if classes[found] == RowClass::Unaccounted {
                        classes[found] = RowClass::Mechanical;
                    }
                    next_add = found + 1;
                }
            }
            continue;
        }
        index += 1;
    }
}

fn normalize_move_line(row: &DiffLine) -> String {
    row.text.get(1..).unwrap_or_default().trim().to_owned()
}

fn is_substantive_line(trimmed: &str) -> bool {
    trimmed
        .chars()
        .filter(|ch| ch.is_alphanumeric() || *ch == '_')
        .count()
        >= 4
}

fn collect_entries(
    lines: &[Vec<DiffLine>],
    classes: &[Vec<RowClass>],
    kind: LineKind,
) -> Vec<ChangedEntry> {
    let mut entries = Vec::new();
    for (file_idx, file_rows) in lines.iter().enumerate() {
        let mut hunk = 0usize;
        for (row_idx, row) in file_rows.iter().enumerate() {
            if row.kind == LineKind::Hunk {
                hunk += 1;
                continue;
            }
            if row.kind != kind || classes[file_idx][row_idx] != RowClass::Unaccounted {
                continue;
            }
            let normalized = normalize_move_line(row);
            if normalized.is_empty() {
                continue;
            }
            let line_no = if kind == LineKind::Deletion {
                row.old_line.unwrap_or(0)
            } else {
                row.new_line.unwrap_or(0)
            };
            entries.push(ChangedEntry {
                file: file_idx,
                row: row_idx,
                hunk,
                line_no,
                normalized,
            });
        }
    }
    entries
}

fn mark_verbatim_moves(
    files: &[ChangedFile],
    lines: &[Vec<DiffLine>],
    classes: &mut [Vec<RowClass>],
) {
    let deletions = collect_entries(lines, classes, LineKind::Deletion);
    let additions = collect_entries(lines, classes, LineKind::Addition);
    if deletions.is_empty() || additions.is_empty() {
        return;
    }

    let mut del_used = vec![false; deletions.len()];
    let mut add_used = vec![false; additions.len()];

    // Index deletion positions by normalized text for fast block extension.
    let mut del_by_text: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, entry) in deletions.iter().enumerate() {
        del_by_text
            .entry(entry.normalized.as_str())
            .or_default()
            .push(idx);
    }

    // Pass A: greedy multi-line contiguous verbatim runs (>= 2 lines, at least one substantive line,
    // from a different file or different hunk so adjacent edits inside one hunk aren't misread as moves).
    let mut add_idx = 0;
    while add_idx < additions.len() {
        if add_used[add_idx] {
            add_idx += 1;
            continue;
        }
        let mut best: Option<(usize, usize)> = None; // (del_start, length)
        if let Some(candidates) = del_by_text.get(additions[add_idx].normalized.as_str()) {
            for &del_start in candidates {
                if del_used[del_start] {
                    continue;
                }
                if deletions[del_start].file == additions[add_idx].file
                    && deletions[del_start].hunk == additions[add_idx].hunk
                {
                    continue;
                }
                let mut length = 0;
                while add_idx + length < additions.len()
                    && del_start + length < deletions.len()
                    && !add_used[add_idx + length]
                    && !del_used[del_start + length]
                    && additions[add_idx + length].file == additions[add_idx].file
                    && deletions[del_start + length].file == deletions[del_start].file
                    && additions[add_idx + length].row == additions[add_idx].row + length
                    && deletions[del_start + length].row == deletions[del_start].row + length
                    && additions[add_idx + length].normalized
                        == deletions[del_start + length].normalized
                {
                    length += 1;
                }
                let substantive = (0..length)
                    .any(|offset| is_substantive_line(&additions[add_idx + offset].normalized));
                if length >= 2 && substantive && best.is_none_or(|(_, best_len)| length > best_len)
                {
                    best = Some((del_start, length));
                }
            }
        }
        if let Some((del_start, length)) = best {
            for offset in 0..length {
                let d = &deletions[del_start + offset];
                let a = &additions[add_idx + offset];
                del_used[del_start + offset] = true;
                add_used[add_idx + offset] = true;
                classes[d.file][d.row] = RowClass::Moved {
                    peer_file: files[a.file].filename.clone(),
                    peer_line: a.line_no,
                    block_start: offset == 0,
                };
                classes[a.file][a.row] = RowClass::Moved {
                    peer_file: files[d.file]
                        .previous_filename
                        .clone()
                        .unwrap_or_else(|| files[d.file].filename.clone()),
                    peer_line: d.line_no,
                    block_start: offset == 0,
                };
            }
            add_idx += length;
        } else {
            add_idx += 1;
        }
    }

    // Pass B: unique single-line relocations across different hunks/files (>= 16 non-whitespace chars,
    // occurring once in remaining deletions and once in remaining additions).
    let mut remaining_del: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, entry) in deletions.iter().enumerate() {
        if !del_used[idx] {
            remaining_del
                .entry(entry.normalized.as_str())
                .or_default()
                .push(idx);
        }
    }
    let mut remaining_add: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, entry) in additions.iter().enumerate() {
        if !add_used[idx] {
            remaining_add
                .entry(entry.normalized.as_str())
                .or_default()
                .push(idx);
        }
    }
    for (text, add_indices) in remaining_add {
        let non_ws = text.chars().filter(|ch| !ch.is_whitespace()).count();
        if non_ws < 16 || add_indices.len() != 1 {
            continue;
        }
        let Some(del_indices) = remaining_del.get(text) else {
            continue;
        };
        if del_indices.len() != 1 {
            continue;
        }
        let a = &additions[add_indices[0]];
        let d = &deletions[del_indices[0]];
        if a.file == d.file && a.hunk == d.hunk {
            continue;
        }
        classes[d.file][d.row] = RowClass::Moved {
            peer_file: files[a.file].filename.clone(),
            peer_line: a.line_no,
            block_start: true,
        };
        classes[a.file][a.row] = RowClass::Moved {
            peer_file: files[d.file]
                .previous_filename
                .clone()
                .unwrap_or_else(|| files[d.file].filename.clone()),
            peer_line: d.line_no,
            block_start: true,
        };
    }
}

fn mark_verified(
    files: &[ChangedFile],
    lines: &[Vec<DiffLine>],
    witnesses: &[Witness],
    verdicts: &BTreeMap<String, Verdict>,
    classes: &mut [Vec<RowClass>],
) {
    for witness in witnesses {
        if verdicts.get(&witness.id()) != Some(&Verdict::Intended) {
            continue;
        }
        let Some(file_idx) = files.iter().position(|file| file.filename == witness.path) else {
            continue;
        };
        let start = witness.effective_start();
        let end = witness.line;
        for (row_idx, row) in lines[file_idx].iter().enumerate() {
            if !matches!(row.kind, LineKind::Addition | LineKind::Deletion) {
                continue;
            }
            let matches = match witness.side {
                Side::Left => row.old_line.is_some_and(|n| (start..=end).contains(&n)),
                Side::Right => row.new_line.is_some_and(|n| (start..=end).contains(&n)),
            };
            if matches {
                classes[file_idx][row_idx] = RowClass::Verified;
            }
        }
    }

    // In a contiguous `[Deletion+, Addition+]` replacement block, if every Addition in that block
    // has been verified as `Intended`, the old lines directly replaced by it are accounted for too.
    for (file_idx, rows) in lines.iter().enumerate() {
        let mut idx = 0;
        while idx < rows.len() {
            if rows[idx].kind == LineKind::Deletion {
                let del_start = idx;
                while idx < rows.len() && rows[idx].kind == LineKind::Deletion {
                    idx += 1;
                }
                let add_start = idx;
                while idx < rows.len() && rows[idx].kind == LineKind::Addition {
                    idx += 1;
                }
                let add_end = idx;
                if add_end > add_start
                    && (add_start..add_end).all(|r| classes[file_idx][r] == RowClass::Verified)
                {
                    classes[file_idx][del_start..add_start].fill(RowClass::Verified);
                }
                continue;
            }
            idx += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{diff::parse_patch, oracle::TestProof};

    fn changed(name: &str, patch: &str) -> ChangedFile {
        ChangedFile {
            filename: name.into(),
            previous_filename: None,
            status: "modified".into(),
            additions: 1,
            deletions: 1,
            patch: Some(patch.into()),
        }
    }

    #[test]
    fn moved_block_dims_verbatim_lines_but_exposes_sneaky_single_line_modification() {
        let old_file = changed(
            "src/legacy.rs",
            "@@ -10,6 +10,0 @@\n-fn compute_quota(limit: u64, used: u64) -> u64 {\n-    if used >= limit {\n-        return 0;\n-    }\n-    limit - used\n-}",
        );
        // Moved to `src/quota.rs`, EXCEPT `used >= limit` was quietly changed to `used > limit`!
        let new_file = changed(
            "src/quota.rs",
            "@@ -0,0 +1,6 @@\n+fn compute_quota(limit: u64, used: u64) -> u64 {\n+    if used > limit {\n+        return 0;\n+    }\n+    limit - used\n+}",
        );
        let files = vec![old_file, new_file];
        let lines = files
            .iter()
            .map(|f| parse_patch(f.patch.as_deref().unwrap()))
            .collect::<Vec<_>>();
        let report = analyze(&files, &lines, &[], &BTreeMap::new());

        // Header line (`fn compute_quota...`) is unique >= 16 non-ws chars -> Moved!
        assert!(matches!(report.files[1].rows[1], RowClass::Moved { .. }));
        // Sneakily edited condition (`if used > limit {`) MUST remain Unaccounted!
        assert_eq!(report.files[1].rows[2], RowClass::Unaccounted);
        assert_eq!(report.files[0].rows[2], RowClass::Unaccounted);
        // Remaining 4 lines (`return 0;`, `}`, `limit - used`, `}`) form a >= 2 verbatim run -> Moved!
        for (row, class) in report.files[1].rows.iter().enumerate().take(7).skip(3) {
            assert!(
                matches!(class, RowClass::Moved { .. }),
                "Expected row {row} to be Moved, got {class:?}"
            );
        }
        assert_eq!(report.unaccounted, 2);
    }

    #[test]
    fn intended_oracle_verdict_marks_replacement_verified_while_flagged_keeps_unaccounted() {
        let files = vec![
            changed(
                "src/client.rs",
                "@@ -18,2 +18,3 @@\n-    let delay = self.retry_delay;\n-    sleep(delay);\n+    let delay = self.retry_delay.min(budget);\n+    if delay.is_zero() { return Err(Error::DeadlineExceeded); }\n+    sleep(delay);",
            ),
            changed(
                "tests/deadline.rs",
                "@@ -0,0 +1,2 @@\n+#[test]\n+fn checks_deadline() {}",
            ),
        ];
        let lines = files
            .iter()
            .map(|f| parse_patch(f.patch.as_deref().unwrap()))
            .collect::<Vec<_>>();
        let witness = Witness {
            title: "Retry clamp".into(),
            input: "budget = 0".into(),
            before: "sleeps".into(),
            after: "Err".into(),
            path: "src/client.rs".into(),
            side: Side::Right,
            start_line: Some(18),
            line: 20,
            proof: TestProof::Covered,
            test_file: Some("tests/deadline.rs".into()),
            mechanism: "clamps".into(),
        };

        // Flagged verdict must NOT mark the code as verified.
        let mut flagged = BTreeMap::new();
        flagged.insert(witness.id(), Verdict::Flagged);
        let before_verify = analyze(&files, &lines, std::slice::from_ref(&witness), &flagged);
        assert_eq!(before_verify.files[0].unaccounted, 3);
        assert_eq!(before_verify.files[1].test, 2);
        assert_eq!(before_verify.files[1].unaccounted, 0);

        // Intended verdict marks both the additions and the replaced deletions in that block as Verified!
        let mut intended = BTreeMap::new();
        intended.insert(witness.id(), Verdict::Intended);
        let after_verify = analyze(&files, &lines, std::slice::from_ref(&witness), &intended);
        assert_eq!(after_verify.files[0].unaccounted, 0);
        assert_eq!(after_verify.files[0].verified, 5);
        assert_eq!(after_verify.unaccounted, 0);
    }

    #[test]
    fn indentation_and_pure_comments_are_mechanical_while_todo_comments_stay_unaccounted() {
        let files = vec![changed(
            "src/lib.rs",
            "@@ -1,1 +1,4 @@\n-fn run() { work(); }\n+    fn run() { work(); }\n+// Explain why this exists\n+// SAFETY: caller upholds pointer validity\n+",
        )];
        let lines = vec![parse_patch(files[0].patch.as_deref().unwrap())];
        let report = analyze(&files, &lines, &[], &BTreeMap::new());
        assert_eq!(report.files[0].rows[1], RowClass::Mechanical);
        assert_eq!(report.files[0].rows[2], RowClass::Mechanical);
        assert_eq!(report.files[0].rows[3], RowClass::Mechanical);
        // SAFETY comment must NOT be silently dismissed as mechanical!
        assert_eq!(report.files[0].rows[4], RowClass::Unaccounted);
        assert_eq!(report.files[0].rows[5], RowClass::Mechanical);
    }
}
