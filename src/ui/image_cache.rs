//! A weighted image cache for every file-backed image in the application.
//!
//! GPUI's default asset cache has no size limit. That is unsafe for an image
//! browser because each compressed file becomes a full BGRA buffer after it is
//! decoded. This cache owns the GPUI asset entries and releases both their CPU
//! buffers and GPU atlas entries when the decoded-byte budget is exceeded.

use gpui::{
    App, Asset, Context, EntityId, Image, ImageCache, ImageCacheError, ImageCacheItem,
    ImgResourceLoader, RenderImage, Resource, Task, WeakEntity, Window,
};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};

// Each decoded buffer normally also has a BGRA copy in the Metal atlas. Keep
// the CPU-side limits low enough that the combined unified-memory footprint
// remains well below half a gigabyte.
pub const DECODED_IMAGE_CACHE_BUDGET: usize = 96 * 1024 * 1024;
const DECODED_IMAGE_CACHE_MAX_ITEMS: usize = 1_024;
// Large enough that a whole board of small far-zoom sprites stays resident;
// a full zoom-out otherwise evicts and re-rasterizes hundreds of cards. The
// smallest tier costs ~75 KB per 800 px card, so this holds roughly 600 of them.
pub const CARD_SPRITE_CACHE_BUDGET: usize = 48 * 1024 * 1024;
// Two full 424-card tiers fit, including the previous tier used while a new
// zoom level rasterizes. A higher cap retained completed-but-not-yet-observed
// GPUI assets whose decoded bytes could not be accounted here.
const CARD_SPRITE_CACHE_MAX_ITEMS: usize = 1_024;

// Every eviction cycle forces two full-window redraws. When the visible set
// alone exceeds the budget nothing can be evicted, so uncapped cycles become
// a continuous redraw storm during zoom gestures; this spaces them out.
const EVICTION_CYCLE_SPACING: Duration = Duration::from_millis(250);
const SATURATED_EVICTION_SPACING: Duration = Duration::from_millis(1500);

/// The default decode resolution. Sized for the most demanding capped
/// consumer: a full-card image at the canvas's maximum zoom (2x) on a retina
/// display. Only the lightbox, zoomed past this, requests the native pixels.
pub const DECODED_LONG_EDGE_CAP: u32 = 2048;

/// One cache entry per (file, resolution tier). Both tiers of the same file
/// can coexist while a zoomed lightbox sharpens on top of the capped decode.
#[derive(Clone, PartialEq, Eq, Hash)]
struct DecodeKey {
    resource: Resource,
    max_dimension: Option<u32>,
}

/// GPUI's stock loader decodes files at native size. This wrapper downscales
/// oversized decodes so a huge attachment costs display-sized memory instead
/// of native-sized memory. ImageIO avoids the native buffer for oversized
/// static images on macOS; other formats only retain the capped copy.
enum CappedImageLoader {}

impl Asset for CappedImageLoader {
    type Source = DecodeKey;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let load = ImgResourceLoader::load(source.resource.clone(), cx);
        async move {
            // Decoded pixels are cached on disk as LZ4-compressed BGRA, which
            // reads back about five times faster than re-decoding the PNG.
            if let Resource::Path(path) = &source.resource
                && let Some(image) = super::disk_cache::load(path, source.max_dimension)
            {
                return Ok(Arc::new(image));
            }

            // ImageIO can create a capped thumbnail without materializing the
            // native pixel buffer. Pure-Rust decode remains faster for images
            // near the target size, but an 8K source otherwise peaks at its
            // full hundreds-of-megabytes buffer plus the resized output.
            #[cfg(target_os = "macos")]
            if let Resource::Path(path) = &source.resource
                && prefers_direct_imageio(path, source.max_dimension)
                && let Ok(bytes) = std::fs::read(path)
                && let Some(image) =
                    super::imageio::decode_render_image(bytes, source.max_dimension)
            {
                let image = Arc::new(image);
                super::disk_cache::store(path, source.max_dimension, &image);
                return Ok(image);
            }

            let decoded = match load.await {
                Ok(image) => Ok(match source.max_dimension {
                    Some(max_dimension) => downscale_to_fit(image, max_dimension),
                    None => image,
                }),
                // The pure-Rust decoders measure faster than ImageIO for every
                // format they support, so ImageIO only rescues the ones they
                // lack entirely (HEIC photo attachments, most notably).
                Err(error) => {
                    #[cfg(target_os = "macos")]
                    if let Resource::Path(path) = &source.resource
                        && let Ok(bytes) = std::fs::read(path)
                        && let Some(image) =
                            super::imageio::decode_render_image(bytes, source.max_dimension)
                    {
                        Ok(Arc::new(image))
                    } else {
                        Err(error)
                    }
                    #[cfg(not(target_os = "macos"))]
                    Err(error)
                }
            };
            if let (Ok(image), Resource::Path(path)) = (&decoded, &source.resource) {
                super::disk_cache::store(path, source.max_dimension, image);
            }
            decoded
        }
    }
}

