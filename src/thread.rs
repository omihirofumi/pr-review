use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    diff::DiffLine,
    model::{ChangedFile, ReviewComment, Side},
};

pub struct Thread<'a> {
    pub root: &'a ReviewComment,
    pub replies: Vec<&'a ReviewComment>,
}

impl Thread<'_> {
    pub fn line_label(&self) -> String {
        self.root.line_label()
    }

    pub fn anchor(&self) -> Option<(Side, u32)> {
        let side = match self.root.side.as_str() {
            "LEFT" => Side::Left,
            "RIGHT" => Side::Right,
            _ => return None,
        };
        self.root
            .line
            .filter(|&line| line > 0)
            .map(|line| (side, line))
    }

    pub fn matches_file(&self, file: &ChangedFile) -> bool {
        self.root.path == file.filename
            || file.previous_filename.as_deref() == Some(self.root.path.as_str())
    }
}

pub fn matches_anchor(line: &DiffLine, side: &Side, number: u32) -> bool {
    number > 0
        && match side {
            Side::Left => line.old_line == Some(number),
            Side::Right => line.new_line == Some(number),
        }
}

pub fn threads(comments: &[ReviewComment]) -> Vec<Thread<'_>> {
    let by_id: HashMap<_, _> = comments
        .iter()
        .map(|comment| (comment.id, comment))
        .collect();
    let mut groups: BTreeMap<u64, Vec<&ReviewComment>> = BTreeMap::new();
    for comment in comments {
        let mut current = comment;
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.id) {
                current = by_id[seen.iter().min().unwrap()];
                break;
            }
            let Some(parent) = current.in_reply_to_id.and_then(|id| by_id.get(&id)) else {
                break;
            };
            current = parent;
        }
        groups.entry(current.id).or_default().push(comment);
    }
    groups
        .into_iter()
        .map(|(id, mut comments)| {
            comments.sort_by_key(|comment| comment.id);
            Thread {
                root: by_id[&id],
                replies: comments
                    .into_iter()
                    .filter(|comment| comment.id != id)
                    .collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_patch;

    fn comment(id: u64, parent: Option<u64>) -> ReviewComment {
        ReviewComment {
            start_line: None,
            start_side: None,
            id,
            in_reply_to_id: parent,
            path: "new.rs".into(),
            body: format!("Comment {id}"),
            author: "reviewer".into(),
            line: Some(8),
            original_line: Some(7),
            side: "RIGHT".into(),
        }
    }

    #[test]
    fn replies_remain_with_the_root_regardless_of_input_order() {
        let comments = [
            comment(3, Some(2)),
            comment(1, None),
            comment(2, Some(1)),
            comment(4, None),
        ];
        let grouped = threads(&comments);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].root.id, 1);
        assert_eq!(
            grouped[0].replies.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(grouped[1].root.id, 4);
    }

    #[test]
    fn orphaned_replies_and_cycles_are_displayed_once() {
        let comments = [
            comment(1, Some(99)),
            comment(2, Some(3)),
            comment(3, Some(2)),
        ];
        let grouped = threads(&comments);
        let mut ids = grouped
            .iter()
            .flat_map(|thread| {
                std::iter::once(thread.root.id).chain(thread.replies.iter().map(|c| c.id))
            })
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn outdated_threads_never_anchor_to_original_line_numbers() {
        let mut root = comment(1, None);
        root.line = None;
        let comments = [root, comment(2, Some(1))];
        assert!(threads(&comments)[0].anchor().is_none());
    }

    #[test]
    fn left_context_and_renamed_paths_match_the_actual_diff() {
        let patch = parse_patch("@@ -7,2 +8,2 @@\n context\n-old\n+new");
        assert!(matches_anchor(&patch[1], &Side::Left, 7));
        assert!(matches_anchor(&patch[1], &Side::Right, 8));
        assert!(!matches_anchor(&patch[2], &Side::Right, 9));
        let comments = [comment(1, None)];
        let file = ChangedFile {
            filename: "latest.rs".into(),
            previous_filename: Some("new.rs".into()),
            status: "renamed".into(),
            additions: 1,
            deletions: 1,
            patch: None,
        };
        assert!(threads(&comments)[0].matches_file(&file));
    }

    #[test]
    fn multiline_thread_labels_keep_the_end_line_as_the_navigation_anchor() {
        let mut root = comment(1, None);
        root.start_line = Some(6);
        root.start_side = Some("RIGHT".into());
        let comments = [root, comment(2, Some(1))];
        let grouped = threads(&comments);
        assert_eq!(grouped[0].line_label(), "6–8");
        assert_eq!(grouped[0].anchor(), Some((Side::Right, 8)));
    }
}
