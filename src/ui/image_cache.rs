//! A weighted image cache for every file-backed image in the application.
//!
//! GPUI's default asset cache has no size limit. That is unsafe for an image
//! browser because each compressed file becomes a full BGRA buffer after it is
//! decoded. This cache owns the GPUI asset entries and releases both their CPU
//! buffers and GPU atlas entries when the decoded-byte budget is exceeded.

use gpui::{
    App, Context, Image, ImageCache, ImageCacheError, ImageCacheItem, ImgResourceLoader,
    RenderImage, Resource, Task, WeakEntity, Window,
};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;

// Each decoded buffer normally also has a BGRA copy in the Metal atlas. Keep
// the CPU-side limits low enough that the combined unified-memory footprint
// remains well below one gigabyte.
pub const DECODED_IMAGE_CACHE_BUDGET: usize = 192 * 1024 * 1024;
const DECODED_IMAGE_CACHE_MAX_ITEMS: usize = 1_024;
pub const CARD_SPRITE_CACHE_BUDGET: usize = 32 * 1024 * 1024;
const CARD_SPRITE_CACHE_MAX_ITEMS: usize = 1_024;

struct CacheEntry {
    image: ImageCacheItem,
    decoded_bytes: usize,
    last_used: u64,
    _notification: Task<()>,
}

struct EvictionCycle<K> {
    used: HashSet<K>,
    last_visible: HashSet<K>,
    generation: u64,
    scheduled: bool,
}

impl<K> Default for EvictionCycle<K> {
    fn default() -> Self {
        Self {
            used: HashSet::new(),
            last_visible: HashSet::new(),
            generation: 0,
            scheduled: false,
        }
    }
}

impl<K: Clone + Eq + Hash> EvictionCycle<K> {
    fn record(&mut self, key: K) {
        self.used.insert(key);
    }

    fn should_start(&self, over_limit: bool, key: &K, capacity_changed: bool) -> bool {
        over_limit && !self.scheduled && (capacity_changed || !self.last_visible.contains(key))
    }

    fn start(&mut self) -> u64 {
        self.scheduled = true;
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    fn begin_collection(&mut self, generation: u64) -> bool {
        if !self.scheduled || self.generation != generation {
            return false;
        }
        self.used.clear();
        true
    }

    fn finish_collection(&mut self, generation: u64) -> Option<HashSet<K>> {
        if !self.scheduled || self.generation != generation {
            return None;
        }
        let visible = std::mem::take(&mut self.used);
        self.last_visible = visible.clone();
        self.scheduled = false;
        Some(visible)
    }

    fn cancel(&mut self) {
        self.used.clear();
        self.last_visible.clear();
        self.scheduled = false;
        self.generation = self.generation.wrapping_add(1);
    }
}

pub(super) struct DecodedImageCache {
    entries: HashMap<Resource, CacheEntry>,
    /// Tiny pre-blurred copies of decoded images, shown for in-progress
    /// generations. Each one lives and dies with its base entry.
    blurred: HashMap<Resource, Arc<RenderImage>>,
    decoded_bytes: usize,
    byte_budget: usize,
    max_items: usize,
    clock: u64,
    eviction: EvictionCycle<Resource>,
    weak_self: WeakEntity<Self>,
}

impl DecodedImageCache {
    pub fn new(byte_budget: usize, cx: &mut Context<Self>) -> Self {
        cx.on_release(|cache, cx| {
            cache.release_all(None, cx);
        })
        .detach();

        Self {
            entries: HashMap::new(),
            blurred: HashMap::new(),
            decoded_bytes: 0,
            byte_budget,
            max_items: DECODED_IMAGE_CACHE_MAX_ITEMS,
            clock: 0,
            eviction: EvictionCycle::default(),
            weak_self: cx.weak_entity(),
        }
    }

    pub fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.eviction.record(resource.clone());
        self.clock = self.clock.wrapping_add(1);
        let last_used = self.clock;

        if self.entries.contains_key(resource) {
            let (result, added_bytes) = {
                let entry = self
                    .entries
                    .get_mut(resource)
                    .expect("the cache entry was just found");
                entry.last_used = last_used;
                let was_loading = matches!(entry.image, ImageCacheItem::Loading(_));
                let result = entry.image.get();
                let added_bytes = if was_loading {
                    result
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .map(|image| decoded_image_bytes(image))
                        .unwrap_or(0)
                } else {
                    0
                };
                if was_loading {
                    entry.decoded_bytes = added_bytes;
                }
                (result, added_bytes)
            };
            self.decoded_bytes = self.decoded_bytes.saturating_add(added_bytes);
            self.schedule_eviction(resource, added_bytes > 0, window);
            return result;
        }