#[cfg(target_os = "macos")]
fn prefers_direct_imageio(path: &std::path::Path, max_dimension: Option<u32>) -> bool {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    if extension
        .as_deref()
        .is_some_and(|extension| matches!(extension, "heic" | "heif" | "hif"))
    {
        // GPUI has no decoder for these, so going straight to ImageIO avoids
        // reading the entire file once just to discover that fact.
        return true;
    }
    let Some(max_dimension) = max_dimension else {
        return false;
    };
    // Animated formats stay in GPUI's decoder so frame delays and all frames
    // survive. SVG also stays in its renderer. These are the common static
    // formats for which ImageIO's thumbnail path is behaviorally equivalent.
    let supported_static = extension.as_deref().is_some_and(|extension| {
        matches!(extension, "png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp")
    });
    supported_static
        && image::image_dimensions(path)
            .is_ok_and(|(width, height)| width.max(height) > max_dimension.saturating_mul(2))
}

/// Returns the image unchanged when every frame already fits within `max`;
/// otherwise resizes oversized frames and preserves their delays.
fn downscale_to_fit(image: Arc<RenderImage>, max: u32) -> Arc<RenderImage> {
    if image.frame_count() == 0
        || (0..image.frame_count()).all(|index| {
            let size = image.size(index);
            (size.width.0 as u32).max(size.height.0 as u32) <= max
        })
    {
        return image;
    }

    let mut frames = Vec::with_capacity(image.frame_count());
    for index in 0..image.frame_count() {
        let size = image.size(index);
        let (width, height) = (size.width.0 as u32, size.height.0 as u32);
        let Some(bytes) = image.as_bytes(index) else {
            return image;
        };
        let Some(buffer) =
            image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(width, height, bytes)
        else {
            return image;
        };
        let resized = if width == 0 || height == 0 || width.max(height) <= max {
            image::RgbaImage::from_raw(width, height, bytes.to_vec())
                .expect("the borrowed image buffer already validated its length")
        } else {
            let scale = max as f32 / width.max(height) as f32;
            let scaled_width = ((width as f32 * scale).round() as u32).max(1);
            let scaled_height = ((height as f32 * scale).round() as u32).max(1);
            // The BGRA channel order survives resizing: every filter works per channel.
            image::imageops::resize(
                &buffer,
                scaled_width,
                scaled_height,
                image::imageops::FilterType::CatmullRom,
            )
        };
        frames.push(image::Frame::from_parts(resized, 0, 0, image.delay(index)));
    }
    Arc::new(RenderImage::new(frames))
}

struct CacheEntry {
    image: ImageCacheItem,
    decoded_bytes: usize,
    last_used: u64,
    load_serial: u64,
    notify_targets: Vec<EntityId>,
    _notification: Option<Task<()>>,
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
        self.scheduled = false;
        Some(std::mem::take(&mut self.used))
    }

    fn remember_visible(&mut self, visible: HashSet<K>) {
        self.last_visible = visible;
    }

    fn cancel(&mut self) {
        self.used.clear();
        self.last_visible.clear();
        self.scheduled = false;
        self.generation = self.generation.wrapping_add(1);
    }
}

/// The accounting both caches keep: an LRU clock, a decoded-byte total against
/// a budget, and the state of the deferred eviction cycle.
struct Budget<K> {
    decoded_bytes: usize,
    byte_budget: usize,
    max_items: usize,
    clock: u64,
    cycle: EvictionCycle<K>,
    backoff: Option<Instant>,
}

impl<K> Budget<K> {
    fn new(byte_budget: usize, max_items: usize) -> Self {
        Self {
            decoded_bytes: 0,
            byte_budget,
            max_items,
            clock: 0,
            cycle: EvictionCycle::default(),
            backoff: None,
        }
    }

