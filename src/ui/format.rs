//! Human-readable text for boards, nodes, and timestamps.

use crate::model::{Board, BoardNode, NodeStatus, StopReason};
use crate::storage::now_ms;
use gpui::ImageFormat;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub fn read_image_ratio(path: &Path) -> Option<f32> {
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    (width > 0 && height > 0).then_some(width as f32 / height as f32)
}

pub fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.)
    } else {
        tokens.to_string()
    }
}

pub fn done_footer(node: &BoardNode) -> String {
    let mut footer = format!(
        "✓ Finished{}",
        if node.images.len() > 1 {
            format!(" · {} images", node.images.len())
        } else {
            String::new()
        }
    );
    if !node.text.is_empty() {
        footer.push_str(" · ");
        footer.push_str(&single_line_excerpt(&node.text, 90));
    }
    if node.token_count() > 0 {
        footer.push_str(&format!(" · {} tok", format_tokens(node.token_count())));
    }
    footer
}

fn single_line_excerpt(text: &str, max_chars: usize) -> String {
    text.chars()
        .take(max_chars)
        .map(|character| match character {
            '\n' | '\r' => ' ',
            character => character,
        })
        .collect()
}

pub fn status_message(node: &BoardNode) -> String {
    let message = match node.status {
        NodeStatus::Error => node.error.as_deref().unwrap_or("Generation failed"),
        NodeStatus::Stopped => match node.stop_reason {
            Some(StopReason::User) => "Stopped by you.",
            Some(StopReason::AppQuit) => "Stopped when CodexImage quit.",
            Some(StopReason::Deleted) => "Stopped when this node was deleted.",
            None => "Stopped.",
        },
        NodeStatus::Running | NodeStatus::Done => "",
    };
    message
        .chars()
        .take(220)
        .map(|character| if character == '\n' { ' ' } else { character })
        .collect()
}

pub fn format_date(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp_millis(timestamp)
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%d.%m.%y")
                .to_string()
        })
        .unwrap_or_default()
}

pub fn time_ago(timestamp: i64) -> String {
    let seconds = ((now_ms() - timestamp) / 1_000).max(0);
    if seconds < 60 {
        "now".into()
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

pub fn status_label(node: &BoardNode) -> String {
    match node.status {
        NodeStatus::Running => "Generating".into(),
        NodeStatus::Error => "Failed".into(),
        NodeStatus::Stopped => "Stopped".into(),
        NodeStatus::Done => format!(
            "{} image{}",
            node.images.len(),
            if node.images.len() == 1 { "" } else { "s" }
        ),
    }
}

pub fn node_depths(board: &Board) -> HashMap<String, usize> {
    let by_id: HashMap<_, _> = board
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    board
        .nodes
        .iter()
        .map(|node| {
            let mut depth = 0;
            let mut current = node.parent_id.as_deref();
            let mut seen = HashSet::new();
            while let Some(id) = current {
                if !seen.insert(id) {
                    break;
                }
                let Some(parent) = by_id.get(id) else { break };
                depth += 1;
                current = parent.parent_id.as_deref();
            }
            (node.id.clone(), depth)
        })
        .collect()
}

pub fn image_format_for_path(path: &Path) -> Option<ImageFormat> {
    match path
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "webp" => Some(ImageFormat::Webp),
        "gif" => Some(ImageFormat::Gif),
        "svg" => Some(ImageFormat::Svg),
        "bmp" => Some(ImageFormat::Bmp),
        "tif" | "tiff" => Some(ImageFormat::Tiff),
        "ico" => Some(ImageFormat::Ico),
        "pnm" | "pbm" | "pgm" | "ppm" => Some(ImageFormat::Pnm),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::single_line_excerpt;

    #[test]
    fn single_line_excerpt_removes_line_breaks_and_limits_characters() {
        assert_eq!(single_line_excerpt("one\ntwo\rthree", 9), "one two t");
    }
}
