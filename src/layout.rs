use crate::model::BoardNode;
use std::collections::{HashMap, HashSet};

pub const CARD_WIDTH: f32 = 340.0;
const H_GAP: f32 = 64.0;
const V_GAP: f32 = 96.0;
const ROOT_GAP: f32 = 140.0;
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

pub fn compute_layout(
    nodes: &[BoardNode],
    heights: &HashMap<String, f32>,
) -> HashMap<String, Position> {
    let manual: HashMap<String, Position> = nodes
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
        .collect();
    let ids: HashSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    let mut sorted: Vec<&BoardNode> = nodes.iter().collect();
    sorted.sort_by_key(|node| (node.created_at, &node.id));

    let mut children: HashMap<&str, Vec<&BoardNode>> = HashMap::new();
    let mut roots = Vec::new();
    for node in sorted {
        match node
            .parent_id
            .as_deref()
            .filter(|parent| ids.contains(parent))
        {
            Some(parent) => children.entry(parent).or_default().push(node),
            None => roots.push(node),
        }
    }

    fn measure(
        node: &BoardNode,
        children: &HashMap<&str, Vec<&BoardNode>>,
        manual: &HashMap<String, Position>,
        widths: &mut HashMap<String, f32>,
        visiting: &mut HashSet<String>,
    ) -> f32 {
        if !visiting.insert(node.id.clone()) {
            return CARD_WIDTH;
        }
        let child_widths: Vec<f32> = children
            .get(node.id.as_str())
            .into_iter()
            .flatten()
            .map(|child| measure(child, children, manual, widths, visiting))
            .zip(children.get(node.id.as_str()).into_iter().flatten())
            .filter_map(|(width, child)| (!manual.contains_key(&child.id)).then_some(width))
            .collect();
        visiting.remove(&node.id);
        let width = if child_widths.is_empty() {
            CARD_WIDTH
        } else {
            child_widths.iter().sum::<f32>() + H_GAP * (child_widths.len() - 1) as f32
        }
        .max(CARD_WIDTH);
        widths.insert(node.id.clone(), width);
        width
    }

    struct Placer<'a, 'nodes> {
        children: &'a HashMap<&'nodes str, Vec<&'nodes BoardNode>>,
        manual: &'a HashMap<String, Position>,
        heights: &'a HashMap<String, f32>,
        widths: &'a HashMap<String, f32>,
        positions: &'a mut HashMap<String, Position>,
        visiting: HashSet<String>,
    }

    impl<'nodes> Placer<'_, 'nodes> {
        fn place(&mut self, node: &'nodes BoardNode, band_x: f32, y: f32) {
            if !self.visiting.insert(node.id.clone()) {
                return;
            }
            let width = self.widths.get(&node.id).copied().unwrap_or(CARD_WIDTH);
            let position = self.manual.get(&node.id).copied().unwrap_or(Position {
                x: band_x + width / 2.0 - CARD_WIDTH / 2.0,
                y,
            });
            self.positions.insert(node.id.clone(), position);
            let kids = self
                .children
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let automatic: Vec<_> = kids
                .iter()
                .copied()
                .filter(|child| !self.manual.contains_key(&child.id))
                .collect();
            let row_width = automatic
                .iter()
                .map(|child| self.widths.get(&child.id).copied().unwrap_or(CARD_WIDTH))
                .sum::<f32>()
                + H_GAP * automatic.len().saturating_sub(1) as f32;
            let row_y = position.y
                + self
                    .heights
                    .get(&node.id)
                    .copied()
                    .unwrap_or(ESTIMATED_CARD_HEIGHT)
                + V_GAP;
            let mut x = position.x + CARD_WIDTH / 2.0 - row_width / 2.0;
            for child in kids {
                let is_manual = self.manual.contains_key(&child.id);
                self.place(child, x, row_y);
                if !is_manual {
                    x += self.widths.get(&child.id).copied().unwrap_or(CARD_WIDTH) + H_GAP;
                }
            }
            self.visiting.remove(&node.id);
        }
    }

    let mut widths = HashMap::new();
    for root in &roots {
        measure(root, &children, &manual, &mut widths, &mut HashSet::new());
    }
    let mut positions = HashMap::new();
    let mut root_x = 0.0;
    {
        let mut placer = Placer {
            children: &children,
            manual: &manual,
            heights,
            widths: &widths,
            positions: &mut positions,
            visiting: HashSet::new(),
        };
        for root in &roots {
            placer.place(root, root_x, 0.0);
            if !manual.contains_key(&root.id) {
                root_x += widths.get(&root.id).copied().unwrap_or(CARD_WIDTH) + ROOT_GAP;
            }
        }
    }

    fn node_rect(
        id: &str,
        positions: &HashMap<String, Position>,
        heights: &HashMap<String, f32>,
    ) -> Rect {
        let position = positions[id];
        Rect {
            x: position.x,
            y: position.y,
            right: position.x + CARD_WIDTH,
            bottom: position.y + heights.get(id).copied().unwrap_or(ESTIMATED_CARD_HEIGHT),
        }
    }
    fn overlaps(a: Rect, b: Rect) -> bool {
        const MARGIN: f32 = 24.0;
        a.x < b.right + MARGIN
            && a.right > b.x - MARGIN
            && a.y < b.bottom + MARGIN
            && a.bottom > b.y - MARGIN
    }
    fn shift_subtree(
        node: &BoardNode,
        dx: f32,
        children: &HashMap<&str, Vec<&BoardNode>>,
        manual: &HashMap<String, Position>,
        positions: &mut HashMap<String, Position>,
        visiting: &mut HashSet<String>,
    ) {
        if manual.contains_key(&node.id) || !visiting.insert(node.id.clone()) {
            return;
        }
        if let Some(position) = positions.get_mut(&node.id) {
            position.x += dx;
        }
        for child in children.get(node.id.as_str()).into_iter().flatten() {
            shift_subtree(child, dx, children, manual, positions, visiting);
        }
        visiting.remove(&node.id);
    }

    let mut occupied: Vec<Rect> = nodes
        .iter()
        .filter(|node| manual.contains_key(&node.id) && positions.contains_key(&node.id))
        .map(|node| node_rect(&node.id, &positions, heights))
        .collect();
    fn resolve(
        node: &BoardNode,
        children: &HashMap<&str, Vec<&BoardNode>>,
        manual: &HashMap<String, Position>,
        heights: &HashMap<String, f32>,
        positions: &mut HashMap<String, Position>,
        occupied: &mut Vec<Rect>,
        visiting: &mut HashSet<String>,
    ) {
        if !visiting.insert(node.id.clone()) {
            return;
        }
        if !manual.contains_key(&node.id) {
            for _ in 0..100 {
                let rect = node_rect(&node.id, positions, heights);
                let Some(hit) = occupied
                    .iter()
                    .copied()
                    .find(|candidate| overlaps(rect, *candidate))
                else {
                    break;
                };
                shift_subtree(
                    node,
                    hit.right + H_GAP - rect.x,
                    children,
                    manual,
                    positions,
                    &mut HashSet::new(),
                );
            }
            occupied.push(node_rect(&node.id, positions, heights));
        }
        for child in children.get(node.id.as_str()).into_iter().flatten() {
            resolve(
                child, children, manual, heights, positions, occupied, visiting,
            );
        }
        visiting.remove(&node.id);
    }
    for root in roots {
        resolve(
            root,
            &children,
            &manual,
            heights,
            &mut positions,
            &mut occupied,
            &mut HashSet::new(),
        );
    }
    positions
}