    /// The next LRU stamp.
    fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    fn add_bytes(&mut self, bytes: usize) {
        self.decoded_bytes = self.decoded_bytes.saturating_add(bytes);
    }

    fn remove_bytes(&mut self, bytes: usize) {
        self.decoded_bytes = self.decoded_bytes.saturating_sub(bytes);
    }
}

/// A cache that owns decoded images against a byte budget.
///
/// Eviction cannot just drop the least-recently-used entry, because GPUI can
/// replay a retained scene without consulting the cache: what a normal frame
/// happens to ask for is not the set of images that are really on screen. The
/// cycle below forces a full draw, collects every key it touches, and only
/// then evicts what that draw did not use. Both caches need that protocol, so
/// it lives here once rather than in each of them.
trait Weighted: 'static + Sized {
    type Key: Clone + Eq + Hash;

    fn budget(&mut self) -> &mut Budget<Self::Key>;
    fn budget_ref(&self) -> &Budget<Self::Key>;
    fn item_count(&self) -> usize;
    /// Every unprotected key, ordered least-recently-used first.
    fn eviction_candidates(&self, protected: &HashSet<Self::Key>) -> Vec<Self::Key>;
    fn release(&mut self, key: &Self::Key, window: Option<&mut Window>, cx: &mut App);
    /// A handle the deferred eviction frames can come back through.
    fn weak_self(&self) -> WeakEntity<Self>;

    fn over_limit(&self) -> bool {
        let budget = self.budget_ref();
        budget.decoded_bytes > budget.byte_budget || self.item_count() > budget.max_items
    }

    fn record_use(&mut self, key: Self::Key) -> u64 {
        let budget = self.budget();
        budget.cycle.record(key);
        budget.tick()
    }

    /// Starts a cycle if this cache is over budget and `key` was not part of
    /// the last known visible set — that is, if something new pushed it over.
    fn schedule_eviction(&mut self, key: &Self::Key, capacity_changed: bool, window: &mut Window) {
        let over_limit = self.over_limit();
        let budget = self.budget();
        if budget.backoff.is_some_and(|until| Instant::now() < until) {
            return;
        }
        if !budget.cycle.should_start(over_limit, key, capacity_changed) {
            return;
        }
        budget.backoff = Some(Instant::now() + EVICTION_CYCLE_SPACING);
        let generation = budget.cycle.start();

        let cache = self.weak_self();
        window.on_next_frame(move |window, cx| {
            let collecting = cache.clone();
            let _ = cache.update(cx, |cache, _| {
                if !cache.budget().cycle.begin_collection(generation) {
                    return;
                }
                // A full draw records every image in the current retained
                // scene. A normal draw can reuse image sprites without calling
                // this cache, so a partial draw is not safe for eviction.
                window.refresh();
                window.on_next_frame(move |window, cx| {
                    let _ = collecting.update(cx, |cache, cx| {
                        let Some(visible) = cache.budget().cycle.finish_collection(generation)
                        else {
                            return;
                        };
                        if cache.evict_to_limits(&visible, window, cx) {
                            // Prevent GPUI from replaying a retained scene that
                            // still contains a removed Metal atlas tile.
                            window.refresh();
                        }
                        if cache.over_limit() {
                            // Everything left is on screen; retrying sooner
                            // would only repeat full redraws for nothing.
                            cache.budget().backoff =
                                Some(Instant::now() + SATURATED_EVICTION_SPACING);
                        }
                        cache.budget().cycle.remember_visible(visible);
                    });
                });
            });
        });
    }

    fn evict_to_limits(
        &mut self,
        protected: &HashSet<Self::Key>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let mut evicted = false;
        // Selecting the minimum from the whole map for every removal made a
        // large trim quadratic. Sort candidates once, then release linearly.
        for oldest in self.eviction_candidates(protected) {
            if !self.over_limit() {
                break;
            }
            self.release(&oldest, Some(window), cx);
            evicted = true;
        }
        evicted
    }
}

/// A blurred stand-in, or the work that is producing one.
enum Blurred {
    /// Dropping the task cancels the blur, so an evicted entry stops paying.
    Pending {
        _task: Task<()>,
    },
    Ready(Arc<RenderImage>),
}

