//! How much room a card needs: prompt wrapping, the arrangement of its
//! outputs, and the resulting card height.

use super::card::{
    ATTACHMENT_ROW_HEIGHT, COLLAPSED_PROMPT_LINES, EXPANDED_PROMPT_LINES, MEDIA_GAP,
    PROMPT_LINE_HEIGHT, PROMPT_WRAP_COLUMNS, SHOW_MORE_HEIGHT,
};
use crate::layout::CARD_WIDTH;
use crate::model::{BoardNode, NodeStatus};
use std::collections::HashMap;
use unicode_segmentation::UnicodeSegmentation;

const HEADER_FIXED_HEIGHT: f32 = 50.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputCell {
    pub index: usize,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OutputLayout {
    None,
    Tiles {
        height: f32,
        cells: Vec<OutputCell>,
    },
    Filmstrip {
        height: f32,
        hero_height: f32,
        compact_count: usize,
        hidden_count: usize,
        strip_cell_width: f32,
    },
}

impl OutputLayout {
    pub fn height(&self) -> f32 {
        match self {
            Self::None => 0.,
            Self::Tiles { height, .. } | Self::Filmstrip { height, .. } => *height,
        }
    }
}

pub fn card_height(node: &BoardNode, expanded: bool, ratios: &HashMap<String, f32>) -> f32 {
    card_height_from_metadata(
        node,
        expanded,
        wrap_prompt(&node.prompt, PROMPT_WRAP_COLUMNS).len(),
        output_layout(node, ratios).height(),
    )
}

pub fn card_height_from_metadata(
    node: &BoardNode,
    expanded: bool,
    total_prompt_lines: usize,
    output_height: f32,
) -> f32 {
    let (prompt_lines, prompt_clamped) =
        prompt_metrics_from_line_count(total_prompt_lines, expanded);
    HEADER_FIXED_HEIGHT
        + prompt_lines * PROMPT_LINE_HEIGHT
        + if prompt_clamped { SHOW_MORE_HEIGHT } else { 0. }
        + if node.attachments.is_empty() {
            0.
        } else {
            ATTACHMENT_ROW_HEIGHT
        }
        + MEDIA_GAP
        + output_height
        + status_area_height(node)
}

fn prompt_metrics_from_line_count(total_lines: usize, expanded: bool) -> (f32, bool) {
    let line_limit = if expanded {
        EXPANDED_PROMPT_LINES
    } else {
        COLLAPSED_PROMPT_LINES
    };
    (
        total_lines.min(line_limit).max(1) as f32,
        total_lines > COLLAPSED_PROMPT_LINES,
    )
}

pub fn wrap_prompt(value: &str, max_graphemes: usize) -> Vec<String> {
    assert!(max_graphemes > 0, "prompt wrap width must be non-zero");
    let mut lines = Vec::new();

    for paragraph in value.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut current_len = 0;
        for word in paragraph.split_whitespace() {
            let grapheme_count = word.graphemes(true).count();
            if grapheme_count <= max_graphemes {
                let separator_len = usize::from(!current.is_empty());
                if current_len + separator_len + grapheme_count <= max_graphemes {
                    if separator_len == 1 {
                        current.push(' ');
                    }
                    current.push_str(word);
                    current_len += separator_len + grapheme_count;
                } else {
                    lines.push(std::mem::take(&mut current));
                    current.push_str(word);
                    current_len = grapheme_count;
                }
                continue;
            }

            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
            for grapheme in word.graphemes(true) {
                current.push_str(grapheme);
                current_len += 1;
                if current_len == max_graphemes {
                    lines.push(std::mem::take(&mut current));
                    current_len = 0;
                }
            }
        }

        if !current.is_empty() {
            lines.push(current);
        } else if paragraph.chars().all(char::is_whitespace) {
            lines.push(String::new());
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub fn displayed_urls(node: &BoardNode) -> &[String] {
    if node.images.is_empty() {
        node.attempts
            .last()
            .map(std::slice::from_ref)
            .unwrap_or_default()
    } else {
        &node.images
    }
}

pub fn output_layout(node: &BoardNode, ratios: &HashMap<String, f32>) -> OutputLayout {
    let urls = displayed_urls(node);
    match urls.len() {
        0 if node.status == NodeStatus::Running => OutputLayout::Tiles {
            height: CARD_WIDTH,
            cells: Vec::new(),
        },
        0 => OutputLayout::None,
        1 => {
            let height = image_height(CARD_WIDTH, output_ratio(node, &urls[0], ratios));
            OutputLayout::Tiles {
                height,
                cells: vec![OutputCell {
                    index: 0,
                    x: 0.,
                    y: 0.,
                    width: CARD_WIDTH,
                    height,
                }],
            }
        }
        2..=4 => {
            let cell_width = (CARD_WIDTH - MEDIA_GAP) / 2.;
            let mut cells = Vec::with_capacity(urls.len());
            let mut y = 0.;
            for (row, chunk) in urls.chunks(2).enumerate() {
                let row_height = chunk
                    .iter()
                    .map(|url| image_height(cell_width, output_ratio(node, url, ratios)))
                    .fold(0_f32, f32::max);
                for (column, _) in chunk.iter().enumerate() {
                    cells.push(OutputCell {
                        index: row * 2 + column,
                        x: column as f32 * (cell_width + MEDIA_GAP),
                        y,
                        width: cell_width,
                        height: row_height,
                    });
                }
                y += row_height;
                if row * 2 + chunk.len() < urls.len() {
                    y += MEDIA_GAP;
                }
            }
            OutputLayout::Tiles { height: y, cells }
        }
        _ => {
            let hero_height = image_height(CARD_WIDTH, output_ratio(node, &urls[0], ratios));
            let compact_count = if urls.len() > 6 {
                4
            } else {
                (urls.len() - 1).min(5)
            };
            let hidden_count = urls.len() - compact_count - 1;
            let strip_cells = compact_count + usize::from(hidden_count > 0);
            let strip_cell_width = (CARD_WIDTH - MEDIA_GAP * strip_cells.saturating_sub(1) as f32)
                / strip_cells as f32;
            OutputLayout::Filmstrip {
                height: hero_height + MEDIA_GAP + strip_cell_width,
                hero_height,
                compact_count,
                hidden_count,
                strip_cell_width,
            }
        }
    }
}

pub const ATTACHED_TEXT_HEIGHT: f32 = 20.;

/// The extra status-area room an error or stopped card needs to show the
/// text the agent attached.
pub fn attached_text_height(node: &BoardNode) -> f32 {
    if node.attached_text().is_some() {
        ATTACHED_TEXT_HEIGHT
    } else {
        0.
    }
}

pub fn status_area_height(node: &BoardNode) -> f32 {
    let has_images = !displayed_urls(node).is_empty();
    let base = match node.status {
        NodeStatus::Running | NodeStatus::Done => 42.,
        NodeStatus::Error if has_images => 64.,
        NodeStatus::Error => 132.,
        NodeStatus::Stopped => 52.,
    };
    base + attached_text_height(node)
}

fn output_ratio(node: &BoardNode, url: &str, ratios: &HashMap<String, f32>) -> f32 {
    ratios
        .get(url)
        .copied()
        .or_else(|| parse_aspect_ratio(&node.aspect))
        .unwrap_or(1.)
        .clamp(0.2, 5.)
}

fn parse_aspect_ratio(aspect: &str) -> Option<f32> {
    let (width, height) = aspect.split_once(':')?;
    let (Ok(width), Ok(height)) = (width.parse::<f32>(), height.parse::<f32>()) else {
        return None;
    };
    (width.is_finite() && height.is_finite() && width > 0. && height > 0.).then_some(width / height)
}

fn image_height(width: f32, ratio: f32) -> f32 {
    (width / ratio).clamp(width * 0.28, width * 2.)
}

#[cfg(test)]
mod tests {
    use super::{
        OutputLayout, PROMPT_WRAP_COLUMNS, card_height, output_layout,
        prompt_metrics_from_line_count, status_area_height, wrap_prompt,
    };
    use crate::layout::CARD_WIDTH;
    use crate::model::{BoardNode, NodeStatus};
    use std::collections::HashMap;
    use unicode_segmentation::UnicodeSegmentation;

    fn node(images: usize, status: NodeStatus) -> BoardNode {
        BoardNode {
            id: "node".into(),
            parent_id: None,
            prompt: "A concise prompt that should preserve its line shape while zooming".into(),
            aspect: "auto".into(),
            source_images: Vec::new(),
            attachments: Vec::new(),
            images: (0..images).map(|index| format!("/{index}.png")).collect(),
            image_labels: Vec::new(),
            attempts: Vec::new(),
            text: String::new(),
            status,
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
    fn four_outputs_form_two_non_overlapping_columns() {
        let node = node(4, NodeStatus::Done);
        let ratios = HashMap::from([
            ("/0.png".into(), 1.0),
            ("/1.png".into(), 0.5),
            ("/2.png".into(), 2.0),
            ("/3.png".into(), 1.0),
        ]);
        let OutputLayout::Tiles { height, cells } = output_layout(&node, &ratios) else {
            panic!("expected tiled layout")
        };
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0].x, 0.);
        assert!(cells[1].x > cells[0].x + cells[0].width);
        assert!(cells[2].y > cells[0].y);
        assert!(height >= cells[3].y + cells[3].height);
    }

    #[test]
    fn large_output_sets_use_a_bounded_hero_and_filmstrip() {
        let node = node(9, NodeStatus::Done);
        let OutputLayout::Filmstrip {
            hero_height,
            compact_count,
            hidden_count,
            strip_cell_width,
            ..
        } = output_layout(&node, &HashMap::new())
        else {
            panic!("expected filmstrip layout")
        };
        assert_eq!(compact_count, 4);
        assert_eq!(hidden_count, 4);
        assert!(hero_height <= CARD_WIDTH * 2.);
        assert!(strip_cell_width < CARD_WIDTH / 4.);
    }

    #[test]
    fn empty_error_state_reserves_more_space_than_a_done_footer() {
        let error = node(0, NodeStatus::Error);
        let done = node(0, NodeStatus::Done);
        assert!(
            card_height(&error, false, &HashMap::new())
                > card_height(&done, false, &HashMap::new())
        );
    }

    #[test]
    fn attached_text_grows_the_error_status_area() {
        let plain = node(0, NodeStatus::Error);
        let mut with_text = node(0, NodeStatus::Error);
        with_text.text = "The agent explained what went wrong".into();
        assert!(status_area_height(&with_text) > status_area_height(&plain));

        // A summary reused as the error message is already visible.
        with_text.error = Some(with_text.text.clone());
        assert_eq!(status_area_height(&with_text), status_area_height(&plain));
    }

    #[test]
    fn prompt_wrap_is_deterministic_and_grapheme_safe() {
        let prompt = "alpha beta supercalifragilistic 🌍🌎🌏🌐";
        let lines = wrap_prompt(prompt, 8);
        assert!(lines.iter().all(|line| line.graphemes(true).count() <= 8));
        let source = prompt
            .chars()
            .filter(|character| !character.is_whitespace());
        let wrapped = lines
            .iter()
            .flat_map(|line| line.chars())
            .filter(|character| !character.is_whitespace());
        assert_eq!(source.collect::<String>(), wrapped.collect::<String>());
    }

    #[test]
    fn prompt_wrap_preserves_explicit_blank_lines() {
        assert_eq!(
            wrap_prompt("one two\n\nthree", 6),
            vec!["one", "two", "", "three"]
        );
    }

    #[test]
    fn expanded_prompt_height_uses_the_same_world_space_lines() {
        let mut node = node(0, NodeStatus::Done);
        node.prompt = std::iter::repeat_n("stable", 80)
            .collect::<Vec<_>>()
            .join(" ");
        let total_lines = wrap_prompt(&node.prompt, PROMPT_WRAP_COLUMNS).len();
        let (collapsed_lines, clamped) = prompt_metrics_from_line_count(total_lines, false);
        let (expanded_lines, expanded_clamped) = prompt_metrics_from_line_count(total_lines, true);
        assert_eq!(collapsed_lines, 6.);
        assert!(expanded_lines > collapsed_lines);
        assert!(clamped && expanded_clamped);
    }
}
