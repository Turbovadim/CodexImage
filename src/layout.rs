use crate::model::BoardNode;
use std::collections::HashMap;

pub const CARD_WIDTH: f32 = 340.0;
const H_GAP: f32 = 64.0;
const V_GAP: f32 = 96.0;
const ROOT_GAP: f32 = 140.0;
const OVERLAP_MARGIN: f32 = 24.0;
const MAX_OVERLAP_PASSES: usize = 100;
pub const ESTIMATED_CARD_HEIGHT: f32 = 460.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    right: f32,
    bottom: f32,
}

impl Rect {
    fn overlaps(self, other: Self) -> bool {
        self.x < other.right + OVERLAP_MARGIN
            && self.right > other.x - OVERLAP_MARGIN
            && self.y < other.bottom + OVERLAP_MARGIN
            && self.bottom > other.y - OVERLAP_MARGIN
    }
}

/// Lays the board out as a top-down tree: every node sits below its parent,
/// centred over the band its subtree occupies. Nodes the user dragged keep
/// their stored position and automatic siblings are pushed aside to avoid them.
pub fn compute_layout(
    nodes: &[BoardNode],
    heights: &HashMap<String, f32>,
) -> HashMap<String, Position> {
    let mut tree = Tree::new(nodes, heights);

    let mut root_x = 0.0;
    for root_index in 0..tree.roots.len() {
        let root = tree.roots[root_index];
        tree.measure(root);
    }
    for root_index in 0..tree.roots.len() {
        let root = tree.roots[root_index];
        tree.place(root, root_x, 0.0);
        if tree.manual[root].is_none() {
            root_x += tree.width(root) + ROOT_GAP;
        }
    }

    let mut occupied: Vec<Rect> = (0..nodes.len())
        .filter(|&node| tree.manual[node].is_some() && tree.positions[node].is_some())
        .map(|node| tree.rect(node))
        .collect();
    for root_index in 0..tree.roots.len() {
        let root = tree.roots[root_index];
        tree.separate(root, &mut occupied);
    }
    tree.into_positions()
}

/// Finds the nearest free column beside `anchor` for a card of `height`,
/// stepping outwards one card slot at a time and preferring the side that
/// needs the smaller move. `occupied` holds the position and height of every
/// card already on the canvas.
pub fn free_spot_near(anchor: Position, height: f32, occupied: &[(Position, f32)]) -> Position {
    let rect = |position: Position, height: f32| Rect {
        x: position.x,
        y: position.y,
        right: position.x + CARD_WIDTH,
        bottom: position.y + height,
    };
    let free = |candidate: Position| {
        let candidate = rect(candidate, height);
        !occupied
            .iter()
            .any(|(position, height)| candidate.overlaps(rect(*position, *height)))
    };
    for step in 1..=64 {
        let dx = (CARD_WIDTH + H_GAP) * step as f32;
        for x in [anchor.x + dx, anchor.x - dx] {
            let candidate = Position { x, y: anchor.y };
            if free(candidate) {
                return candidate;
            }
        }
    }
    Position {
        x: anchor.x + CARD_WIDTH + H_GAP,
        y: anchor.y + height + V_GAP,
    }
}

struct Tree<'nodes> {
    nodes: &'nodes [BoardNode],
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
    manual: Vec<Option<Position>>,
    heights: Vec<f32>,
    widths: Vec<f32>,
    positions: Vec<Option<Position>>,
    visiting: Vec<bool>,
    shift_marks: Vec<u32>,
    shift_generation: u32,
}