pub(super) struct DecodedImageCache {
    entries: HashMap<DecodeKey, CacheEntry>,
    /// Tiny pre-blurred copies of decoded images, shown for in-progress
    /// generations. Each one lives and dies with its base entry.
    blurred: HashMap<DecodeKey, Blurred>,
    budget: Budget<DecodeKey>,
    next_load_serial: u64,
    pending_notifications: HashSet<EntityId>,
    notification_scheduled: bool,
    weak_self: WeakEntity<Self>,
}

impl Weighted for DecodedImageCache {
    type Key = DecodeKey;

    fn budget(&mut self) -> &mut Budget<DecodeKey> {
        &mut self.budget
    }

    fn budget_ref(&self) -> &Budget<DecodeKey> {
        &self.budget
    }

    fn item_count(&self) -> usize {
        self.entries.len()
    }

    fn eviction_candidates(&self, protected: &HashSet<DecodeKey>) -> Vec<DecodeKey> {
        let mut candidates: Vec<_> = self
            .entries
            .iter()
            .filter(|(key, _)| !protected.contains(*key))
            .map(|(key, entry)| (entry.last_used, key.clone()))
            .collect();
        candidates.sort_unstable_by_key(|(last_used, _)| *last_used);
        candidates.into_iter().map(|(_, key)| key).collect()
    }

    fn weak_self(&self) -> WeakEntity<Self> {
        self.weak_self.clone()
    }