        let (task, _) = cx.fetch_asset::<ImgResourceLoader>(resource);
        let entity = window.current_view();
        let notification_task = task.clone();
        let notification = window.spawn(cx, async move |cx| {
            let _ = notification_task.await;
            cx.on_next_frame(move |_, cx| {
                cx.notify(entity);
            });
        });
        self.entries.insert(
            resource.clone(),
            CacheEntry {
                image: ImageCacheItem::Loading(task.clone()),
                decoded_bytes: 0,
                last_used,
                _notification: notification,
            },
        );
        self.schedule_eviction(resource, true, window);
        None
    }

    /// Loads the heavily blurred stand-in for an image. The base image decode
    /// is started (and kept warm) through the normal `load` path; the blur is
    /// computed once from the decoded thumbnail and cached beside it.
    pub fn load_blurred(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<RenderImage>> {
        let base = self.load(resource, window, cx)?.ok()?;
        if let Some(blurred) = self.blurred.get(resource) {
            return Some(blurred.clone());
        }
        let blurred = Arc::new(blur_render_image(&base)?);
        self.blurred.insert(resource.clone(), blurred.clone());
        Some(blurred)
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut App) {
        self.release_all(Some(window), cx);
    }

    fn over_limit(&self) -> bool {
        self.decoded_bytes > self.byte_budget || self.entries.len() > self.max_items
    }

    fn schedule_eviction(
        &mut self,
        resource: &Resource,
        capacity_changed: bool,
        window: &mut Window,
    ) {
        if !self
            .eviction
            .should_start(self.over_limit(), resource, capacity_changed)
        {
            return;
        }

        let generation = self.eviction.start();
        let cache = self.weak_self.clone();
        window.on_next_frame(move |window, cx| {
            let eviction_cache = cache.clone();
            let _ = cache.update(cx, |cache, _| {
                if !cache.eviction.begin_collection(generation) {
                    return;
                }

                // A full draw records every image in the current retained
                // scene. A normal draw can reuse image sprites without calling
                // this cache, so a partial draw is not safe for eviction.
                window.refresh();
                window.on_next_frame(move |window, cx| {
                    let _ = eviction_cache.update(cx, |cache, cx| {
                        let Some(visible) = cache.eviction.finish_collection(generation) else {
                            return;
                        };
                        if cache.evict_to_limits(&visible, window, cx) {
                            // Prevent GPUI from replaying a retained scene that
                            // still contains a removed Metal atlas tile.
                            window.refresh();
                        }
                    });
                });
            });
        });
    }

    fn evict_to_limits(
        &mut self,
        protected: &HashSet<Resource>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let mut evicted = false;
        while self.decoded_bytes > self.byte_budget || self.entries.len() > self.max_items {
            let oldest = self
                .entries
                .iter()
                .filter(|(resource, _)| !protected.contains(*resource))
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(resource, _)| resource.clone());
            let Some(oldest) = oldest else {
                // A single image can be larger than the budget. Keep it because
                // refusing to cache it would start a new decode on every frame.
                break;
            };
            self.release(&oldest, Some(window), cx);
            evicted = true;
        }
        evicted
    }

    fn release(&mut self, resource: &Resource, mut window: Option<&mut Window>, cx: &mut App) {
        if let Some(blurred) = self.blurred.remove(resource) {
            cx.drop_image(blurred, window.as_deref_mut());
        }
        let Some(mut entry) = self.entries.remove(resource) else {
            return;
        };
        self.decoded_bytes = self.decoded_bytes.saturating_sub(entry.decoded_bytes);
        cx.remove_asset::<ImgResourceLoader>(resource);
        if let Some(Ok(image)) = entry.image.get() {
            cx.drop_image(image, window);
        }
    }

    fn release_all(&mut self, mut window: Option<&mut Window>, cx: &mut App) {
        let entries = std::mem::take(&mut self.entries);
        let blurred = std::mem::take(&mut self.blurred);
        self.decoded_bytes = 0;
        self.eviction.cancel();
        for (_, image) in blurred {
            cx.drop_image(image, window.as_deref_mut());
        }
        for (resource, mut entry) in entries {
            cx.remove_asset::<ImgResourceLoader>(&resource);
            if let Some(Ok(image)) = entry.image.get() {
                cx.drop_image(image, window.as_deref_mut());
            }
        }
    }
}

