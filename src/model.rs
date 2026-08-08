use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_ATTACHMENTS: usize = 8;
pub const MAX_ATTACHMENT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_ATTACHMENT_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_ACTIVE_PER_BOARD: usize = 20;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeStatus {
    Running,
    #[default]
    Done,
    Error,
    Stopped,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopReason {
    User,
    AppQuit,
    Deleted,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub prompt: String,
    #[serde(default = "default_aspect")]
    pub aspect: String,
    #[serde(default)]
    pub source_images: Vec<String>,
    #[serde(default)]
    pub attachments: Vec<String>,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub image_labels: Vec<String>,
    #[serde(default)]
    pub attempts: Vec<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub status: NodeStatus,
    pub error: Option<String>,
    pub stop_reason: Option<StopReason>,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub created_at: i64,
    pub run_started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub usage: Option<BTreeMap<String, u64>>,
}

fn default_aspect() -> String {
    "auto".into()
}

impl BoardNode {
    /// Text the agent attached that the card does not already show elsewhere:
    /// the done footer covers `Done`, and the error line covers a summary that
    /// was reused as the error message.
    pub fn attached_text(&self) -> Option<&str> {
        if self.text.is_empty()
            || !matches!(self.status, NodeStatus::Error | NodeStatus::Stopped)
            || self.error.as_deref() == Some(self.text.as_str())
        {
            return None;
        }
        Some(&self.text)
    }

    pub fn token_count(&self) -> u64 {
        let Some(usage) = &self.usage else { return 0 };
        usage.get("input_tokens").copied().unwrap_or(0)
            + usage.get("output_tokens").copied().unwrap_or(0)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Board {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    #[serde(default)]
    pub nodes: Vec<BoardNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardSummary {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub image_count: usize,
    pub last_image: Option<String>,
    pub generating: bool,
    pub total_tokens: u64,
}

#[derive(Clone, Debug)]
pub struct NewNodesRequest {
    pub prompt: String,
    pub parent_id: Option<String>,
    pub source_images: Option<Vec<String>>,
    pub aspect: String,
    pub count: usize,
    pub attachment_paths: Vec<std::path::PathBuf>,
    pub attachment_urls: Vec<String>,
    /// Pins the new nodes at an explicit canvas position instead of letting
    /// the tree layout place them.
    pub position: Option<(f32, f32)>,
}
