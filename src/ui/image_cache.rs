//! A weighted image cache for every file-backed image in the application.
//!
//! GPUI's default asset cache has no size limit. That is unsafe for an image
//! browser because each compressed file becomes a full BGRA buffer after it is
//! decoded. This cache owns the GPUI asset entries and releases both their CPU
//! buffers and GPU atlas entries when the decoded-byte budget is exceeded.

use gpui::{
    App, Asset, Context, Entity, EntityId, Image, ImageCache, ImageCacheError, ImageCacheItem,
    ImgResourceLoader, RenderImage, Resource, Task, WeakEntity, Window,
};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;

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

/// The largest decode any consumer asks for: the lightbox at its fitted size
/// and the original a card image promotes to at the canvas's maximum zoom
/// (2x), both on a retina display. Only a lightbox zoomed past this requests
/// the native pixels.
pub const DECODED_LONG_EDGE_CAP: u32 = 2048;
/// Thumbnail files are written at this size, so every thumbnail consumer
/// decodes the file's own pixels and shares one tier. A full-width card image
/// at zoom 1 on a retina display needs exactly this many pixels; the original
/// takes over past that. Thumbnails from older versions are larger and are
/// downscaled to it on their first decode.
pub const THUMBNAIL_DECODE_CAP: u32 = crate::storage::THUMBNAIL_MAX_DIMENSION;

/// Each first paint copies a whole decoded buffer into the atlas on the main
/// thread. A burst of landed decodes is spread over a few frames rather than
/// stalling one; the rest paint on the frames right after.
const MAX_FIRST_PAINTS_PER_FRAME: usize = 8;

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
        let file_backed = matches!(&source.resource, Resource::Path(_));
        let decode = async move {
            // Decoded pixels are cached on disk as LZ4-compressed BGRA, which
            // reads back about five times faster than re-decoding the PNG.
            if let Resource::Path(path) = &source.resource
                && let Some(image) = crate::disk_cache::load(path, source.max_dimension)
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
                store_sidecar_in_background(path.clone(), source.max_dimension, image.clone());
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
                store_sidecar_in_background(path.clone(), source.max_dimension, image.clone());
            }
            decoded
        };
        async move {
            if file_backed {
                // GPUI's file loader also uses synchronous reads. Poll the
                // entire file decode on the blocking pool, including cache I/O.
                smol::unblock(move || smol::block_on(decode)).await
            } else {
                decode.await
            }
        }
    }
}

/// The sidecar only speeds up the next load, so it must not delay this one:
/// compressing and writing it moves to the blocking pool.
fn store_sidecar_in_background(
    path: Arc<std::path::Path>,
    max_dimension: Option<u32>,
    image: Arc<RenderImage>,
) {
    smol::unblock(move || crate::disk_cache::store(&path, max_dimension, &image)).detach();
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
    /// Decoded but not yet handed to a paint, so its atlas upload is pending.
    unpainted: bool,
    _notification: Option<Task<()>>,
}

/// The accounting both caches keep: an LRU clock, a decoded-byte total against
/// a budget, and the keys the current frame has touched.
struct Budget<K> {
    decoded_bytes: usize,
    byte_budget: usize,
    max_items: usize,
    clock: u64,
    used_this_frame: HashSet<K>,
}