/// Shrinks the decoded image far below recognizability, blurs it, and scales it
/// back up so GPU bilinear sampling shows a smooth color wash instead of the
/// actual picture. The pixel format (premultiplied BGRA) survives untouched
/// because every step works per channel.
fn blur_render_image(image: &RenderImage) -> Option<RenderImage> {
    const SMALL_MAX_DIMENSION: u32 = 24;
    const SMOOTH_UPSCALE: u32 = 4;
    const BLUR_SIGMA: f32 = 2.5;

    let size = image.size(0);
    let (width, height) = (size.width.0 as u32, size.height.0 as u32);
    if width == 0 || height == 0 {
        return None;
    }
    let buffer = image::RgbaImage::from_raw(width, height, image.as_bytes(0)?.to_vec())?;
    let scale = SMALL_MAX_DIMENSION as f32 / width.max(height) as f32;
    let small_width = ((width as f32 * scale).round() as u32).max(1);
    let small_height = ((height as f32 * scale).round() as u32).max(1);
    let small = image::imageops::thumbnail(&buffer, small_width, small_height);
    let blurred = image::imageops::blur(&small, BLUR_SIGMA);
    let smooth = image::imageops::resize(
        &blurred,
        small_width * SMOOTH_UPSCALE,
        small_height * SMOOTH_UPSCALE,
        image::imageops::FilterType::Triangle,
    );
    Some(RenderImage::new(vec![image::Frame::new(smooth)]))
}

impl ImageCache for DecodedImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        DecodedImageCache::load(self, resource, window, cx)
    }
}

struct SpriteEntry {
    source: Arc<Image>,
    rendered: Option<Arc<RenderImage>>,
    decoded_bytes: usize,
    last_used: u64,
}

/// A weighted cache for card sprites.
///
/// Sprite sources are in-memory SVG images, so they do not use `ImageCache`.
/// This cache gives them the same bounded ownership rules as file-backed
/// images. The key is GPUI's content-derived image ID, which also makes
/// identical card sprites share one decoded result safely.
pub(super) struct CardSpriteCache {
    entries: HashMap<u64, SpriteEntry>,
    decoded_bytes: usize,
    byte_budget: usize,
    clock: u64,
    eviction: EvictionCycle<u64>,
    weak_self: WeakEntity<Self>,
}

impl CardSpriteCache {
    pub fn new(byte_budget: usize, cx: &mut Context<Self>) -> Self {
        cx.on_release(|cache, cx| {
            cache.release_all(None, cx);
        })
        .detach();

        Self {
            entries: HashMap::new(),
            decoded_bytes: 0,
            byte_budget,
            clock: 0,
            eviction: EvictionCycle::default(),
            weak_self: cx.weak_entity(),
        }
    }