    fn release(&mut self, key: &DecodeKey, mut window: Option<&mut Window>, cx: &mut App) {
        if let Some(Blurred::Ready(blurred)) = self.blurred.remove(key) {
            self.budget.remove_bytes(decoded_image_bytes(&blurred));
            cx.drop_image(blurred, window.as_deref_mut());
        }
        let Some(mut entry) = self.entries.remove(key) else {
            return;
        };
        self.budget.remove_bytes(entry.decoded_bytes);
        cx.remove_asset::<CappedImageLoader>(key);
        if let Some(Ok(image)) = entry.image.get() {
            cx.drop_image(image, window);
        }
    }
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
            budget: Budget::new(byte_budget, DECODED_IMAGE_CACHE_MAX_ITEMS),
            next_load_serial: 0,
            pending_notifications: HashSet::new(),
            notification_scheduled: false,
            weak_self: cx.weak_entity(),
        }
    }

    /// Loads the display-capped decode of an image. This is the tier every
    /// canvas and element consumer uses.
    pub fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.load_key(
            DecodeKey {
                resource: resource.clone(),
                max_dimension: Some(DECODED_LONG_EDGE_CAP),
            },
            window,
            cx,
        )
    }

    /// Starts a capped neighbor decode without treating it as visible, keeping
    /// it fresh on every render, or notifying the window when it completes.
    pub fn prefetch(&mut self, resource: &Resource, window: &mut Window, cx: &mut App) {
        let key = DecodeKey {
            resource: resource.clone(),
            max_dimension: Some(DECODED_LONG_EDGE_CAP),
        };
        if self.entries.contains_key(&key) {
            return;
        }
        let last_used = self.budget.tick();
        self.start_load(key.clone(), last_used, None, window, cx);
        self.schedule_eviction(&key, true, window);
    }

    /// Loads the native-resolution decode. Only the lightbox asks for this,
    /// and only once its zoom outgrows the capped tier.
    pub fn load_full(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.load_key(
            DecodeKey {
                resource: resource.clone(),
                max_dimension: None,
            },
            window,
            cx,
        )
    }

    fn load_key(
        &mut self,
        key: DecodeKey,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let last_used = self.record_use(key.clone());
        let target = window.current_view();

        if self.entries.contains_key(&key) {
            let (result, added_bytes) = {
                let entry = self
                    .entries
                    .get_mut(&key)
                    .expect("the cache entry was just found");
                entry.last_used = last_used;
                let was_loading = matches!(entry.image, ImageCacheItem::Loading(_));
                if was_loading && !entry.notify_targets.contains(&target) {
                    entry.notify_targets.push(target);
                }
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
                if was_loading && result.is_some() {
                    entry.decoded_bytes = added_bytes;
                    // This render observed the result directly, so the waiter
                    // does not need to invalidate it again.
                    entry.notify_targets.clear();
                }
                (result, added_bytes)
            };
            self.budget.add_bytes(added_bytes);
            self.schedule_eviction(&key, added_bytes > 0, window);
            return result;
        }

        self.start_load(key.clone(), last_used, Some(target), window, cx);
        self.schedule_eviction(&key, true, window);
        None
    }

    fn start_load(
        &mut self,
        key: DecodeKey,
        last_used: u64,
        target: Option<EntityId>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (task, _) = cx.fetch_asset::<CappedImageLoader>(&key);
        self.next_load_serial = self.next_load_serial.wrapping_add(1);
        let load_serial = self.next_load_serial;
        self.entries.insert(
            key.clone(),
            CacheEntry {
                image: ImageCacheItem::Loading(task.clone()),
                decoded_bytes: 0,
                last_used,
                load_serial,
                notify_targets: target.into_iter().collect(),
                _notification: None,
            },
        );

        let notification_task = task.clone();
        let cache = self.weak_self.clone();
        let completed_key = key.clone();
        let notification = window.spawn(cx, async move |cx| {
            let result = notification_task.await;
            let _ = cx.update(|window, app| {
                let _ = cache.update(app, |cache, _| {
                    let Some((added_bytes, targets)) =
                        cache.complete_load(&completed_key, load_serial, result)
                    else {
                        return;
                    };
                    cache.schedule_eviction(&completed_key, added_bytes > 0, window);
                    cache.queue_notifications(targets, window);
                });
            });
        });
        if let Some(entry) = self.entries.get_mut(&key)
            && entry.load_serial == load_serial
        {
            entry._notification = Some(notification);
        }
    }

    /// Accounts a result as soon as its background task completes. Without
    /// this, a decode that became invisible before its next paint retained CPU
    /// and GPU memory while reporting zero bytes to the eviction budget.
    fn complete_load(
        &mut self,
        key: &DecodeKey,
        load_serial: u64,
        result: Result<Arc<RenderImage>, ImageCacheError>,
    ) -> Option<(usize, Vec<EntityId>)> {
        let entry = self.entries.get_mut(key)?;
        if entry.load_serial != load_serial || !matches!(entry.image, ImageCacheItem::Loading(_)) {
            return None;
        }
        let decoded_bytes = result
            .as_ref()
            .ok()
            .map(|image| decoded_image_bytes(image))
            .unwrap_or(0);
        entry.image = ImageCacheItem::Loaded(result);
        entry.decoded_bytes = decoded_bytes;
        let targets = std::mem::take(&mut entry.notify_targets);
        self.budget.add_bytes(decoded_bytes);
        Some((decoded_bytes, targets))
    }

    fn queue_notifications(&mut self, targets: Vec<EntityId>, window: &mut Window) {
        if targets.is_empty() {
            return;
        }
        self.pending_notifications.extend(targets);
        if self.notification_scheduled {
            return;
        }
        self.notification_scheduled = true;
        let cache = self.weak_self.clone();
        window.on_next_frame(move |_, cx| {
            let targets = cache
                .update(cx, |cache, _| {
                    cache.notification_scheduled = false;
                    std::mem::take(&mut cache.pending_notifications)
                })
                .unwrap_or_default();
            for target in targets {
                cx.notify(target);
            }
        });
    }

    /// Loads the heavily blurred stand-in for an image. The base image decode
    /// is started (and kept warm) through the normal `load` path; the blur is
    /// computed once from the decoded thumbnail and cached beside it.
    ///
    /// The convolution costs about 8 ms per image — half a frame, and a branch
    /// off four source images would blur all four in the same one — so it runs
    /// on the background executor. Until it lands this returns `None` and the
    /// caller simply skips that image, which the shimmer already covers.
    pub fn load_blurred(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<RenderImage>> {
        let base = self.load(resource, window, cx)?.ok()?;
        let key = DecodeKey {
            resource: resource.clone(),
            max_dimension: Some(DECODED_LONG_EDGE_CAP),
        };
        match self.blurred.get(&key) {
            Some(Blurred::Ready(blurred)) => return Some(blurred.clone()),
            Some(Blurred::Pending { .. }) => return None,
            None => {}
        }

        let view = window.current_view();
        let cache = self.weak_self.clone();
        let pending = key.clone();
        let task = window.spawn(cx, async move |cx| {
            let blurred = cx
                .background_executor()
                .spawn(async move { blur_render_image(&base) })
                .await;
            let _ = cx.update(|window, app| {
                let _ = cache.update(app, |cache, _| {
                    // The base entry may have been evicted while this ran, in
                    // which case the stand-in has nothing left to stand in for.
                    let Some(blurred) = blurred.filter(|_| cache.entries.contains_key(&pending))
                    else {
                        cache.blurred.remove(&pending);
                        return;
                    };
                    let blurred = Arc::new(blurred);
                    cache.budget.add_bytes(decoded_image_bytes(&blurred));
                    cache
                        .blurred
                        .insert(pending.clone(), Blurred::Ready(blurred));
                    cache.schedule_eviction(&pending, true, window);
                    cache.queue_notifications(vec![view], window);
                });
            });
        });
        self.blurred.insert(key, Blurred::Pending { _task: task });
        None
    }

    /// Releases everything, returning whether anything was held. Freeing every
    /// entry at once also lets the Metal atlas drop all of its image textures,
    /// which individual LRU evictions cannot: a texture survives until its
    /// last tile dies, so scattered survivors pin whole textures.
    pub fn clear(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.release_all(Some(window), cx)
    }

    fn release_all(&mut self, mut window: Option<&mut Window>, cx: &mut App) -> bool {
        let entries = std::mem::take(&mut self.entries);
        let blurred = std::mem::take(&mut self.blurred);
        let released = !entries.is_empty() || !blurred.is_empty();
        self.budget.decoded_bytes = 0;
        self.budget.cycle.cancel();
        self.budget.backoff = None;
        self.pending_notifications.clear();
        for (_, blurred) in blurred {
            if let Blurred::Ready(image) = blurred {
                cx.drop_image(image, window.as_deref_mut());
            }
        }
        for (key, mut entry) in entries {
            cx.remove_asset::<CappedImageLoader>(&key);
            if let Some(Ok(image)) = entry.image.get() {
                cx.drop_image(image, window.as_deref_mut());
            }
        }
        released
    }
}

