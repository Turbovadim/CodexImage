//! A weighted image cache for every file-backed image in the application.
//!
//! GPUI's default asset cache has no size limit. That is unsafe for an image
//! browser because each compressed file becomes a full BGRA buffer after it is
//! decoded. This cache owns the GPUI asset entries and releases both their CPU
//! buffers and GPU atlas entries when the decoded-byte budget is exceeded.

use gpui::{
    App, Asset, Context, Image, ImageCache, ImageCacheError, ImageCacheItem, ImgResourceLoader,
    RenderImage, Resource, Task, WeakEntity, Window,
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
// a full zoom-out otherwise evicts and re-rasterizes hundreds of cards.
pub const CARD_SPRITE_CACHE_BUDGET: usize = 32 * 1024 * 1024;
const CARD_SPRITE_CACHE_MAX_ITEMS: usize = 4_096;

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
/// of native-sized memory. The full decode still exists transiently, but only
/// the capped copy is cached and uploaded to the Metal atlas.
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
                            super::imageio::decode_render_image(&bytes, source.max_dimension)
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

/// Returns the image unchanged when its long edge already fits within `max`;
/// otherwise resizes the frame down to it. Animations pass through untouched
/// because per-frame delays are not reachable through `RenderImage`.
fn downscale_to_fit(image: Arc<RenderImage>, max: u32) -> Arc<RenderImage> {
    if image.frame_count() != 1 {
        return image;
    }
    let size = image.size(0);
    let (width, height) = (size.width.0 as u32, size.height.0 as u32);
    if width == 0 || height == 0 || width.max(height) <= max {
        return image;
    }
    let Some(buffer) = image
        .as_bytes(0)
        .and_then(|bytes| image::RgbaImage::from_raw(width, height, bytes.to_vec()))
    else {
        return image;
    };
    let scale = max as f32 / width.max(height) as f32;
    let scaled_width = ((width as f32 * scale).round() as u32).max(1);
    let scaled_height = ((height as f32 * scale).round() as u32).max(1);
    // The BGRA channel order survives resizing: every filter works per channel.
    let resized = image::imageops::resize(
        &buffer,
        scaled_width,
        scaled_height,
        image::imageops::FilterType::CatmullRom,
    );
    Arc::new(RenderImage::new(vec![image::Frame::new(resized)]))
}

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
    /// The least-recently-used key that `protected` does not cover.
    fn oldest_unprotected(&self, protected: &HashSet<Self::Key>) -> Option<Self::Key>;
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
        while self.over_limit() {
            // A single image can be larger than the budget. Keep it, because
            // refusing to cache it would start a new decode on every frame.
            let Some(oldest) = self.oldest_unprotected(protected) else {
                break;
            };
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

    fn oldest_unprotected(&self, protected: &HashSet<DecodeKey>) -> Option<DecodeKey> {
        self.entries
            .iter()
            .filter(|(key, _)| !protected.contains(*key))
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone())
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

        if self.entries.contains_key(&key) {
            let (result, added_bytes) = {
                let entry = self
                    .entries
                    .get_mut(&key)
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
            self.budget.add_bytes(added_bytes);
            self.schedule_eviction(&key, added_bytes > 0, window);
            return result;
        }

        let (task, _) = cx.fetch_asset::<CappedImageLoader>(&key);
        let entity = window.current_view();
        let notification_task = task.clone();
        let notification = window.spawn(cx, async move |cx| {
            let _ = notification_task.await;
            cx.on_next_frame(move |_, cx| {
                cx.notify(entity);
            });
        });
        self.entries.insert(
            key.clone(),
            CacheEntry {
                image: ImageCacheItem::Loading(task.clone()),
                decoded_bytes: 0,
                last_used,
                _notification: notification,
            },
        );
        self.schedule_eviction(&key, true, window);
        None
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
            let stored = cache
                .update(cx, |cache, _| {
                    // The base entry may have been evicted while this ran, in
                    // which case the stand-in has nothing left to stand in for.
                    let Some(blurred) = blurred.filter(|_| cache.entries.contains_key(&pending))
                    else {
                        cache.blurred.remove(&pending);
                        return false;
                    };
                    let blurred = Arc::new(blurred);
                    cache.budget.add_bytes(decoded_image_bytes(&blurred));
                    cache.blurred.insert(pending, Blurred::Ready(blurred));
                    true
                })
                .unwrap_or(false);
            if stored {
                cx.on_next_frame(move |_, cx| cx.notify(view));
            }
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

/// Applies a true Gaussian convolution to the sprite thumbnail at its native
/// resolution. Processing stays in premultiplied linear-light floats until the
/// final BGRA8 conversion so translucent edges and dark gradients stay clean.
fn blur_render_image(image: &RenderImage) -> Option<RenderImage> {
    // Sprite thumbnails have a 320 px long edge. Scaling sigma with the input
    // keeps the same apparent blur for genuinely smaller source images.
    const BLUR_SIGMA_AT_320_PX: f32 = 14.0;

    let size = image.size(0);
    let (width, height) = (size.width.0 as u32, size.height.0 as u32);
    if width == 0 || height == 0 {
        return None;
    }
    let pixels = image.as_bytes(0)?;
    if pixels.len() != width as usize * height as usize * 4 {
        return None;
    }
    // RenderImage stores straight-alpha BGRA. The image operations are
    // channel-agnostic, so BGR order is harmless, but interpolation and blur
    // require premultiplied alpha to avoid colored transparent pixels bleeding
    // into their neighbors.
    let buffer = image::Rgba32FImage::from_fn(width, height, |x, y| {
        let index = (y as usize * width as usize + x as usize) * 4;
        let alpha = pixels[index + 3] as f32 / 255.0;
        image::Rgba([
            srgb_to_linear(pixels[index]) * alpha,
            srgb_to_linear(pixels[index + 1]) * alpha,
            srgb_to_linear(pixels[index + 2]) * alpha,
            alpha,
        ])
    });
    let sigma = (width.max(height) as f32 * BLUR_SIGMA_AT_320_PX / 320.0).max(0.8);
    let blurred = image::imageops::blur(&buffer, sigma);
    let output = image::RgbaImage::from_fn(width, height, |x, y| {
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

    fn oldest_unprotected(&self, protected: &HashSet<u64>) -> Option<u64> {
        self.entries
            .iter()
            .filter(|(image_id, _)| !protected.contains(*image_id))
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(image_id, _)| *image_id)
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

        if let Some(entry) = self.entries.get_mut(&image_id) {
            entry.last_used = last_used;
            if let Some(rendered) = &entry.rendered {
                let rendered = rendered.clone();
                self.schedule_eviction(&image_id, false, window);
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

    pub fn clear(&mut self, window: &mut Window, cx: &mut App) {
        self.release_all(Some(window), cx);
    }

    fn release_all(&mut self, mut window: Option<&mut Window>, cx: &mut App) {
        let entries = std::mem::take(&mut self.entries);
        self.budget.decoded_bytes = 0;
        self.budget.cycle.cancel();
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

        let frame = image::Frame::new(image::RgbaImage::new(4000, 1000));
        let animated = Arc::new(RenderImage::new(vec![frame.clone(), frame]));
        let untouched = downscale_to_fit(animated.clone(), 2048);
        assert_eq!(untouched.id, animated.id);
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