impl<'nodes> Tree<'nodes> {
    fn new(nodes: &'nodes [BoardNode], heights: &'nodes HashMap<String, f32>) -> Self {
        let node_count = nodes.len();
        let ids: HashMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.as_str(), index))
            .collect();
        let mut sorted: Vec<usize> = (0..node_count).collect();
        sorted.sort_by_key(|&index| (nodes[index].created_at, &nodes[index].id));

        let mut children = vec![Vec::new(); node_count];
        let mut roots = Vec::new();
        for node in sorted {
            if let Some(parent) = nodes[node]
                .parent_id
                .as_deref()
                .and_then(|parent| ids.get(parent))
                .copied()
            {
                children[parent].push(node);
            } else {
                roots.push(node);
            }
        }

        Self {
            nodes,
            children,
            roots,
            manual: nodes
                .iter()
                .map(|node| {
                    Some(Position {
                        x: node.x?,
                        y: node.y?,
                    })
                })
                .collect(),
            heights: nodes
                .iter()
                .map(|node| {
                    heights
                        .get(node.id.as_str())
                        .copied()
                        .unwrap_or(ESTIMATED_CARD_HEIGHT)
                })
                .collect(),
            widths: vec![CARD_WIDTH; node_count],
            positions: vec![None; node_count],
            visiting: vec![false; node_count],
            shift_marks: vec![0; node_count],
            shift_generation: 0,
        }
    }

    fn width(&self, node: usize) -> f32 {
        self.widths[node]
    }

    fn height(&self, node: usize) -> f32 {
        self.heights[node]
    }

    fn rect(&self, node: usize) -> Rect {
        let position = self.positions[node].unwrap_or_default();
        Rect {
            x: position.x,
            y: position.y,
            right: position.x + CARD_WIDTH,
            bottom: position.y + self.height(node),
        }
    }

    fn into_positions(self) -> HashMap<String, Position> {
        let mut result = HashMap::with_capacity(self.positions.len());
        for (index, position) in self.positions.into_iter().enumerate() {
            if let Some(position) = position {
                result.insert(self.nodes[index].id.clone(), position);
            }
        }
        result
    }

    /// Records how wide each subtree is, ignoring manually placed children
    /// because they are not part of their parent's band.
    fn measure(&mut self, node: usize) -> f32 {
        if self.visiting[node] {
            return CARD_WIDTH;
        }
        self.visiting[node] = true;
        let mut child_width = 0.;
        let mut automatic_children = 0;
        for child_index in 0..self.children[node].len() {
            let child = self.children[node][child_index];
            let width = self.measure(child);
            if self.manual[child].is_none() {
                child_width += width;
                automatic_children += 1;
            }
        }
        self.visiting[node] = false;

        let width = if automatic_children == 0 {
            CARD_WIDTH
        } else {
            child_width + H_GAP * (automatic_children - 1) as f32
        }
        .max(CARD_WIDTH);
        self.widths[node] = width;
        width
    }

    /// Centres `node` in the band starting at `band_x`, then lays its children
    /// out as a row underneath it.
    fn place(&mut self, node: usize, band_x: f32, y: f32) {
        if self.visiting[node] {
            return;
        }
        self.visiting[node] = true;
        let position = self.manual[node].unwrap_or(Position {
            x: band_x + self.width(node) / 2.0 - CARD_WIDTH / 2.0,
            y,
        });
        self.positions[node] = Some(position);

        let mut row_width = 0.;
        let mut automatic_children = 0usize;
        for &child in &self.children[node] {
            if self.manual[child].is_none() {
                row_width += self.width(child);
                automatic_children += 1;
            }
        }
        row_width += H_GAP * automatic_children.saturating_sub(1) as f32;
        let row_y = position.y + self.height(node) + V_GAP;
        let mut x = position.x + CARD_WIDTH / 2.0 - row_width / 2.0;
        for child_index in 0..self.children[node].len() {
            let child = self.children[node][child_index];
            let is_manual = self.manual[child].is_some();
            self.place(child, x, row_y);
            if !is_manual {
                x += self.width(child) + H_GAP;
            }
        }
        self.visiting[node] = false;
    }

    /// Nudges automatically placed subtrees sideways until they no longer
    /// collide with anything already laid down. The first hit picks whichever
    /// direction moves the subtree less; later hits keep that direction so the
    /// subtree cannot oscillate between two neighbours forever.
    fn separate(&mut self, node: usize, occupied: &mut Vec<Rect>) {
        if self.visiting[node] {
            return;
        }
        self.visiting[node] = true;
        if self.manual[node].is_none() {
            let mut direction = 0.0f32;
            for _ in 0..MAX_OVERLAP_PASSES {
                let rect = self.rect(node);
                let Some(hit) = occupied
                    .iter()
                    .copied()
                    .find(|candidate| rect.overlaps(*candidate))
                else {
                    break;
                };
                let rightwards = hit.right + H_GAP - rect.x;
                let leftwards = hit.x - H_GAP - rect.right;
                let dx = if direction > 0. {
                    rightwards
                } else if direction < 0. {
                    leftwards
                } else if rightwards <= -leftwards {
                    rightwards
                } else {
                    leftwards
                };
                direction = dx.signum();
                self.shift_subtree(node, dx);
            }
            occupied.push(self.rect(node));
        }
        for child_index in 0..self.children[node].len() {
            let child = self.children[node][child_index];
            self.separate(child, occupied);
        }
        self.visiting[node] = false;
    }

    fn shift_subtree(&mut self, node: usize, dx: f32) {
        self.shift_generation = self.shift_generation.wrapping_add(1);
        if self.shift_generation == 0 {
            self.shift_marks.fill(0);
            self.shift_generation = 1;
        }
        self.shift(node, dx, self.shift_generation);
    }

    fn shift(&mut self, node: usize, dx: f32, generation: u32) {
        if self.manual[node].is_some() || self.shift_marks[node] == generation {
            return;
        }
        self.shift_marks[node] = generation;
        if let Some(position) = &mut self.positions[node] {
            position.x += dx;
        }
        for child_index in 0..self.children[node].len() {
            let child = self.children[node][child_index];
            self.shift(child, dx, generation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Position, V_GAP, compute_layout};
    use crate::model::{BoardNode, NodeStatus};
    use std::collections::HashMap;

    fn node(index: usize, parent: Option<usize>) -> BoardNode {
        BoardNode {
            id: format!("node-{index:04}"),
            parent_id: parent.map(|parent| format!("node-{parent:04}")),
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
            created_at: index as i64,
            run_started_at: None,
            finished_at: None,
            usage: None,
        }
    }

    #[test]
    fn large_indexed_tree_layout_is_complete_and_input_order_independent() {
        const NODE_COUNT: usize = 511;
        let nodes: Vec<_> = (0..NODE_COUNT)
            .map(|index| node(index, (index > 0).then(|| (index - 1) / 2)))
            .collect();
        let heights: HashMap<_, _> = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.clone(), 180. + (index % 7) as f32 * 11.))
            .collect();

        let expected = compute_layout(&nodes, &heights);
        let mut reversed = nodes.clone();
        reversed.reverse();
        assert_eq!(compute_layout(&reversed, &heights), expected);
        assert_eq!(expected.len(), NODE_COUNT);

        for index in 1..NODE_COUNT {
            let parent = (index - 1) / 2;
            let position = expected[&nodes[index].id];
            let parent_position = expected[&nodes[parent].id];
            assert_eq!(
                position.y,
                parent_position.y + heights[&nodes[parent].id] + V_GAP
            );
            assert!(position.x.is_finite());
        }
    }

    #[test]
    fn manual_node_keeps_its_position() {
        let mut nodes = vec![node(0, None), node(1, Some(0)), node(2, Some(0))];
        nodes[1].x = Some(-725.);
        nodes[1].y = Some(315.);

        let positions = compute_layout(&nodes, &HashMap::new());

        assert_eq!(positions[&nodes[1].id], Position { x: -725., y: 315. });
    }
}
