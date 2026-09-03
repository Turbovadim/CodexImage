//! macOS window visibility bridge for releasing application-owned image
//! memory while a window is occluded.
//!
//! Renderer-owned GPU memory (the Metal layer, its drawable pool, path targets
//! and atlas textures) is handled by the vendored `gpui_macos`/`gpui_apple`
//! occlusion patch in `vendor/zed-gpui-occlusion`, not here. This observer only
//! covers the decoded-image and sprite caches, which GPUI cannot see. Never
//! message the view's backing layer from here: the patch swaps in a plain
//! `CALayer` while the window is occluded.

// The legacy `objc` macros inspect a historical `cargo-clippy` cfg that modern
// Cargo's check-cfg lint cannot discover from the dependency.
#![allow(unexpected_cfgs)]

use anyhow::{Result, anyhow, bail};
use async_channel::{Receiver, unbounded};
use block::ConcreteBlock;
use cocoa::base::{id, nil};
use cocoa::foundation::{NSString, NSUInteger};
use gpui::Window;
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const OCCLUSION_NOTIFICATION: &str = "NSWindowDidChangeOcclusionStateNotification";
const NS_WINDOW_OCCLUSION_STATE_VISIBLE: NSUInteger = 1 << 1;

/// The memory transition associated with a native visibility notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VisibilityChange {
    None,
    Release,
    Restore,
}

/// Deduplicates AppKit notifications so caches are cleared only once per
/// occlusion and rebuilt only after the window becomes visible again.
pub(super) struct WindowMemoryState {
    visible: bool,
}

impl Default for WindowMemoryState {
    fn default() -> Self {
        Self { visible: true }
    }
}

impl WindowMemoryState {
    pub(super) fn update(&mut self, visible: bool) -> VisibilityChange {
        if self.visible == visible {
            return VisibilityChange::None;
        }

        self.visible = visible;
        if visible {
            VisibilityChange::Restore
        } else {
            VisibilityChange::Release
        }
    }
}

/// Owns the block-based AppKit observer. Removing the token also drops the
/// channel sender captured by the block, which lets the GPUI task terminate.
pub(super) struct WindowOcclusionObserver {
    center: id,
    token: id,
}

impl WindowOcclusionObserver {
    pub(super) fn new(window: &Window) -> Result<(Self, Receiver<bool>)> {
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|error| anyhow!("GPUI did not expose its native window handle: {error}"))?
            .as_raw();
        let RawWindowHandle::AppKit(handle) = handle else {
            bail!("GPUI returned a non-AppKit window handle on macOS");
        };
        let view: id = handle.ns_view.as_ptr().cast();

        // SAFETY: GPUI's AppKit handle is its live GPUIView. The observer is
        // filtered to that view's NSWindow and is removed before this owner is
        // dropped. AppKit invokes the block on the posting (main) thread.
        unsafe {
            let native_window: id = msg_send![view, window];
            if native_window == nil {
                bail!("GPUI's native view is not attached to an NSWindow");
            }

            let center: id = msg_send![class!(NSNotificationCenter), defaultCenter];
            let name = NSString::alloc(nil).init_str(OCCLUSION_NOTIFICATION);
            let (sender, receiver) = unbounded();
            let block = ConcreteBlock::new(move |_: id| {
                let state: NSUInteger = msg_send![native_window, occlusionState];
                let visible = state & NS_WINDOW_OCCLUSION_STATE_VISIBLE != 0;
                let _ = sender.try_send(visible);
            })
            .copy();
            let token: id = msg_send![
                center,
                addObserverForName: name
                object: native_window
                queue: nil
                usingBlock: &*block
            ];
            let _: () = msg_send![name, release];

            if token == nil {
                bail!("AppKit rejected the window occlusion observer");
            }

            Ok((Self { center, token }, receiver))
        }
    }
}

impl Drop for WindowOcclusionObserver {
    fn drop(&mut self) {
        // SAFETY: `token` came from this notification center and is removed
        // exactly once while both objects are still alive.
        unsafe {
            let _: () = msg_send![self.center, removeObserver: self.token];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_changes_are_deduplicated() {
        let mut state = WindowMemoryState::default();

        assert_eq!(state.update(true), VisibilityChange::None);
        assert_eq!(state.update(false), VisibilityChange::Release);
        assert_eq!(state.update(false), VisibilityChange::None);
        assert_eq!(state.update(true), VisibilityChange::Restore);
        assert_eq!(state.update(true), VisibilityChange::None);
    }
}