/// Applies a true Gaussian convolution to a sprite-sized copy. Processing
/// stays in premultiplied linear-light floats until the final BGRA8 conversion
/// so translucent edges and dark gradients stay clean.
fn blur_render_image(image: &RenderImage) -> Option<RenderImage> {
    const MAX_BLUR_DIMENSION: u32 = 320;
    const BLUR_SIGMA_AT_320_PX: f32 = 14.0;

    let size = image.size(0);
    let (width, height) = (size.width.0 as u32, size.height.0 as u32);
    if width == 0 || height == 0 {
        return None;
    }
    let pixels = image.as_bytes(0)?;
    if width
        .checked_mul(height)?
        .checked_mul(4)
        .is_none_or(|expected| expected as usize != pixels.len())
    {
        return None;
    }
    // A missing sprite thumbnail can briefly send a 1080 px thumbnail or a
    // capped original here. Shrink before converting to four-float pixels and
    // running the convolution, whose cost grows with pixel count and sigma.
    let scaled = if width.max(height) > MAX_BLUR_DIMENSION {
        let source = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(width, height, pixels)?;
        let scale = MAX_BLUR_DIMENSION as f32 / width.max(height) as f32;
        Some(image::imageops::resize(
            &source,
            ((width as f32 * scale).round() as u32).max(1),
            ((height as f32 * scale).round() as u32).max(1),
            image::imageops::FilterType::Triangle,
        ))
    } else {
        None
    };
    let (work_width, work_height, pixels) = if let Some(scaled) = &scaled {
        (scaled.width(), scaled.height(), scaled.as_raw().as_slice())
    } else {
        (width, height, pixels)
    };
    // RenderImage stores straight-alpha BGRA. The image operations are
    // channel-agnostic, so BGR order is harmless, but interpolation and blur
    // require premultiplied alpha to avoid colored transparent pixels bleeding
    // into their neighbors.
    let buffer = image::Rgba32FImage::from_fn(work_width, work_height, |x, y| {
        let index = (y as usize * work_width as usize + x as usize) * 4;
        let alpha = pixels[index + 3] as f32 / 255.0;
        image::Rgba([
            srgb_to_linear(pixels[index]) * alpha,
            srgb_to_linear(pixels[index + 1]) * alpha,
            srgb_to_linear(pixels[index + 2]) * alpha,
            alpha,
        ])
    });
    let sigma = (work_width.max(work_height) as f32 * BLUR_SIGMA_AT_320_PX / 320.0).max(0.8);
    let blurred = image::imageops::blur(&buffer, sigma);
    let output = image::RgbaImage::from_fn(work_width, work_height, |x, y| {
        let pixel = blurred.get_pixel(x, y).0;
        let alpha = pixel[3].clamp(0.0, 1.0);
        let inverse_alpha = if alpha > f32::EPSILON {
            alpha.recip()
        } else {
            0.0
        };
        image::Rgba([
            linear_to_srgb(pixel[0] * inverse_alpha),
            linear_to_srgb(pixel[1] * inverse_alpha),
            linear_to_srgb(pixel[2] * inverse_alpha),
            (alpha * 255.0).round() as u8,
        ])
    });
    Some(RenderImage::new(vec![image::Frame::new(output)]))
}

