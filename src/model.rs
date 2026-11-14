use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PrId {
    pub repo: String,
    pub number: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequest {
    pub id: PrId,
    pub title: String,
    pub body: String,
    pub url: String,
    pub author: String,
    pub head_sha: String,
    pub base_sha: String,
    pub draft: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PrSummary {
    pub id: PrId,
    pub title: String,
    pub author: String,
    pub draft: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PrOverview {
    pub id: PrId,
    pub title: String,
    pub body: String,
    pub url: String,
    pub author: String,
    pub head_sha: String,
    pub base_tip_sha: String,
    pub draft: bool,
    pub state: String,
    pub changed_files: u64,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChangedFile {
    pub filename: String,
    pub previous_filename: Option<String>,
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    pub patch: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReviewComment {
    pub id: u64,
    pub path: String,
    pub body: String,
    pub author: String,
    pub line: Option<u32>,
    pub original_line: Option<u32>,
    pub side: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_side: Option<String>,
    pub in_reply_to_id: Option<u64>,
}

impl ReviewComment {
    pub fn line_label(&self) -> String {
        match self.line {
            Some(line) => range_label(
                self.start_line
                    .filter(|_| self.start_side.as_deref() == Some(self.side.as_str())),
                line,
            ),
            None => "outdated".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReviewData {
    pub pr: PullRequest,
    pub files: Vec<ChangedFile>,
    pub comments: Vec<ReviewComment>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Side {
    #[serde(rename = "LEFT")]
    Left,
    #[serde(rename = "RIGHT")]
    Right,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DraftComment {
    pub path: String,
    pub line: u32,
    pub side: Side,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_side: Option<Side>,
    pub body: String,
}

impl DraftComment {
    pub fn line_label(&self) -> String {
        range_label(
            self.start_line
                .filter(|_| self.start_side.as_ref() == Some(&self.side)),
            self.line,
        )
    }
}

fn range_label(start: Option<u32>, end: u32) -> String {
    match start.filter(|&start| start > 0 && start < end) {
        Some(start) => format!("{start}–{end}"),
        None => end.to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewEvent {
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "COMMENT",
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_single_line_drafts_keep_their_json_shape() {
        let old = json!({"path": "a.rs", "line": 21, "side": "RIGHT", "body": "Please explain"});
        let draft: DraftComment = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(draft.start_line, None);
        assert_eq!(draft.start_side, None);
        assert_eq!(draft.line_label(), "21");
        assert_eq!(serde_json::to_value(draft).unwrap(), old);
    }

    #[test]
    fn ranges_serialize_with_explicit_sides_and_readable_labels() {
        let draft: DraftComment = serde_json::from_value(json!({
            "path": "a.rs", "line": 21, "side": "LEFT", "body": "Keep this invariant",
            "start_line": 19, "start_side": "LEFT"
        }))
        .unwrap();
        assert_eq!(draft.line_label(), "19–21");
        let value = serde_json::to_value(draft).unwrap();
        assert_eq!(value["start_line"], 19);
        assert_eq!(value["start_side"], "LEFT");
    }

    #[test]
    fn published_range_labels_do_not_reuse_outdated_coordinates() {
        let mut comment: ReviewComment = serde_json::from_value(json!({
            "id": 1, "path": "a.rs", "body": "Range feedback", "author": "reviewer",
            "line": 21, "original_line": 99, "side": "RIGHT", "in_reply_to_id": null,
            "start_line": 19, "start_side": "RIGHT"
        }))
        .unwrap();
        assert_eq!(comment.line_label(), "19–21");
        comment.start_side = Some("LEFT".into());
        assert_eq!(comment.line_label(), "21");
        comment.line = None;
        assert_eq!(comment.line_label(), "outdated");
    }
}