    pub fn load(
        &mut self,
        source: Arc<Image>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<RenderImage>> {
        self.clock = self.clock.wrapping_add(1);
        let last_used = self.clock;
        let image_id = source.id();
        self.eviction.record(image_id);

        if let Some(entry) = self.entries.get_mut(&image_id) {
            entry.last_used = last_used;
            if let Some(rendered) = &entry.rendered {
                let rendered = rendered.clone();
                self.schedule_eviction(image_id, false, window);
                return Some(rendered);
            }
        } else {
            self.entries.insert(
                image_id,
                SpriteEntry {
                    source: source.clone(),
                    rendered: None,
                    decoded_bytes: 0,
                    last_used,
                },
            );
        }

        let rendered = source.use_render_image(window, cx)?;
        let decoded_bytes = decoded_image_bytes(&rendered);
        if let Some(entry) = self.entries.get_mut(&image_id) {
            entry.rendered = Some(rendered.clone());
            entry.decoded_bytes = decoded_bytes;
            self.decoded_bytes = self.decoded_bytes.saturating_add(decoded_bytes);
        }
        self.schedule_eviction(image_id, true, window);
        Some(rendered)
    }

    pub fn ready(&mut self, source: &Image, window: &mut Window) -> Option<Arc<RenderImage>> {
        self.clock = self.clock.wrapping_add(1);
        let last_used = self.clock;
        let image_id = source.id();
        self.eviction.record(image_id);
        let entry = self.entries.get_mut(&source.id())?;
        let rendered = entry.rendered.clone()?;
        entry.last_used = last_used;
        self.schedule_eviction(image_id, false, window);
        Some(rendered)
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut App) {
        self.release_all(Some(window), cx);
    }

    fn over_limit(&self) -> bool {
        self.decoded_bytes > self.byte_budget || self.entries.len() > CARD_SPRITE_CACHE_MAX_ITEMS
    }

    fn schedule_eviction(&mut self, image_id: u64, capacity_changed: bool, window: &mut Window) {
        if !self
            .eviction
            .should_start(self.over_limit(), &image_id, capacity_changed)
        {
            return;
        }

        let generation = self.eviction.start();
        let cache = self.weak_self.clone();
        window.on_next_frame(move |window, cx| {
            let eviction_cache = cache.clone();
            let _ = cache.update(cx, |cache, _| {
                if !cache.eviction.begin_collection(generation) {
                    return;
                }
                window.refresh();
                window.on_next_frame(move |window, cx| {
                    let _ = eviction_cache.update(cx, |cache, cx| {
                        let Some(visible) = cache.eviction.finish_collection(generation) else {
                            return;
                        };
                        if cache.evict_to_limits(&visible, window, cx) {
                            window.refresh();
                        }
                    });
                });
            });
        });
    }

    fn evict_to_limits(
        &mut self,
        protected: &HashSet<u64>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let mut evicted = false;
        while self.decoded_bytes > self.byte_budget
            || self.entries.len() > CARD_SPRITE_CACHE_MAX_ITEMS
        {
            let oldest = self
                .entries
                .iter()
                .filter(|(image_id, _)| !protected.contains(*image_id))
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(image_id, _)| *image_id);
            let Some(oldest) = oldest else {
                break;
            };
            self.release(oldest, Some(window), cx);
            evicted = true;
        }
        evicted
    }

    fn release(&mut self, image_id: u64, window: Option<&mut Window>, cx: &mut App) {
        let Some(entry) = self.entries.remove(&image_id) else {
            return;
        };
        self.decoded_bytes = self.decoded_bytes.saturating_sub(entry.decoded_bytes);
        entry.source.remove_asset(cx);
        if let Some(rendered) = entry.rendered {
            cx.drop_image(rendered, window);
        }
    }

    fn release_all(&mut self, mut window: Option<&mut Window>, cx: &mut App) {
        let entries = std::mem::take(&mut self.entries);
        self.decoded_bytes = 0;
        self.eviction.cancel();
        for (_, entry) in entries {
            entry.source.remove_asset(cx);
            if let Some(rendered) = entry.rendered {
                cx.drop_image(rendered, window.as_deref_mut());
            }
        }
    }
}

fn decoded_image_bytes(image: &RenderImage) -> usize {
    (0..image.frame_count())
        .filter_map(|frame_index| image.as_bytes(frame_index))
        .map(<[u8]>::len)
        .fold(0, usize::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::{EvictionCycle, decoded_image_bytes};
    use gpui::RenderImage;

    #[test]
    fn decoded_size_includes_every_animation_frame() {
        let frame = image::Frame::new(image::RgbaImage::new(12, 7));
        let image = RenderImage::new(vec![frame.clone(), frame]);

        assert_eq!(decoded_image_bytes(&image), 12 * 7 * 4 * 2);
    }

    #[test]
    fn eviction_cycle_collects_a_complete_visible_set() {
        let mut cycle = EvictionCycle::default();
        assert!(cycle.should_start(true, &7, true));
        let generation = cycle.start();
        cycle.record(99);
        assert!(cycle.begin_collection(generation));
        cycle.record(7);
        cycle.record(8);
        cycle.record(7);

        assert_eq!(
            cycle.finish_collection(generation),
            Some([7, 8].into_iter().collect())
        );
        assert!(!cycle.should_start(true, &7, false));
        assert!(cycle.should_start(true, &9, false));
    }

    #[test]
    fn cancelled_eviction_callbacks_cannot_resume() {
        let mut cycle = EvictionCycle::<u64>::default();
        let generation = cycle.start();
        cycle.cancel();

        assert!(!cycle.begin_collection(generation));
        assert_eq!(cycle.finish_collection(generation), None);
    }
}