fn srgb_to_linear(channel: u8) -> f32 {
    let channel = channel as f32 / 255.0;
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(channel: f32) -> u8 {
    let channel = channel.clamp(0.0, 1.0);
    let encoded = if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
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
    budget: Budget<u64>,
    weak_self: WeakEntity<Self>,
}

impl Weighted for CardSpriteCache {
    type Key = u64;

    fn budget(&mut self) -> &mut Budget<u64> {
        &mut self.budget
    }

    fn budget_ref(&self) -> &Budget<u64> {
        &self.budget
    }

    fn item_count(&self) -> usize {
        self.entries.len()
    }

    fn eviction_candidates(&self, protected: &HashSet<u64>) -> Vec<u64> {
        let mut candidates: Vec<_> = self
            .entries
            .iter()
            .filter(|(image_id, _)| !protected.contains(*image_id))
            .map(|(image_id, entry)| (entry.last_used, *image_id))
            .collect();
        candidates.sort_unstable_by_key(|(last_used, _)| *last_used);
        candidates
            .into_iter()
            .map(|(_, image_id)| image_id)
            .collect()
    }

    fn weak_self(&self) -> WeakEntity<Self> {
        self.weak_self.clone()
    }

    fn release(&mut self, image_id: &u64, window: Option<&mut Window>, cx: &mut App) {
        let Some(entry) = self.entries.remove(image_id) else {
            return;
        };
        self.budget.remove_bytes(entry.decoded_bytes);
        entry.source.remove_asset(cx);
        if let Some(rendered) = entry.rendered {
            cx.drop_image(rendered, window);
        }
    }
}

impl CardSpriteCache {
    pub fn new(byte_budget: usize, cx: &mut Context<Self>) -> Self {
        cx.on_release(|cache, cx| {
            cache.release_all(None, cx);
        })
        .detach();

        Self {
            entries: HashMap::new(),
            budget: Budget::new(byte_budget, CARD_SPRITE_CACHE_MAX_ITEMS),
            weak_self: cx.weak_entity(),
        }
    }

    pub fn load(
        &mut self,
        source: Arc<Image>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<RenderImage>> {
        let image_id = source.id();
        let last_used = self.record_use(image_id);

        let mut inserted = false;
        if let Some(entry) = self.entries.get_mut(&image_id) {
            entry.last_used = last_used;
            if let Some(rendered) = &entry.rendered {
                let rendered = rendered.clone();
                self.schedule_eviction(&image_id, false, window);
                return Some(rendered);
            }
        } else {
            inserted = true;
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
        // Pending GPUI assets have no decoded byte count yet, so the item cap
        // must be enforced even when `use_render_image` returns `None` below.
        if inserted {
            self.schedule_eviction(&image_id, true, window);
        }

        let rendered = source.use_render_image(window, cx)?;
        let decoded_bytes = decoded_image_bytes(&rendered);
        if let Some(entry) = self.entries.get_mut(&image_id) {
            entry.rendered = Some(rendered.clone());
            entry.decoded_bytes = decoded_bytes;
            self.budget.add_bytes(decoded_bytes);
        }
        self.schedule_eviction(&image_id, true, window);
        Some(rendered)
    }

    pub fn ready(&mut self, source: &Image, window: &mut Window) -> Option<Arc<RenderImage>> {
        let image_id = source.id();
        let last_used = self.record_use(image_id);
        let entry = self.entries.get_mut(&image_id)?;
        let rendered = entry.rendered.clone()?;
        entry.last_used = last_used;
        self.schedule_eviction(&image_id, false, window);
        Some(rendered)
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.release_all(Some(window), cx)
    }

    fn release_all(&mut self, mut window: Option<&mut Window>, cx: &mut App) -> bool {
        let entries = std::mem::take(&mut self.entries);
        let released = !entries.is_empty();
        self.budget.decoded_bytes = 0;
        self.budget.cycle.cancel();
        self.budget.backoff = None;
        for (_, entry) in entries {
            entry.source.remove_asset(cx);
            if let Some(rendered) = entry.rendered {
                cx.drop_image(rendered, window.as_deref_mut());
            }
        }
        released
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
    #[cfg(target_os = "macos")]
    use super::prefers_direct_imageio;
    use super::{EvictionCycle, blur_render_image, decoded_image_bytes, downscale_to_fit};
    use gpui::RenderImage;
    use std::sync::Arc;

    #[test]
    fn oversized_decodes_shrink_to_the_cap_and_small_ones_pass_through() {
        let large = Arc::new(RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::new(4000, 1000),
        )]));
        let capped = downscale_to_fit(large, 2048);
        let size = capped.size(0);
        assert_eq!((size.width.0, size.height.0), (2048, 512));

        let small = Arc::new(RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::new(720, 480),
        )]));
        let untouched = downscale_to_fit(small.clone(), 2048);
        assert_eq!(untouched.id, small.id);

        let delay = image::Delay::from_numer_denom_ms(80, 1);
        let frame = image::Frame::from_parts(image::RgbaImage::new(4000, 1000), 0, 0, delay);
        let animated = Arc::new(RenderImage::new(vec![frame.clone(), frame]));
        let capped = downscale_to_fit(animated.clone(), 2048);
        assert_ne!(capped.id, animated.id);
        assert_eq!(capped.frame_count(), 2);
        assert_eq!(
            (capped.size(1).width.0, capped.size(1).height.0),
            (2048, 512)
        );
        assert_eq!(capped.delay(1).numer_denom_ms(), delay.numer_denom_ms());
    }

    #[test]
    fn decoded_size_includes_every_animation_frame() {
        let frame = image::Frame::new(image::RgbaImage::new(12, 7));
        let image = RenderImage::new(vec![frame.clone(), frame]);

        assert_eq!(decoded_image_bytes(&image), 12 * 7 * 4 * 2);
    }

    #[test]
    fn gaussian_blur_preserves_dimensions_and_ignores_color_under_transparency() {
        let mut pixels = image::RgbaImage::new(48, 24);
        for (x, _, pixel) in pixels.enumerate_pixels_mut() {
            *pixel = if x < 24 {
                image::Rgba([180, 180, 180, 255])
            } else {
                // Hidden red in BGRA order must not tint the blurred edge.
                image::Rgba([0, 0, 255, 0])
            };
        }
        let source = RenderImage::new(vec![image::Frame::new(pixels)]);
        let blurred = blur_render_image(&source).expect("blurred stand-in");

        let size = blurred.size(0);
        assert_eq!((size.width.0, size.height.0), (48, 24));
        for pixel in blurred.as_bytes(0).expect("frame").chunks_exact(4) {
            if pixel[3] > 8 {
                assert!((pixel[0] as i16 - pixel[1] as i16).abs() <= 1);
                assert!((pixel[1] as i16 - pixel[2] as i16).abs() <= 1);
            }
        }
    }

    #[test]
    fn gaussian_blur_bounds_oversized_fallback_inputs_before_convolution() {
        let source = RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(1280, 640))]);
        let blurred = blur_render_image(&source).expect("blurred stand-in");

        assert_eq!(
            (blurred.size(0).width.0, blurred.size(0).height.0),
            (320, 160)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn imageio_downsample_threshold_excludes_small_and_animated_sources() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let oversized = directory.path().join("oversized.png");
        image::RgbaImage::new(5000, 10)
            .save(&oversized)
            .expect("oversized png");
        let near_cap = directory.path().join("near-cap.png");
        image::RgbaImage::new(4000, 10)
            .save(&near_cap)
            .expect("near-cap png");

        assert!(prefers_direct_imageio(&oversized, Some(2048)));
        assert!(!prefers_direct_imageio(&near_cap, Some(2048)));
        assert!(!prefers_direct_imageio(
            &directory.path().join("animated.gif"),
            Some(2048)
        ));
        assert!(!prefers_direct_imageio(
            &directory.path().join("vector.svg"),
            Some(2048)
        ));
        assert!(prefers_direct_imageio(
            &directory.path().join("photo.heic"),
            None
        ));
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

        let visible = cycle
            .finish_collection(generation)
            .expect("completed collection");
        assert_eq!(visible, [7, 8].into_iter().collect());
        cycle.remember_visible(visible);
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
