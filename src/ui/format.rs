//! Human-readable text for boards, nodes, and timestamps.

use crate::model::{Board, BoardNode, NodeStatus, StopReason};
use crate::storage::now_ms;
use gpui::ImageFormat;
use std::collections::HashMap;
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

/// A one-line excerpt of the text the agent attached to an error or stopped
/// card; empty when the card already shows that text elsewhere.
pub fn attached_text_excerpt(node: &BoardNode) -> String {
    node.attached_text()
        .map(|text| single_line_excerpt(text, 90))
        .unwrap_or_default()
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
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index))
        .collect();
    let parents: Vec<_> = board
        .nodes
        .iter()
        .map(|node| {
            node.parent_id
                .as_deref()
                .and_then(|parent| by_id.get(parent))
                .copied()
        })
        .collect();
    let mut depths = vec![None; board.nodes.len()];
    let mut visit_generation = vec![usize::MAX; board.nodes.len()];
    let mut visit_position = vec![0; board.nodes.len()];
    let mut path = Vec::new();

    for start in 0..board.nodes.len() {
        if depths[start].is_some() {
            continue;
        }
        path.clear();
        let mut current = start;
        loop {
            if let Some(mut depth) = depths[current] {
                while let Some(node) = path.pop() {
                    depth += 1;
                    depths[node] = Some(depth);
                }
                break;
            }

            if visit_generation[current] == start {
                let cycle_start = visit_position[current];
                let cycle_depth = path.len() - cycle_start;
                for &node in &path[cycle_start..] {
                    depths[node] = Some(cycle_depth);
                }
                let mut depth = cycle_depth;
                for &node in path[..cycle_start].iter().rev() {
                    depth += 1;
                    depths[node] = Some(depth);
                }
                break;
            }

            visit_generation[current] = start;
            visit_position[current] = path.len();
            path.push(current);
            let Some(parent) = parents[current] else {
                let node = path.pop().expect("current node was just pushed");
                depths[node] = Some(0);
                let mut depth = 0;
                while let Some(node) = path.pop() {
                    depth += 1;
                    depths[node] = Some(depth);
                }
                break;
            };
            current = parent;
        }
    }

    board
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.clone(), depths[index].unwrap_or(0)))
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
    use super::{node_depths, single_line_excerpt};
    use crate::model::{Board, BoardNode, NodeStatus};

    fn node(id: impl Into<String>, parent_id: Option<String>) -> BoardNode {
        BoardNode {
            id: id.into(),
            parent_id,
            prompt: String::new(),
            aspect: "auto".into(),
            source_images: Vec::new(),
            attachments: Vec::new(),
            images: Vec::new(),
            image_labels: Vec::new(),
            attempts: Vec::new(),
            text: String::new(),
            status: NodeStatus::Done,
            error: None,
            stop_reason: None,
            x: None,
            y: None,
            created_at: 0,
            run_started_at: None,
            finished_at: None,
            usage: None,
        }
    }

    #[test]
    fn single_line_excerpt_removes_line_breaks_and_limits_characters() {
        assert_eq!(single_line_excerpt("one\ntwo\rthree", 9), "one two t");
    }

    #[test]
    fn node_depths_memoize_long_chains_and_bound_cycles() {
        let mut nodes = Vec::with_capacity(4_099);
        for index in 0..4_096 {
            nodes.push(node(
                format!("chain-{index}"),
                (index > 0).then(|| format!("chain-{}", index - 1)),
            ));
        }
        nodes.push(node("cycle-a", Some("cycle-b".into())));
        nodes.push(node("cycle-b", Some("cycle-a".into())));
        nodes.push(node("cycle-child", Some("cycle-a".into())));
        let board = Board {
            id: "board".into(),
            title: String::new(),
            created_at: 0,
            nodes,
        };

        let depths = node_depths(&board);

        assert_eq!(depths["chain-4095"], 4_095);
        assert_eq!(depths["cycle-a"], 2);
        assert_eq!(depths["cycle-b"], 2);
        assert_eq!(depths["cycle-child"], 3);
    }
}