impl<K> Budget<K> {
    fn new(byte_budget: usize, max_items: usize) -> Self {
        Self {
            decoded_bytes: 0,
            byte_budget,
            max_items,
            clock: 0,
            used_this_frame: HashSet::new(),
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
/// replay a retained scene without consulting the cache: a frame that reuses
/// the app view's prepaint never calls `load`, so its usage would look empty.
/// Every image load happens inside one render of the app view, so that view
/// brackets each of its renders with [`Self::begin_frame`] and, once the draw
/// is on screen, [`Self::finish_frame`]. The keys recorded in between are
/// exactly what that draw painted; everything else is fair game, oldest
/// first. No extra draws are forced: the caller only redraws when something
/// was actually released, so a scene never replays a dropped atlas tile.
trait Weighted: 'static + Sized {
    type Key: Clone + Eq + Hash;

    fn budget(&mut self) -> &mut Budget<Self::Key>;
    fn budget_ref(&self) -> &Budget<Self::Key>;
    fn item_count(&self) -> usize;
    /// Every unprotected key, ordered least-recently-used first.
    fn eviction_candidates(&self, protected: &HashSet<Self::Key>) -> Vec<Self::Key>;
    fn release(&mut self, key: &Self::Key, window: Option<&mut Window>, cx: &mut App);

    fn over_limit(&self) -> bool {
        let budget = self.budget_ref();
        budget.decoded_bytes > budget.byte_budget || self.item_count() > budget.max_items
    }

    fn record_use(&mut self, key: Self::Key) -> u64 {
        let budget = self.budget();
        budget.used_this_frame.insert(key);
        budget.tick()
    }

    /// Forgets the previous frame's usage. Call before the app view renders.
    fn begin_frame(&mut self) {
        self.budget().used_this_frame.clear();
    }

    /// Releases least-recently-used entries the frame did not touch until the
    /// cache fits its limits again. Returns whether anything was released, in
    /// which case the caller must redraw the view that painted them.
    fn finish_frame(&mut self, mut window: Option<&mut Window>, cx: &mut App) -> bool {
        if !self.over_limit() {
            return false;
        }
        let protected = std::mem::take(&mut self.budget().used_this_frame);
        let mut evicted = false;
        // Selecting the minimum from the whole map for every removal made a
        // large trim quadratic. Sort candidates once, then release linearly.
        for oldest in self.eviction_candidates(&protected) {
            if !self.over_limit() {
                break;
            }
            self.release(&oldest, window.as_deref_mut(), cx);
            evicted = true;
        }
        self.budget().used_this_frame = protected;
        evicted
    }
}

/// Brackets one render of the view that paints every image and sprite. Call
/// at the top of its render: both caches forget the previous frame's usage,
/// and once this draw is on screen they trim whatever it did not paint. The
/// view is redrawn only if something was released.
pub(super) fn bracket_frame(
    image_cache: &Entity<DecodedImageCache>,
    sprite_cache: &Entity<CardSpriteCache>,
    view: EntityId,
    window: &mut Window,
    cx: &mut App,
) {
    image_cache.update(cx, |cache, _| cache.begin_frame());
    sprite_cache.update(cx, |cache, _| cache.begin_frame());
    let image_cache = image_cache.clone();
    let sprite_cache = sprite_cache.clone();
    window.on_next_frame(move |window, cx| {
        let images = image_cache.update(cx, |cache, cx| cache.finish_frame(Some(window), cx));
        let sprites = sprite_cache.update(cx, |cache, cx| cache.finish_frame(Some(window), cx));
        if images || sprites {
            cx.notify(view);
        }
    });
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
    first_paints_this_frame: usize,
    weak_self: WeakEntity<Self>,
}

impl Weighted for DecodedImageCache {
    type Key = DecodeKey;

    fn budget(&mut self) -> &mut Budget<DecodeKey> {
        &mut self.budget
    }

    fn begin_frame(&mut self) {
        self.budget.used_this_frame.clear();
        self.first_paints_this_frame = 0;
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
            first_paints_this_frame: 0,
            weak_self: cx.weak_entity(),
        }
    }

    /// Loads an image decoded to at most `max_dimension` on its long edge:
    /// the largest size the caller will ever paint it at, so a source never
    /// costs more memory than its on-screen size warrants.
    pub fn load(
        &mut self,
        resource: &Resource,
        max_dimension: u32,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.load_key(
            DecodeKey {
                resource: resource.clone(),
                max_dimension: Some(max_dimension),
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
        self.start_load(key, last_used, None, window, cx);
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

        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_used = last_used;
            let was_loading = matches!(entry.image, ImageCacheItem::Loading(_));
            if was_loading && !entry.notify_targets.contains(&target) {
                entry.notify_targets.push(target);
            }
            let result = entry.image.get();
            if was_loading && result.is_some() {
                let added_bytes = result
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .map(|image| decoded_image_bytes(image))
                    .unwrap_or(0);
                entry.decoded_bytes = added_bytes;
                entry.unpainted = added_bytes > 0;
                // This render observed the result directly, so the waiter
                // does not need to invalidate it again.
                entry.notify_targets.clear();
                self.budget.add_bytes(added_bytes);
            }
            if entry.unpainted {
                if self.first_paints_this_frame >= MAX_FIRST_PAINTS_PER_FRAME {
                    window.request_animation_frame();
                    return None;
                }
                self.first_paints_this_frame += 1;
                entry.unpainted = false;
            }
            return result;
        }

        self.start_load(key, last_used, Some(target), window, cx);
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
                unpainted: false,
                _notification: None,
            },
        );

        let notification_task = task.clone();
        let cache = self.weak_self.clone();
        let completed_key = key.clone();
        let notification = window.spawn(cx, async move |cx| {
            let result = notification_task.await;
            let _ = cx.update(|_, app| {
                let Ok(Some(targets)) = cache.update(app, |cache, _| {
                    cache.complete_load(&completed_key, load_serial, result)
                }) else {
                    return;
                };
                // GPUI coalesces repeated notifies, so the views that asked
                // for this image redraw on the very next frame.
                for target in targets {
                    app.notify(target);
                }
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
    ) -> Option<Vec<EntityId>> {
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
        entry.unpainted = decoded_bytes > 0;
        let targets = std::mem::take(&mut entry.notify_targets);
        self.budget.add_bytes(decoded_bytes);
        Some(targets)
    }

    /// Loads the heavily blurred stand-in for a thumbnail. The base decode is
    /// started (and kept warm) through the normal `load` path; the blur is
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
        let base = self
            .load(resource, THUMBNAIL_DECODE_CAP, window, cx)?
            .ok()?;
        let key = DecodeKey {
            resource: resource.clone(),
            max_dimension: Some(THUMBNAIL_DECODE_CAP),
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
            let _ = cx.update(|_, app| {
                let ready = cache.update(app, |cache, _| {
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
                });
                if ready.unwrap_or(false) {
                    app.notify(view);
                }
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
        self.budget.used_this_frame.clear();
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
    // A missing sprite thumbnail can briefly send a 680 px thumbnail or a
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

/// The `img` elements of the gallery, board switcher, and lightbox placeholder
/// all show thumbnails, so they share the thumbnail tier with the canvas.
impl ImageCache for DecodedImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        DecodedImageCache::load(self, resource, THUMBNAIL_DECODE_CAP, window, cx)
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
                return Some(rendered.clone());
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
        Some(rendered)
    }

    /// An already rasterized sprite, without starting any work for a missing one.
    pub fn ready(&mut self, source: &Image) -> Option<Arc<RenderImage>> {
        let image_id = source.id();
        let last_used = self.record_use(image_id);
        let entry = self.entries.get_mut(&image_id)?;
        let rendered = entry.rendered.clone()?;
        entry.last_used = last_used;
        Some(rendered)
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.release_all(Some(window), cx)
    }

    fn release_all(&mut self, mut window: Option<&mut Window>, cx: &mut App) -> bool {
        let entries = std::mem::take(&mut self.entries);
        let released = !entries.is_empty();
        self.budget.decoded_bytes = 0;
        self.budget.used_this_frame.clear();
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
    use super::{
        Budget, CappedImageLoader, DecodeKey, Weighted, blur_render_image, decoded_image_bytes,
        downscale_to_fit,
    };
    use gpui::{App, Asset, RenderImage, Resource, TestAppContext, Window};
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    #[gpui::test]
    fn file_loader_preserves_capped_decodes_and_read_errors(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("image.png");
        image::RgbaImage::from_pixel(24, 12, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .expect("write png");
        let image = cx.update(|cx| {
            smol::block_on(CappedImageLoader::load(
                DecodeKey {
                    resource: Resource::Path(path.clone().into()),
                    max_dimension: Some(16),
                },
                cx,
            ))
            .expect("decode png on blocking pool")
        });
        assert_eq!(
            image.size(0),
            gpui::size(gpui::DevicePixels(16), gpui::DevicePixels(8))
        );
        assert_eq!(
            &image.as_bytes(0).expect("decoded pixels")[..4],
            &[0, 0, 255, 255]
        );

        let missing = cx.update(|cx| {
            smol::block_on(CappedImageLoader::load(
                DecodeKey {
                    resource: Resource::Path(directory.path().join("missing.png").into()),
                    max_dimension: None,
                },
                cx,
            ))
        });
        assert!(missing.is_err());
    }

    /// The eviction protocol with the GPUI asset plumbing stubbed out.
    struct MockCache {
        entries: HashMap<u32, (usize, u64)>,
        budget: Budget<u32>,
        released: Vec<u32>,
    }

    impl MockCache {
        fn insert(&mut self, key: u32, bytes: usize) {
            let last_used = self.budget.tick();
            self.entries.insert(key, (bytes, last_used));
            self.budget.add_bytes(bytes);
        }
    }

    impl Weighted for MockCache {
        type Key = u32;

        fn budget(&mut self) -> &mut Budget<u32> {
            &mut self.budget
        }

        fn budget_ref(&self) -> &Budget<u32> {
            &self.budget
        }

        fn item_count(&self) -> usize {
            self.entries.len()
        }

        fn eviction_candidates(&self, protected: &HashSet<u32>) -> Vec<u32> {
            let mut candidates: Vec<_> = self
                .entries
                .iter()
                .filter(|(key, _)| !protected.contains(*key))
                .map(|(key, (_, last_used))| (*last_used, *key))
                .collect();
            candidates.sort_unstable();
            candidates.into_iter().map(|(_, key)| key).collect()
        }

        fn release(&mut self, key: &u32, _: Option<&mut Window>, _: &mut App) {
            let (bytes, _) = self.entries.remove(key).expect("released key exists");
            self.budget.remove_bytes(bytes);
            self.released.push(*key);
        }
    }

    #[gpui::test]
    fn finishing_a_frame_evicts_only_what_it_did_not_paint_oldest_first(cx: &mut TestAppContext) {
        let mut cache = MockCache {
            entries: HashMap::new(),
            budget: Budget::new(100, usize::MAX),
            released: Vec::new(),
        };
        for key in 1..=4 {
            cache.insert(key, 40);
        }

        cache.begin_frame();
        cache.record_use(4);
        cache.record_use(3);
        assert!(cx.update(|cx| cache.finish_frame(None, cx)));
        assert_eq!(cache.released, [1, 2]);
        assert_eq!(cache.budget.decoded_bytes, 80);

        // Within budget: nothing is released even for keys the frame skipped.
        cache.begin_frame();
        cache.record_use(3);
        assert!(!cx.update(|cx| cache.finish_frame(None, cx)));
        assert_eq!(cache.entries.len(), 2);

        // Saturated: the frame painted everything, so nothing can go.
        cache.insert(5, 40);
        cache.begin_frame();
        for key in [3, 4, 5] {
            cache.record_use(key);
        }
        assert!(!cx.update(|cx| cache.finish_frame(None, cx)));
        assert_eq!(cache.entries.len(), 3);
    }

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
}
