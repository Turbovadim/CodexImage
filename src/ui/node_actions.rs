//! Actions that operate on a single board node: branch, combine, edit,
//! regenerate, duplicate, and delete.

use super::app::AppView;
use super::app::Overlay;
use super::composer::ComposerTarget;
use super::input::TextInputMode;
use super::keymap::{
    BranchHovered, ChatHovered, CombineHovered, DeleteHovered, DuplicateHovered, EditHovered,
    OpenSettings, RegenerateHovered,
};
use crate::layout::{ESTIMATED_CARD_HEIGHT, free_spot_near};
use crate::model::NewNodesRequest;
use anyhow::Result;
use gpui::{Context, Focusable, Window};

impl AppView {
    /// Submission can wait on another import's mutex, so `action` runs off
    /// the UI thread. `on_success` runs back on it once the engine accepted
    /// the work; failures surface as a toast.
    pub(super) fn submit_node_action(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        action: impl FnOnce(crate::generation::GenerationEngine, String) -> Result<()> + Send + 'static,
        on_success: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if self.submission_pending {
            self.show_toast("A generation is already starting".into(), false, None, cx);
            return;
        }
        let board_id = match self.board_id() {
            Ok(id) => id.to_owned(),
            Err(error) => {
                self.show_error(error, cx);
                return;
            }
        };
        self.submission_pending = true;
        cx.notify();
        let engine = self.engine.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let result = smol::unblock(move || action(engine, board_id)).await;
            let _ = weak.update_in(cx, |view, window, cx| {
                view.submission_pending = false;
                match result {
                    Ok(()) => on_success(view, window, cx),
                    Err(error) => view.show_error(error, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Runs `action` against the open board, reporting any failure as a toast.
    pub(super) fn on_board(
        &mut self,
        cx: &mut Context<Self>,
        action: impl FnOnce(&mut Self, &str, &mut Context<Self>) -> Result<()>,
    ) {
        let board_id = match self.board_id() {
            Ok(id) => id.to_owned(),
            Err(error) => {
                self.show_error(error, cx);
                return;
            }
        };
        if let Err(error) = action(self, &board_id, cx) {
            self.show_error(error, cx);
        }
    }

    pub(super) fn branch_hovered(
        &mut self,
        _: &BranchHovered,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((id, image)) = self.hovered_or_lightbox_image() else {
            return;
        };
        self.branch_node(&id, image, window, cx);
    }

    pub(super) fn branch_node(
        &mut self,
        id: &str,
        source_image: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(node) = self.node(id) {
            self.targets = vec![ComposerTarget {
                node_id: node.id,
                prompt: node.prompt,
                source_image,
            }];
            self.overlay = Overlay::None;
            window.focus(&self.prompt.focus_handle(cx), cx);
            cx.notify();
        }
    }

    pub(super) fn combine_hovered(
        &mut self,
        _: &CombineHovered,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((id, image)) = self.hovered_or_lightbox_image() else {
            return;
        };
        self.combine_node(&id, image, window, cx);
    }

    /// Adds a card to the composer's targets so the next generation combines
    /// its image with the others. Every target needs a concrete image, so a
    /// card without one is skipped.
    pub(super) fn combine_node(
        &mut self,
        id: &str,
        source_image: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.node(id) else { return };
        let Some(image) = source_image.or_else(|| node.images.first().cloned()) else {
            self.show_toast("This card has no image to combine".into(), false, None, cx);
            return;
        };
        for target in &mut self.targets {
            if target.source_image.is_none()
                && let Some(node) = self
                    .board
                    .as_ref()
                    .and_then(|board| board.nodes.iter().find(|node| node.id == target.node_id))
            {
                target.source_image = node.images.first().cloned();
            }
        }
        self.targets.retain(|target| target.source_image.is_some());
        let already_added = self.targets.iter().any(|target| {
            target.node_id == node.id && target.source_image.as_deref() == Some(&image)
        });
        if !already_added {
            self.targets.push(ComposerTarget {
                node_id: node.id,
                prompt: node.prompt,
                source_image: Some(image),
            });
        }
        self.overlay = Overlay::None;
        window.focus(&self.prompt.focus_handle(cx), cx);
        cx.notify();
    }

    /// The card a keyboard action applies to: the lightbox image when one is
    /// open, otherwise the hovered card with no particular image.
    fn hovered_or_lightbox_image(&self) -> Option<(String, Option<String>)> {
        if let Overlay::Lightbox(lightbox) = &self.overlay {
            return Some((lightbox.node_id.clone(), Some(lightbox.image.clone())));
        }
        self.hovered_node.clone().map(|id| (id, None))
    }

    pub(super) fn regenerate_hovered(
        &mut self,
        _: &RegenerateHovered,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.hovered_node.clone() else {
            return;
        };
        self.regenerate_node(&id, window, cx);
    }

    pub(super) fn regenerate_node(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = id.to_owned();
        self.submit_node_action(
            window,
            cx,
            move |engine, board_id| engine.regenerate(&board_id, &id, None, None),
            |_, _, _| {},
        );
    }

    pub(super) fn edit_hovered(
        &mut self,
        _: &EditHovered,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.hovered_node.clone() else {
            return;
        };
        self.edit_node(&id, window, cx);
    }

    pub(super) fn edit_node(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node) = self.node(id) else { return };
        self.modal_input.update(cx, |input, cx| {
            input.set_mode(TextInputMode::AutoGrow { max_lines: 24 }, cx);
            input.set_placeholder("Edit prompt…", cx);
            input.set_content(node.prompt, cx);
        });
        self.overlay = Overlay::EditNode(id.to_owned());
        window.focus(&self.modal_input.focus_handle(cx), cx);
        cx.notify();
    }

    pub(super) fn duplicate_hovered(
        &mut self,
        _: &DuplicateHovered,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.hovered_node.clone() else {
            return;
        };
        self.duplicate_node(&id, window, cx);
    }

    pub(super) fn duplicate_node(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node) = self.node(id) else { return };
        // Pin the copy to the nearest free column beside the original; left to
        // the tree layout it would land at the far end of the sibling row.
        let position = self.current_position(id).map(|anchor| {
            let occupied = self
                .layout
                .iter()
                .map(|(id, position)| {
                    let height = self
                        .heights
                        .get(id)
                        .copied()
                        .unwrap_or(ESTIMATED_CARD_HEIGHT);
                    (*position, height)
                })
                .collect::<Vec<_>>();
            let spot = free_spot_near(anchor, self.card_height(&node), &occupied);
            (spot.x, spot.y)
        });
        let request = NewNodesRequest {
            prompt: node.prompt,
            parent_id: node.parent_id,
            merged_from: node.merged_from,
            source_images: Some(node.source_images),
            aspect: node.aspect,
            count: 1,
            attachment_paths: Vec::new(),
            attachment_urls: node.attachments,
            position,
        };
        self.submit_node_action(
            window,
            cx,
            move |engine, board_id| engine.add_and_start(&board_id, request).map(|_| ()),
            |_, _, _| {},
        );
    }

    pub(super) fn chat_hovered(
        &mut self,
        _: &ChatHovered,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.hovered_node.clone() else {
            return;
        };
        self.open_chat(&id, window, cx);
    }

    pub(super) fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings(window, cx);
    }

    pub(super) fn delete_hovered(
        &mut self,
        _: &DeleteHovered,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.hovered_node.clone() else {
            return;
        };
        self.delete_node(&id, cx);
    }

    pub(super) fn delete_node(&mut self, id: &str, cx: &mut Context<Self>) {
        let board_id = match self.board_id() {
            Ok(id) => id.to_owned(),
            Err(error) => {
                self.show_error(error, cx);
                return;
            }
        };
        match self.engine.delete_subtree(&board_id, id) {
            Ok((deleted, undo_id)) => {
                let text = if deleted.len() == 1 {
                    "Node deleted".into()
                } else {
                    format!("{} nodes deleted", deleted.len())
                };
                self.show_toast(text, false, Some((board_id, undo_id)), cx);
            }
            Err(error) => self.show_error(error, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        generation::GenerationEngine,
        storage::{DataPaths, Repository},
    };
    use gpui::TestAppContext;
    use std::time::Duration;

    #[gpui::test]
    async fn pending_submission_leaves_ui_available_and_reports_failure(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(
            directory.path().to_owned(),
            directory.path().join("generated"),
        );
        let (events, _events) = async_channel::unbounded();
        let repository = Repository::open_at(paths, events).unwrap();
        repository.create_board().unwrap();
        let engine = GenerationEngine::new(repository);
        let (_events, receiver) = async_channel::unbounded();
        let handle = cx.add_window(move |window, cx| AppView::new(engine, receiver, window, cx));
        let (release, blocked) = std::sync::mpsc::channel();
        handle
            .update(cx, |view, window, cx| {
                view.submit_node_action(
                    window,
                    cx,
                    move |_, _| {
                        blocked
                            .recv_timeout(Duration::from_secs(2))
                            .expect("UI can release waiting work");
                        anyhow::bail!("Submission failed");
                    },
                    |_, _, _| panic!("failed submission reported success"),
                );
                assert!(view.submission_pending);
                view.submit_node_action(
                    window,
                    cx,
                    |_, _| panic!("duplicate submission was accepted"),
                    |_, _, _| {},
                );
                release.send(()).unwrap();
            })
            .unwrap();
        cx.condition(&handle.entity(cx).unwrap(), |view, _| {
            !view.submission_pending
        })
        .await;
        handle
            .update(cx, |view, _, _| {
                assert_eq!(
                    view.toast.as_ref().unwrap().text.as_ref(),
                    "Submission failed"
                );
            })
            .unwrap();
    }
}
