use crate::model::BoardNode;
use std::collections::{HashMap, HashSet};

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
    let roots = tree.roots(nodes);

    let mut root_x = 0.0;
    for root in &roots {
        tree.measure(root);
    }
    for root in &roots {
        tree.place(root, root_x, 0.0);
        if !tree.manual.contains_key(&root.id) {
            root_x += tree.width(&root.id) + ROOT_GAP;
        }
    }

    let mut occupied: Vec<Rect> = nodes
        .iter()
        .filter(|node| tree.manual.contains_key(&node.id) && tree.positions.contains_key(&node.id))
        .map(|node| tree.rect(&node.id))
        .collect();
    for root in &roots {
        tree.separate(root, &mut occupied);
    }
    tree.positions
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
    children: HashMap<&'nodes str, Vec<&'nodes BoardNode>>,
    manual: HashMap<String, Position>,
    heights: &'nodes HashMap<String, f32>,
    widths: HashMap<String, f32>,
    positions: HashMap<String, Position>,
    visiting: HashSet<String>,
}

impl<'nodes> Tree<'nodes> {
    fn new(nodes: &'nodes [BoardNode], heights: &'nodes HashMap<String, f32>) -> Self {
        Self {
            children: HashMap::new(),
            manual: nodes
                .iter()
                .filter_map(|node| {
                    Some((
                        node.id.clone(),
                        Position {
                            x: node.x?,
                            y: node.y?,
                        },
                    ))
                })
                .collect(),
            heights,
            widths: HashMap::new(),
            positions: HashMap::new(),
            visiting: HashSet::new(),
        }
    }

    /// Indexes every node under its parent and returns the nodes that have none.
    fn roots(&mut self, nodes: &'nodes [BoardNode]) -> Vec<&'nodes BoardNode> {
        let ids: HashSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
        let mut sorted: Vec<&BoardNode> = nodes.iter().collect();
        sorted.sort_by_key(|node| (node.created_at, &node.id));

        let mut roots = Vec::new();
        for node in sorted {
            match node
                .parent_id
                .as_deref()
                .filter(|parent| ids.contains(parent))
            {
                Some(parent) => self.children.entry(parent).or_default().push(node),
                None => roots.push(node),
            }
        }
        roots
    }

    fn children_of(&self, node: &BoardNode) -> Vec<&'nodes BoardNode> {
        self.children
            .get(node.id.as_str())
            .cloned()
            .unwrap_or_default()
    }

    fn width(&self, id: &str) -> f32 {
        self.widths.get(id).copied().unwrap_or(CARD_WIDTH)
    }

    fn height(&self, id: &str) -> f32 {
        self.heights
            .get(id)
            .copied()
            .unwrap_or(ESTIMATED_CARD_HEIGHT)
    }

    fn rect(&self, id: &str) -> Rect {
        let position = self.positions.get(id).copied().unwrap_or_default();
        Rect {
            x: position.x,
            y: position.y,
            right: position.x + CARD_WIDTH,
            bottom: position.y + self.height(id),
        }
    }

    /// Records how wide each subtree is, ignoring manually placed children
    /// because they are not part of their parent's band.
    fn measure(&mut self, node: &'nodes BoardNode) -> f32 {
        if !self.visiting.insert(node.id.clone()) {
            return CARD_WIDTH;
        }
        let mut child_widths = Vec::new();
        for child in self.children_of(node) {
            let width = self.measure(child);
            if !self.manual.contains_key(&child.id) {
                child_widths.push(width);
            }
        }
        self.visiting.remove(&node.id);

        let width = if child_widths.is_empty() {
            CARD_WIDTH
        } else {
            child_widths.iter().sum::<f32>() + H_GAP * (child_widths.len() - 1) as f32
        }
        .max(CARD_WIDTH);
        self.widths.insert(node.id.clone(), width);
        width
    }

    /// Centres `node` in the band starting at `band_x`, then lays its children
    /// out as a row underneath it.
    fn place(&mut self, node: &'nodes BoardNode, band_x: f32, y: f32) {
        if !self.visiting.insert(node.id.clone()) {
            return;
        }
        let position = self.manual.get(&node.id).copied().unwrap_or(Position {
            x: band_x + self.width(&node.id) / 2.0 - CARD_WIDTH / 2.0,
            y,
        });
        self.positions.insert(node.id.clone(), position);

        let children = self.children_of(node);
        let automatic: Vec<_> = children
            .iter()
            .filter(|child| !self.manual.contains_key(&child.id))
            .collect();
        let row_width = automatic
            .iter()
            .map(|child| self.width(&child.id))
            .sum::<f32>()
            + H_GAP * automatic.len().saturating_sub(1) as f32;
        let row_y = position.y + self.height(&node.id) + V_GAP;
        let mut x = position.x + CARD_WIDTH / 2.0 - row_width / 2.0;
        for child in children {
            let is_manual = self.manual.contains_key(&child.id);
            self.place(child, x, row_y);
            if !is_manual {
                x += self.width(&child.id) + H_GAP;
            }
        }
        self.visiting.remove(&node.id);
    }

    /// Nudges automatically placed subtrees sideways until they no longer
    /// collide with anything already laid down. The first hit picks whichever
    /// direction moves the subtree less; later hits keep that direction so the
    /// subtree cannot oscillate between two neighbours forever.
    fn separate(&mut self, node: &'nodes BoardNode, occupied: &mut Vec<Rect>) {
        if !self.visiting.insert(node.id.clone()) {
            return;
        }
        if !self.manual.contains_key(&node.id) {
            let mut direction = 0.0f32;
            for _ in 0..MAX_OVERLAP_PASSES {
                let rect = self.rect(&node.id);
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
            occupied.push(self.rect(&node.id));
        }
        for child in self.children_of(node) {
            self.separate(child, occupied);
        }
        self.visiting.remove(&node.id);
    }

    fn shift_subtree(&mut self, node: &'nodes BoardNode, dx: f32) {
        let mut shifted = HashSet::new();
        self.shift(node, dx, &mut shifted);
    }

    fn shift(&mut self, node: &'nodes BoardNode, dx: f32, shifted: &mut HashSet<String>) {
        if self.manual.contains_key(&node.id) || !shifted.insert(node.id.clone()) {
            return;
        }
        if let Some(position) = self.positions.get_mut(&node.id) {
            position.x += dx;
        }
        for child in self.children_of(node) {
            self.shift(child, dx, shifted);
        }
    }
}
