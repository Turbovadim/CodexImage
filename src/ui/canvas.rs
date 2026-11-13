//! Painting the infinite canvas: the dot grid, the dashed parent connectors,
//! and each visible card (as a cached sprite when one is ready).

use super::card::MEDIA_RADIUS;
use super::card::{
    CARD_SPRITE_WIDTHS, CanvasNode, CardImageFit, CardPrimitive, CardRect, CardScene,
    FINEST_SPRITE_TIER,
};
use super::card_scene::MIN_LEGIBLE_FONT_SIZE;
use super::icons::Icon;
use super::image_cache::{
    CardSpriteCache, DECODED_LONG_EDGE_CAP, DecodedImageCache, THUMBNAIL_DECODE_CAP,
};
use super::theme;
use crate::layout::CARD_WIDTH;
use gpui::{
    App, BorderStyle, Bounds, ContentMask, Entity, Hsla, ObjectFit, PathBuilder, Pixels, Point,
    Resource, SharedString, TextAlign, TextRun, TransformationMatrix, Window, fill,
    linear_color_stop, linear_gradient, point, px, quad, size,
};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

const GRID_GAP: f32 = 28.;
const GRID_DOT_SIZE: f32 = 1.4;
/// Eight cells keep the 2x tile texture under the atlas page size, so it
/// shares a page instead of pinning a dedicated 13 MB texture as a 32-cell
/// tile did. A viewport then needs a few dozen tile quads, which is nothing.
const GRID_TILE_CELLS: usize = 8;
const GRID_TILE_SIZE: f32 = GRID_GAP * GRID_TILE_CELLS as f32;
const GRID_TEXTURE_SCALE: u32 = 2;
const GRID_ANTIALIAS_SAMPLES: u32 = 8;
const GRID_COLOR_BGRA: [u8; 3] = [0x2d, 0x22, 0x1e];
/// The closest two dots are ever allowed to sit on screen. Zooming out past
/// this doubles the world-space spacing instead of shrinking the dots into an
/// illegible haze, which also keeps the tile count bounded.
const GRID_MIN_SCREEN_GAP: f32 = 22.;
const GRID_MAX_DENSITY_STEP: f32 = 1_024.;
pub const VIEWPORT_CULL_MARGIN: f32 = 96.;
/// Smooth a large first frame over a few bounded frames instead of encoding
/// every newly visible SVG source in one application-thread burst.
pub const MAX_SPRITE_SOURCE_BUILDS_PER_FRAME: usize = 24;
const CONNECTOR_STROKE_WIDTH: f32 = 1.6;
const CONNECTOR_DASH_LENGTH: f32 = 7.;
const CONNECTOR_GAP_LENGTH: f32 = 5.;

impl From<CardImageFit> for ObjectFit {
    fn from(fit: CardImageFit) -> Self {
        match fit {
            CardImageFit::Contain => Self::Contain,
            CardImageFit::Cover => Self::Cover,
        }
    }
}

pub struct CanvasNodeFrame {
    pub node_index: usize,
    pub screen_x: f32,
    pub screen_y: f32,
    pub height: f32,
    pub targeted: bool,
    /// Pre-rendered "Generating · 12s · …" line, built once per frame so the
    /// paint pass never formats strings or clones the activity map.
    pub status_line: Option<SharedString>,
}

#[derive(Clone, Copy)]
pub struct CanvasConnector {
    pub from_x: f32,
    pub from_y: f32,
    pub to_x: f32,
    pub to_y: f32,
    /// Links a combined image's extra parent; drawn in the accent colour.
    pub merged: bool,
}

impl CanvasConnector {
    fn screen_points(
        self,
        camera_x: f32,
        camera_y: f32,
        zoom: f32,
    ) -> (Point<Pixels>, Point<Pixels>) {
        (
            point(
                px(camera_x + self.from_x * zoom),
                px(camera_y + self.from_y * zoom),
            ),
            point(
                px(camera_x + self.to_x * zoom),
                px(camera_y + self.to_y * zoom),
            ),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DotGridMetrics {
    tile_size: f32,
    dot_gap: f32,
    origin_x: f32,
    origin_y: f32,
}

/// How many world-space cells the tile texture stands in for. The texture is
/// periodic, so stretching it by a power of two draws a coarser grid with the
/// same dot size instead of more, smaller tiles.
fn dot_grid_density(zoom: f32) -> f32 {
    let mut density = 1.;
    while GRID_GAP * density * zoom < GRID_MIN_SCREEN_GAP && density < GRID_MAX_DENSITY_STEP {
        density *= 2.;
    }
    density
}

fn dot_grid_metrics(camera_x: f32, camera_y: f32, zoom: f32) -> DotGridMetrics {
    let zoom = zoom.max(0.0001);
    let density = dot_grid_density(zoom);
    let tile_size = GRID_TILE_SIZE * density * zoom;
    let dot_gap = GRID_GAP * density * zoom;
    let dot_offset = dot_gap / 2.;
    DotGridMetrics {
        tile_size,
        dot_gap,
        origin_x: (camera_x - dot_offset).rem_euclid(tile_size) - tile_size,
        origin_y: (camera_y - dot_offset).rem_euclid(tile_size) - tile_size,
    }
}

fn dot_grid_texture_pixels() -> image::RgbaImage {
    let texture_size = (GRID_TILE_SIZE * GRID_TEXTURE_SCALE as f32).round() as u32;
    let mut texture = image::RgbaImage::new(texture_size, texture_size);
    let scale = GRID_TEXTURE_SCALE as f32;
    let dot_radius = GRID_DOT_SIZE * scale / 2.;
    let samples_per_pixel = GRID_ANTIALIAS_SAMPLES.pow(2);

    for row in 0..GRID_TILE_CELLS {
        let center_y = (GRID_GAP / 2. + row as f32 * GRID_GAP) * scale;
        for column in 0..GRID_TILE_CELLS {
            let center_x = (GRID_GAP / 2. + column as f32 * GRID_GAP) * scale;
            let min_x = (center_x - dot_radius).floor().max(0.) as u32;
            let max_x = (center_x + dot_radius).ceil().min(texture_size as f32) as u32;
            let min_y = (center_y - dot_radius).floor().max(0.) as u32;
            let max_y = (center_y + dot_radius).ceil().min(texture_size as f32) as u32;

            for pixel_y in min_y..max_y {
                for pixel_x in min_x..max_x {
                    let mut covered_samples = 0;
                    for sample_y in 0..GRID_ANTIALIAS_SAMPLES {
                        let sample_y = pixel_y as f32
                            + (sample_y as f32 + 0.5) / GRID_ANTIALIAS_SAMPLES as f32;
                        for sample_x in 0..GRID_ANTIALIAS_SAMPLES {
                            let sample_x = pixel_x as f32
                                + (sample_x as f32 + 0.5) / GRID_ANTIALIAS_SAMPLES as f32;
                            let dx = sample_x - center_x;
                            let dy = sample_y - center_y;
                            covered_samples +=
                                u32::from(dx * dx + dy * dy <= dot_radius * dot_radius);
                        }
                    }
                    if covered_samples > 0 {
                        let alpha = ((covered_samples * 255 + samples_per_pixel / 2)
                            / samples_per_pixel) as u8;
                        texture.put_pixel(
                            pixel_x,
                            pixel_y,
                            image::Rgba([
                                GRID_COLOR_BGRA[0],
                                GRID_COLOR_BGRA[1],
                                GRID_COLOR_BGRA[2],
                                alpha,
                            ]),
                        );
                    }
                }
            }
        }
    }
    texture
}

fn dot_grid_image() -> Arc<gpui::RenderImage> {
    static IMAGE: OnceLock<Arc<gpui::RenderImage>> = OnceLock::new();
    IMAGE
        .get_or_init(|| {
            Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
                dot_grid_texture_pixels(),
            )]))
        })
        .clone()
}

pub fn paint_dot_grid(
    bounds: Bounds<Pixels>,
    camera_x: f32,
    camera_y: f32,
    zoom: f32,
    window: &mut Window,
) {
    let image = dot_grid_image();
    let metrics = dot_grid_metrics(camera_x, camera_y, zoom);
    let tile_size = px(metrics.tile_size);
    // paint_image clips each tile to `bounds`, so the loop needs no content mask.
    let mut y = bounds.top() + px(metrics.origin_y);
    while y < bounds.bottom() {
        let mut x = bounds.left() + px(metrics.origin_x);
        while x < bounds.right() {
            let _ = window.paint_image(
                bounds,
                Bounds {
                    origin: point(x, y),
                    size: size(tile_size, tile_size),
                },
                px(0.).into(),
                image.clone(),
                0,
                false,
            );
            x += tile_size;
        }
        y += tile_size;
    }
}

pub fn rect_is_visible(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    viewport_width: f32,
    viewport_height: f32,
    margin: f32,
) -> bool {
    x + width >= -margin
        && y + height >= -margin
        && x <= viewport_width + margin
        && y <= viewport_height + margin
}

pub fn edge_is_visible(
    from: Point<Pixels>,
    to: Point<Pixels>,
    viewport_width: f32,
    viewport_height: f32,
    margin: f32,
) -> bool {
    let from_x = f32::from(from.x);
    let from_y = f32::from(from.y);
    let to_x = f32::from(to.x);
    let to_y = f32::from(to.y);
    rect_is_visible(
        from_x.min(to_x),
        from_y.min(to_y),
        (from_x - to_x).abs(),
        (from_y - to_y).abs(),
        viewport_width,
        viewport_height,
        margin,
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ConnectorStyle {
    stroke_width: f32,
    dash_length: f32,
    gap_length: f32,
}

impl ConnectorStyle {
    fn for_zoom(zoom: f32) -> Self {
        debug_assert!(zoom > 0.);
        Self {
            stroke_width: (CONNECTOR_STROKE_WIDTH * zoom).max(0.55),
            dash_length: (CONNECTOR_DASH_LENGTH * zoom).max(2.5),
            gap_length: (CONNECTOR_GAP_LENGTH * zoom).max(1.75),
        }
    }
}

/// Transforms, culls, and dashes every visible parent-to-child connector.
/// Connectors are axis-aligned, so each dash is a plain quad: no tessellation
/// and no separate path render pass.
pub fn paint_connectors(
    edges: &[CanvasConnector],
    camera_x: f32,
    camera_y: f32,
    zoom: f32,
    viewport_width: f32,
    viewport_height: f32,
    window: &mut Window,
) {
    if edges.is_empty() {
        return;
    }
    let style = ConnectorStyle::for_zoom(zoom);
    let parent_color = theme::line();
    let merged_color = theme::accent().opacity(0.6);
    for edge in edges {
        let (from, to) = edge.screen_points(camera_x, camera_y, zoom);
        if edge_is_visible(
            from,
            to,
            viewport_width,
            viewport_height,
            VIEWPORT_CULL_MARGIN,
        ) {
            let color = if edge.merged {
                merged_color
            } else {
                parent_color
            };
            trace_connector_dashes(from, to, style, viewport_width, viewport_height, |dash| {
                window.paint_quad(fill(dash, color));
            });
        }
    }
}

/// Emits the on-screen dashes of the elbow connector `from` -> `to`. The dash
/// pattern runs continuously along the whole polyline, so it stays anchored to
/// the connector while only the visible part is walked.
fn trace_connector_dashes(
    from: Point<Pixels>,
    to: Point<Pixels>,
    style: ConnectorStyle,
    viewport_width: f32,
    viewport_height: f32,
    mut emit: impl FnMut(Bounds<Pixels>),
) {
    debug_assert!(style.dash_length > 0. && style.gap_length > 0.);
    let (from_x, from_y) = (f32::from(from.x), f32::from(from.y));
    let (to_x, to_y) = (f32::from(to.x), f32::from(to.y));
    let middle_y = (from_y + to_y) / 2.;
    let half_stroke = style.stroke_width / 2.;
    let period = style.dash_length + style.gap_length;
    // (vertical, fixed coordinate, start, end) along the moving axis.
    let segments = [
        (true, from_x, from_y, middle_y),
        (false, middle_y, from_x, to_x),
        (true, to_x, middle_y, to_y),
    ];
    let mut offset = 0.;
    for (vertical, fixed, start, end) in segments {
        let length = (end - start).abs();
        let segment_offset = offset;
        offset += length;
        let (fixed_limit, moving_limit) = if vertical {
            (viewport_width, viewport_height)
        } else {
            (viewport_height, viewport_width)
        };
        if length <= f32::EPSILON || fixed + half_stroke < 0. || fixed - half_stroke > fixed_limit {
            continue;
        }
        let direction = (end - start).signum();
        // The stretch of the segment, as distance from `start`, that is on screen.
        let (enter, exit) = if direction > 0. {
            (-start, moving_limit - start)
        } else {
            (start - moving_limit, start)
        };
        let visible_start = enter.max(0.);
        let visible_end = exit.min(length);
        if visible_start >= visible_end {
            continue;
        }
        let mut dash_index = ((segment_offset + visible_start) / period).floor();
        loop {
            let dash_start = dash_index * period - segment_offset;
            if dash_start >= visible_end {
                break;
            }
            let dash_end = (dash_start + style.dash_length).min(length);
            let dash_start = dash_start.max(0.);
            dash_index += 1.;
            if dash_end <= dash_start {
                continue;
            }
            let near = start + direction * if direction > 0. { dash_start } else { dash_end };
            let extent = dash_end - dash_start;
            emit(if vertical {
                canvas_bounds(fixed - half_stroke, near, style.stroke_width, extent)
            } else {
                canvas_bounds(near, fixed - half_stroke, extent, style.stroke_width)
            });
        }
    }
}

fn canvas_bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
}

/// Clip before shaping text or decoding images on partly visible cards.
fn bounds_are_visible(bounds: Bounds<Pixels>, window: &Window) -> bool {
    let viewport = window.viewport_size();
    rect_is_visible(
        f32::from(bounds.left()),
        f32::from(bounds.top()),
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
        f32::from(viewport.width),
        f32::from(viewport.height),
        0.,
    )
}

#[derive(Clone, Copy)]
struct CanvasTextStyle {
    /// The font size in world space, before `zoom`.
    font_size: f32,
    zoom: f32,
    /// The line height on screen.
    line_height: f32,
    color: gpui::Hsla,
    align: TextAlign,
}

fn paint_canvas_text(
    text: SharedString,
    bounds: Bounds<Pixels>,
    style: CanvasTextStyle,
    window: &mut Window,
) {
    // Skipping illegible text spares the shaping pass, which dominates a full
    // direct redraw of a large board at far-out zoom (hundreds of cards,
    // thousands of runs per frame).
    let screen_font_size = style.font_size * style.zoom;
    if text.is_empty()
        || screen_font_size < MIN_LEGIBLE_FONT_SIZE
        || !bounds_are_visible(bounds, window)
    {
        return;
    }
    let run = TextRun {
        len: text.len(),
        color: style.color,
        font: gpui::font(theme::CARD_FONT_FAMILY),
        ..Default::default()
    };
    // Shaped once in world space: the layout is the same at every zoom, so
    // GPUI's line cache keeps hitting through a zoom gesture and each glyph
    // only moves to its scaled position. Text then tracks its card exactly
    // instead of jumping between the widths of discrete font sizes.
    let line = window
        .text_system()
        .shape_line(text, px(style.font_size), &[run], None);
    // GPUI keys atlas glyphs on the exact font size, so quarter-pixel steps
    // bound how many sizes a continuous zoom rasterizes.
    let glyph_size = px((screen_font_size * 4.).round() / 4.);
    let width = line.width * style.zoom;
    let left = bounds.left()
        + match style.align {
            TextAlign::Left => px(0.),
            TextAlign::Center => (bounds.size.width - width) / 2.,
            TextAlign::Right => bounds.size.width - width,
        };
    // The same baseline placement as GPUI's line painter.
    let baseline = bounds.top()
        + (px(style.line_height) - (line.ascent + line.descent) * style.zoom) / 2.
        + line.ascent * style.zoom;
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        for run in &line.runs {
            for glyph in &run.glyphs {
                let origin = point(
                    left + glyph.position.x * style.zoom,
                    baseline + glyph.position.y * style.zoom,
                );
                let _ = if glyph.is_emoji {
                    window.paint_emoji(origin, run.font_id, glyph.id, glyph_size)
                } else {
                    window.paint_glyph(origin, run.font_id, glyph.id, glyph_size, style.color)
                };
            }
        }
    });
}

struct CanvasImageStyle {
    fit: ObjectFit,
    corner_radius: f32,
    blurred: bool,
    /// The largest long edge this paint can need, which picks the decode tier.
    decode_cap: u32,
    /// Whether a missing image starts decoding or is simply left unpainted.
    decode_missing: bool,
}

fn paint_canvas_image(
    path: &Arc<Path>,
    bounds: Bounds<Pixels>,
    style: CanvasImageStyle,
    image_cache: &Entity<DecodedImageCache>,
    window: &mut Window,
    cx: &mut App,
) {
    if !bounds_are_visible(bounds, window) {
        return;
    }
    let resource = Resource::Path(path.clone());
    let data = if style.blurred {
        let Some(data) =
            image_cache.update(cx, |cache, cx| cache.load_blurred(&resource, window, cx))
        else {
            return;
        };
        data
    } else {
        let Some(Ok(data)) = image_cache.update(cx, |cache, cx| {
            if style.decode_missing {
                cache.load(&resource, style.decode_cap, window, cx)
            } else {
                cache.ready(&resource, style.decode_cap, window, cx)
            }
        }) else {
            return;
        };
        data
    };
    if data.frame_count() == 0 {
        return;
    }
    let image_bounds = style.fit.get_bounds(bounds, data.size(0));
    // paint_image clips to bounds ∩ image_bounds itself, so no content mask is needed.
    let _ = window.paint_image(
        bounds,
        image_bounds,
        px(style.corner_radius).into(),
        data,
        0,
        false,
    );
}

/// How the current frame relates to the zoom gesture, decided once per frame.
#[derive(Clone, Copy)]
pub struct ZoomPhase {
    /// Zoom has held still long enough to rasterize sprites for its tier.
    pub settled: bool,
    /// Keep painting card text while the gesture moves. Shaping text for a
    /// handful of cards is cheap; only a crowded overview skips it.
    pub gesture_text: bool,
}

impl ZoomPhase {
    pub fn new(settled: bool, visible_cards: usize) -> Self {
        Self {
            settled,
            gesture_text: visible_cards <= MAX_TEXT_CARDS_DURING_GESTURE,
        }
    }
}

/// Above this many visible cards, text is dropped while a zoom gesture moves.
const MAX_TEXT_CARDS_DURING_GESTURE: usize = 48;

/// Which primitives a direct paint of the scene draws.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SceneLayers {
    All,
    /// Everything, but only the images already decoded: the card's sprite is
    /// rasterizing and replaces this paint, so a decode started now would be
    /// thrown away, after loading the same thumbnails the sprite reads itself.
    AwaitingSprite,
    /// A crowded overview mid-gesture: prioritize image continuity.
    NoText,
    /// Crisp display thumbnails laid over a scaled sprite's small copies.
    ImagesOnly,
}

/// The best sprite already rasterized for this card: the target tier when it
/// has been visited before, otherwise whatever tier was painted last. Never
/// starts new raster work, so it is safe to call every frame of a gesture.
fn ready_sprite(
    canvas_node: &CanvasNode,
    tiers: [usize; 2],
    sprite_cache: &Entity<CardSpriteCache>,
    cx: &mut App,
) -> Option<Arc<gpui::RenderImage>> {
    tiers.into_iter().find_map(|tier| {
        canvas_node
            .ready_sprite_image(tier)
            .and_then(|image| sprite_cache.update(cx, |cache, _| cache.ready(image)))
    })
}

#[expect(clippy::too_many_arguments)]
pub fn paint_canvas_node(
    frame: &CanvasNodeFrame,
    canvas_node: &CanvasNode,
    zoom: f32,
    phase: ZoomPhase,
    image_cache: &Entity<DecodedImageCache>,
    sprite_cache: &Entity<CardSpriteCache>,
    sprite_source_budget: &mut usize,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let bounds = canvas_bounds(
        frame.screen_x,
        frame.screen_y,
        CARD_WIDTH * zoom,
        frame.height,
    );
    // resvg implements large feGaussianBlur filters with quantized box-blur
    // passes, which become visible as contour bands over dark images. Running
    // cards are few and short-lived, so paint their shared CPU-blurred image
    // directly instead of rasterizing the blurred image into a card sprite.
    let sprites_allowed = !canvas_node.scene.has_blurred_images;
    let zoom_tier = sprite_tier_for_zoom(zoom);
    // Above the tiers a card rests as direct paint. The finest sprite is still
    // rasterized there as the gesture fallback: a raster scales on the GPU
    // without touching a glyph, which a crowded view cannot afford per frame.
    let raster_tier = sprites_allowed.then(|| zoom_tier.unwrap_or(FINEST_SPRITE_TIER));
    let mut sprite = None;
    let mut sprite_rasterizing = false;
    let mut deferred_sprite_source = false;
    let previous_tier = canvas_node.last_ready_sprite_tier.load(Ordering::Relaxed) as usize;
    if let Some(raster_tier) = raster_tier {
        let fallback_tiers = [raster_tier, previous_tier];
        if phase.settled {
            // A cold pinch must not enqueue hundreds of SVG raster jobs for
            // tiers the user is only passing through, so rasterization waits
            // for the settle; the settle task requests one redraw.
            let source = if canvas_node.sprite_image_is_initialized(raster_tier) {
                canvas_node.sprite_image(raster_tier)
            } else if *sprite_source_budget > 0 {
                *sprite_source_budget -= 1;
                canvas_node.sprite_image(raster_tier)
            } else {
                deferred_sprite_source = true;
                None
            };
            let rendered = source.and_then(|image| {
                sprite_cache.update(cx, |cache, cx| cache.load(image, window, cx))
            });
            if rendered.is_some() {
                canvas_node
                    .last_ready_sprite_tier
                    .store(raster_tier as u8, Ordering::Relaxed);
            } else {
                sprite_rasterizing = !deferred_sprite_source;
            }
            if zoom_tier.is_some() {
                sprite = rendered
                    .or_else(|| ready_sprite(canvas_node, fallback_tiers, sprite_cache, cx));
            }
        } else {
            sprite = ready_sprite(canvas_node, fallback_tiers, sprite_cache, cx);
            // Above the tiers the sprite is scaled up, which blurs its text
            // until the settle repaints it. Glyphs keep their layout at any
            // zoom, so a view sparse enough to paint them every frame stays
            // crisp through the gesture. A card whose images are still
            // decoding keeps its sprite, so nothing blinks in.
            if zoom_tier.is_none()
                && phase.gesture_text
                && sprite.is_some()
                && direct_images_are_decoded(&canvas_node.scene, zoom, image_cache, window, cx)
            {
                sprite = None;
            }
        }
    }
    match sprite {
        Some(sprite) => {
            let _ = window.paint_image(bounds, bounds, px(20. * zoom).into(), sprite, 0, false);
            if zoom_tier.is_none() {
                // The sprite carries the tiny thumbnails scaled well past
                // their size. The display thumbnails were decoded at rest,
                // so lay them over crisp; anything missing shows the sprite's.
                paint_card_scene(
                    frame,
                    &canvas_node.scene,
                    zoom,
                    SceneLayers::ImagesOnly,
                    image_cache,
                    window,
                    cx,
                );
            }
        }
        None => {
            let layers = if sprite_rasterizing && zoom_tier.is_some() {
                SceneLayers::AwaitingSprite
            } else if phase.settled || phase.gesture_text || zoom_tier.is_none() {
                SceneLayers::All
            } else {
                SceneLayers::NoText
            };
            paint_card_scene(
                frame,
                &canvas_node.scene,
                zoom,
                layers,
                image_cache,
                window,
                cx,
            );
        }
    }

    if phase.settled {
        paint_high_resolution_card_images(frame, &canvas_node.scene, zoom, image_cache, window, cx);
    }

    if frame.status_line.is_some()
        && let Some(area) = canvas_node.scene.generating_area
    {
        paint_generating_waves(
            MEDIA_RADIUS * zoom,
            canvas_bounds(
                frame.screen_x + area.x * zoom,
                frame.screen_y + area.y * zoom,
                area.width * zoom,
                area.height * zoom,
            ),
            zoom,
            window,
        );
    }

    if let Some(status_line) = &frame.status_line {
        paint_canvas_text(
            status_line.clone(),
            canvas_bounds(
                frame.screen_x + 14. * zoom,
                frame.screen_y + (canvas_node.scene.height - 42.) * zoom,
                (CARD_WIDTH - 28.) * zoom,
                42. * zoom,
            ),
            CanvasTextStyle {
                font_size: 10.8,
                zoom,
                line_height: 42. * zoom,
                color: theme::dim(),
                align: TextAlign::Left,
            },
            window,
        );
    }

    if frame.targeted {
        paint_target_outline(bounds, zoom, window);
    }
    deferred_sprite_source
}

/// Seconds since the first animated frame; every animation shares this clock
/// so cards generating side by side move in step.
fn animation_seconds() -> f32 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f32()
}

/// Layered, drifting sine waves with a light sweep and a pulsing label, painted
/// live over a running card's empty media area.
fn paint_generating_waves(radius: f32, bounds: Bounds<Pixels>, zoom: f32, window: &mut Window) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if width < 24. || height < 24. {
        return;
    }
    let t = animation_seconds();
    let left = f32::from(bounds.left());
    let top = f32::from(bounds.top());
    let bottom = f32::from(bounds.bottom());
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        // A faint accent glow rising from the bottom.
        window.paint_quad(fill(
            bounds,
            linear_gradient(
                180.,
                linear_color_stop(theme::accent_strong().opacity(0.), 0.2),
                linear_color_stop(theme::accent_strong().opacity(0.16), 1.),
            ),
        ));

        // (baseline, amplitude, wavelength, speed, phase, top alpha), back to front.
        const LAYERS: [(f32, f32, f32, f32, f32, f32); 4] = [
            (0.42, 0.075, 1.15, 0.55, 0.0, 0.10),
            (0.54, 0.065, 0.85, -0.8, 1.7, 0.14),
            (0.66, 0.055, 1.35, 1.05, 3.1, 0.20),
            (0.78, 0.045, 0.7, -1.35, 4.6, 0.28),
        ];
        let steps = ((width / 6.).ceil() as usize).clamp(24, 96);
        for (baseline, amplitude, wavelength, speed, phase, alpha) in LAYERS {
            let base_y = top + height * baseline;
            let amplitude = height * amplitude * (0.85 + 0.15 * (t * 0.7 + phase).sin());
            let lambda = width * wavelength;
            let mut path = PathBuilder::fill();
            path.move_to(point(px(left), px(bottom)));
            for step in 0..=steps {
                let x = left + width * step as f32 / steps as f32;
                let u = (x - left) / lambda * std::f32::consts::TAU;
                let y = base_y
                    + amplitude * (u + t * speed + phase).sin()
                    + amplitude * 0.35 * (u * 2.3 - t * speed * 1.6 + phase).sin();
                path.line_to(point(px(x), px(y)));
            }
            path.line_to(point(px(left + width), px(bottom)));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(
                    path,
                    linear_gradient(
                        180.,
                        linear_color_stop(theme::accent().opacity(alpha), 0.),
                        linear_color_stop(theme::accent_strong().opacity(alpha * 0.25), 1.),
                    ),
                );
            }
        }

        // A soft sheen sweeping across every few seconds.
        let period = 3.2;
        let progress = (t % period) / period;
        let sheen_width = width * 0.45;
        let sheen_x = left - sheen_width + (width + sheen_width * 2.) * progress;
        let half = sheen_width / 2.;
        for (x, from, to) in [(sheen_x, 0., 0.05), (sheen_x + half, 0.05, 0.)] {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(x), bounds.top()),
                    size(px(half), bounds.size.height),
                ),
                linear_gradient(
                    90.,
                    linear_color_stop(gpui::white().opacity(from), 0.),
                    linear_color_stop(gpui::white().opacity(to), 1.),
                ),
            ));
        }

        // "Generating" with three dots that pulse in sequence.
        let label_height = 20. * zoom;
        let label_top = top + height * 0.24;
        paint_canvas_text(
            "Generating".into(),
            canvas_bounds(left, label_top, width, label_height),
            CanvasTextStyle {
                font_size: 12.5,
                zoom,
                line_height: label_height,
                color: theme::ink().opacity(0.75),
                align: TextAlign::Center,
            },
            window,
        );
        let dot = 4. * zoom;
        let gap = 5. * zoom;
        let dots_left = left + width / 2. - (dot * 3. + gap * 2.) / 2.;
        let dots_top = label_top + label_height + 6. * zoom;
        for index in 0..3 {
            let pulse = ((t * 3.2 - index as f32 * 0.6).sin() * 0.5 + 0.5).powf(1.5);
            window.paint_quad(
                fill(
                    Bounds::new(
                        point(px(dots_left + index as f32 * (dot + gap)), px(dots_top)),
                        size(px(dot), px(dot)),
                    ),
                    theme::accent().opacity(0.25 + 0.75 * pulse),
                )
                .corner_radii(px(dot / 2.)),
            );
        }
        paint_rounded_corner_masks(bounds, radius, theme::raised(), window);
    });
}

/// Rounds a rectangular paint by covering its four corners with `color`
/// (the surface behind it), since content masks only clip to rectangles.
fn paint_rounded_corner_masks(
    bounds: Bounds<Pixels>,
    radius: f32,
    color: Hsla,
    window: &mut Window,
) {
    if radius < 0.5 {
        return;
    }
    let r = px(radius);
    let (left, right, top, bottom) = (bounds.left(), bounds.right(), bounds.top(), bounds.bottom());
    // (corner, point along the horizontal edge, point along the vertical edge)
    let corners = [
        (point(left, top), point(left + r, top), point(left, top + r)),
        (
            point(right, top),
            point(right - r, top),
            point(right, top + r),
        ),
        (
            point(left, bottom),
            point(left + r, bottom),
            point(left, bottom - r),
        ),
        (
            point(right, bottom),
            point(right - r, bottom),
            point(right, bottom - r),
        ),
    ];
    // Control points 0.552r from the edge points approximate a quarter circle.
    const K: f32 = 0.552;
    for (corner, horizontal, vertical) in corners {
        let mut path = PathBuilder::fill();
        path.move_to(corner);
        path.line_to(horizontal);
        path.cubic_bezier_to(
            vertical,
            horizontal + (corner - horizontal) * K,
            vertical + (corner - vertical) * K,
        );
        path.close();
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }
}

/// The smallest sprite tier that still covers the card's on-screen width, or
/// `None` above the last tier, where cards are painted directly.
fn sprite_tier_for_zoom(zoom: f32) -> Option<usize> {
    CARD_SPRITE_WIDTHS
        .iter()
        .position(|width| zoom * CARD_WIDTH <= *width)
}

fn paint_target_outline(bounds: Bounds<Pixels>, zoom: f32, window: &mut Window) {
    window.paint_quad(quad(
        bounds,
        px(20. * zoom),
        gpui::transparent_black(),
        px(zoom.max(0.75)),
        theme::accent(),
        BorderStyle::Solid,
    ));
}

pub struct ToolbarButtonPaint {
    pub bounds: Bounds<Pixels>,
    pub hint: SharedString,
    pub icon: Icon,
    pub color: gpui::Hsla,
    pub hovered: bool,
    /// Entrance progress from 0 (hidden) to 1 (settled).
    pub appear: f32,
}

fn ease_out_cubic(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

/// Ease-out with a small overshoot, for a playful pop.
fn ease_out_back(t: f32) -> f32 {
    const C1: f32 = 1.70158;
    const C3: f32 = C1 + 1.;
    1. + C3 * (t - 1.).powi(3) + C1 * (t - 1.).powi(2)
}

/// Draws the hovered card's action bar in the same paint pass as the cards
/// themselves. Painting it (rather than laying it out as elements) keeps it
/// glued to the card during zoom: element layout re-rounds fractional sizes
/// every frame and shimmers, painted quads and glyphs do not.
pub fn paint_node_toolbar(
    buttons: &[ToolbarButtonPaint],
    zoom: f32,
    window: &mut Window,
    cx: &App,
) {
    let (Some(first), Some(last)) = (buttons.first(), buttons.last()) else {
        return;
    };
    // The bar fades in and rises a few pixels, led by the first button.
    let bar_progress = ease_out_cubic((first.appear * 1.6).min(1.));
    let rise = px((1. - bar_progress) * 6. * zoom);
    let pad = px(3. * zoom);
    let bar = Bounds::from_corners(
        point(first.bounds.left() - pad, first.bounds.top() - pad + rise),
        point(last.bounds.right() + pad, last.bounds.bottom() + pad + rise),
    );
    // A soft drop shadow lifts the bar off the canvas.
    window.paint_quad(
        fill(
            bar + point(px(0.), px(2. * zoom)),
            theme::background().opacity(0.6 * bar_progress),
        )
        .corner_radii(px(10. * zoom)),
    );
    window.paint_quad(quad(
        bar,
        px(10. * zoom),
        theme::raised().opacity(bar_progress),
        px(zoom),
        theme::line().opacity(bar_progress),
        BorderStyle::Solid,
    ));
    for button in buttons {
        let fade = ease_out_cubic(button.appear);
        let bounds = button.bounds + point(px(0.), rise);
        if button.hovered {
            window
                .paint_quad(fill(bounds, theme::hover().opacity(fade)).corner_radii(px(7. * zoom)));
        }
        let icon_size = 15. * zoom * (0.55 + 0.45 * ease_out_back(button.appear));
        let center = bounds.center();
        let icon_bounds = Bounds::new(
            point(center.x - px(icon_size / 2.), center.y - px(icon_size / 2.)),
            size(px(icon_size), px(icon_size)),
        );
        let color = if button.hovered {
            button.color
        } else {
            button.color.opacity(0.85)
        };
        let color = color.opacity(fade);
        let _ = window.paint_svg(
            icon_bounds,
            button.icon.path().into(),
            None,
            TransformationMatrix::unit(),
            color,
            cx,
        );
    }
    if let Some(hovered) = buttons.iter().find(|button| button.hovered) {
        paint_toolbar_hint(hovered, bar, zoom, window);
    }
    if buttons.iter().any(|button| button.appear < 1.) {
        window.request_animation_frame();
    }
}

/// The hovered action's name in a small label just above the bar.
fn paint_toolbar_hint(
    button: &ToolbarButtonPaint,
    bar: Bounds<Pixels>,
    zoom: f32,
    window: &mut Window,
) {
    let width = (button.hint.chars().count() as f32 * 6.3 + 18.) * zoom;
    let height = 22. * zoom;
    let center_x = f32::from(button.bounds.center().x);
    // Keep the label inside the bar's horizontal span so it never drifts far
    // from the card at the edges.
    let left = (center_x - width / 2.)
        .min(f32::from(bar.right()) - width)
        .max(f32::from(bar.left()));
    let bounds = Bounds::new(
        point(px(left), bar.top() - px(height + 5. * zoom)),
        size(px(width), px(height)),
    );
    window.paint_quad(quad(
        bounds,
        px(6. * zoom),
        theme::hover(),
        px(zoom),
        theme::line(),
        BorderStyle::Solid,
    ));
    paint_canvas_text(
        button.hint.clone(),
        bounds,
        CanvasTextStyle {
            font_size: 10.5,
            zoom,
            line_height: height,
            color: theme::ink(),
            align: TextAlign::Center,
        },
        window,
    );
}

fn paint_card_scene(
    frame: &CanvasNodeFrame,
    scene: &CardScene,
    zoom: f32,
    layers: SceneLayers,
    image_cache: &Entity<DecodedImageCache>,
    window: &mut Window,
    cx: &mut App,
) {
    let card_bounds = canvas_bounds(
        frame.screen_x,
        frame.screen_y,
        CARD_WIDTH * zoom,
        frame.height,
    );
    let scale_factor = window.scale_factor();
    window.with_content_mask(
        Some(ContentMask {
            bounds: card_bounds,
        }),
        |window| {
            for primitive in &scene.primitives {
                let skipped = match primitive {
                    CardPrimitive::Text { .. } => {
                        matches!(layers, SceneLayers::NoText | SceneLayers::ImagesOnly)
                    }
                    CardPrimitive::Quad { .. } => layers == SceneLayers::ImagesOnly,
                    CardPrimitive::Image { .. } => false,
                };
                if skipped {
                    continue;
                }
                match primitive {
                    CardPrimitive::Quad {
                        bounds,
                        radius,
                        fill,
                        border,
                    } => {
                        let bounds = transform_card_rect(*bounds, frame, zoom);
                        let (border_width, border_color) = border
                            .map(|(width, color)| (width * zoom, color.hsla()))
                            .unwrap_or((0., gpui::transparent_black()));
                        window.paint_quad(quad(
                            bounds,
                            px(radius * zoom),
                            fill.hsla(),
                            px(border_width),
                            border_color,
                            BorderStyle::Solid,
                        ));
                    }
                    CardPrimitive::Text {
                        text,
                        bounds,
                        font_size,
                        line_height,
                        color,
                        align,
                    } => paint_canvas_text(
                        text.clone(),
                        transform_card_rect(*bounds, frame, zoom),
                        CanvasTextStyle {
                            font_size: *font_size,
                            zoom,
                            line_height: line_height * zoom,
                            color: color.hsla(),
                            align: *align,
                        },
                        window,
                    ),
                    CardPrimitive::Image {
                        bounds,
                        fit,
                        radius,
                        blurred,
                        ..
                    } => paint_canvas_image(
                        direct_image_path(primitive, zoom, scale_factor)
                            .expect("an image primitive has a path"),
                        transform_card_rect(*bounds, frame, zoom),
                        CanvasImageStyle {
                            fit: (*fit).into(),
                            corner_radius: radius * zoom,
                            blurred: *blurred,
                            decode_cap: THUMBNAIL_DECODE_CAP,
                            decode_missing: layers != SceneLayers::AwaitingSprite,
                        },
                        image_cache,
                        window,
                        cx,
                    ),
                }
            }
        },
    );
}

/// The file a direct paint shows for an image primitive, or `None` for any
/// other primitive.
///
/// Past the sprite tiers this is the card's resting look, so it earns the
/// thumbnail; the original takes over once even that is too small. Within the
/// tiers direct painting is a transient stand-in until the sprite lands, and
/// the tiny thumbnail avoids re-reading hundreds of MB of sidecars per tier
/// crossing. Small frames (attachment rows, grid cells) never outgrow the tiny
/// thumbnail at all.
fn direct_image_path(
    primitive: &CardPrimitive,
    zoom: f32,
    scale_factor: f32,
) -> Option<&Arc<Path>> {
    let CardPrimitive::Image {
        asset,
        bounds,
        fit,
        blurred,
        ..
    } = primitive
    else {
        return None;
    };
    let past_sprite_tiers = sprite_tier_for_zoom(zoom).is_none();
    Some(
        if past_sprite_tiers
            && (*blurred || !sprite_thumbnail_suffices(*bounds, *fit, zoom, scale_factor))
        {
            &asset.thumbnail
        } else {
            &asset.sprite
        },
    )
}

/// Whether a direct paint of the scene would show every image right away.
fn direct_images_are_decoded(
    scene: &CardScene,
    zoom: f32,
    image_cache: &Entity<DecodedImageCache>,
    window: &Window,
    cx: &App,
) -> bool {
    let scale_factor = window.scale_factor();
    let image_cache = image_cache.read(cx);
    scene
        .primitives
        .iter()
        .filter_map(|primitive| direct_image_path(primitive, zoom, scale_factor))
        .all(|path| image_cache.is_decoded(&Resource::Path(path.clone()), THUMBNAIL_DECODE_CAP))
}

fn paint_high_resolution_card_images(
    frame: &CanvasNodeFrame,
    scene: &CardScene,
    zoom: f32,
    image_cache: &Entity<DecodedImageCache>,
    window: &mut Window,
    cx: &mut App,
) {
    let scale_factor = window.scale_factor();
    if scene.max_high_resolution_dimension * zoom * scale_factor <= THUMBNAIL_DECODE_CAP as f32 {
        return;
    }
    for primitive in &scene.primitives {
        let CardPrimitive::Image {
            asset,
            bounds,
            fit,
            radius,
            blurred,
        } = primitive
        else {
            continue;
        };
        // A blurred stand-in never benefits from more pixels.
        if *blurred
            || asset.original.as_ref() == asset.thumbnail.as_ref()
            || !image_needs_high_resolution(*bounds, zoom, scale_factor)
        {
            continue;
        }
        paint_canvas_image(
            &asset.original,
            transform_card_rect(*bounds, frame, zoom),
            CanvasImageStyle {
                fit: (*fit).into(),
                corner_radius: radius * zoom,
                blurred: false,
                decode_cap: DECODED_LONG_EDGE_CAP,
                decode_missing: true,
            },
            image_cache,
            window,
            cx,
        );
    }
}

/// Whether the tiny `s_` thumbnail already holds every pixel this frame shows,
/// which makes the display thumbnail's four-times-larger decode pure waste.
fn sprite_thumbnail_suffices(
    bounds: CardRect,
    fit: CardImageFit,
    zoom: f32,
    scale_factor: f32,
) -> bool {
    // A cover-fit image overflows its frame along one axis; this leaves room
    // for a 2:1 source before it would be upscaled.
    let overflow = match fit {
        CardImageFit::Contain => 1.,
        CardImageFit::Cover => 2.,
    };
    bounds.width.max(bounds.height) * zoom * scale_factor * overflow
        <= crate::storage::SPRITE_THUMBNAIL_MAX_DIMENSION as f32
}

/// Whether the thumbnail decode would be upscaled on screen, so the original
/// should be painted over it.
fn image_needs_high_resolution(bounds: CardRect, zoom: f32, scale_factor: f32) -> bool {
    bounds.width.max(bounds.height) * zoom * scale_factor > THUMBNAIL_DECODE_CAP as f32
}

fn transform_card_rect(bounds: CardRect, frame: &CanvasNodeFrame, zoom: f32) -> Bounds<Pixels> {
    canvas_bounds(
        frame.screen_x + bounds.x * zoom,
        frame.screen_y + bounds.y * zoom,
        bounds.width * zoom,
        bounds.height * zoom,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        CARD_WIDTH, CanvasConnector, CardRect, ConnectorStyle, GRID_COLOR_BGRA, GRID_GAP,
        GRID_MIN_SCREEN_GAP, GRID_TEXTURE_SCALE, GRID_TILE_SIZE, canvas_bounds, dot_grid_metrics,
        dot_grid_texture_pixels, edge_is_visible, image_needs_high_resolution, rect_is_visible,
        sprite_tier_for_zoom, trace_connector_dashes,
    };
    use gpui::{point, px};

    #[test]
    fn viewport_culling_keeps_intersecting_cards_and_rejects_distant_ones() {
        assert!(rect_is_visible(-40., 40., 100., 100., 800., 600., 16.));
        assert!(rect_is_visible(790., 590., 30., 30., 800., 600., 16.));
        assert!(!rect_is_visible(-200., 40., 100., 100., 800., 600., 16.));
        assert!(!rect_is_visible(900., 40., 30., 30., 800., 600., 16.));
    }

    #[test]
    fn sprite_tiers_cover_far_out_zoom_and_stop_at_half_scale() {
        assert_eq!(sprite_tier_for_zoom(0.08), Some(0));
        assert_eq!(sprite_tier_for_zoom(0.125), Some(0));
        assert_eq!(sprite_tier_for_zoom(0.2), Some(1));
        assert_eq!(sprite_tier_for_zoom(0.5), Some(2));
        assert_eq!(sprite_tier_for_zoom(0.51), None);
        assert_eq!(sprite_tier_for_zoom(1.), None);
    }

    #[test]
    fn edge_culling_uses_the_full_connector_bounds() {
        assert!(edge_is_visible(
            point(px(-100.), px(300.)),
            point(px(900.), px(300.)),
            800.,
            600.,
            16.,
        ));
        assert!(!edge_is_visible(
            point(px(-200.), px(-100.)),
            point(px(-100.), px(-40.)),
            800.,
            600.,
            16.,
        ));
    }

    #[test]
    fn connector_geometry_is_transformed_from_cached_world_space() {
        let connector = CanvasConnector {
            from_x: 20.,
            from_y: 40.,
            to_x: 100.,
            to_y: 140.,
            merged: false,
        };

        assert_eq!(
            connector.screen_points(10., -5., 0.5),
            (point(px(20.), px(15.)), point(px(60.), px(65.)))
        );
    }

    #[test]
    fn connector_style_scales_with_canvas_zoom() {
        assert_eq!(
            ConnectorStyle::for_zoom(1.),
            ConnectorStyle {
                stroke_width: 1.6,
                dash_length: 7.,
                gap_length: 5.,
            }
        );
        assert_eq!(
            ConnectorStyle::for_zoom(0.25),
            ConnectorStyle {
                stroke_width: 0.55,
                dash_length: 2.5,
                gap_length: 1.75,
            }
        );
    }

    #[test]
    fn connector_dash_remains_continuous_around_elbows() {
        let style = ConnectorStyle::for_zoom(1.);
        let mut dashes = Vec::new();
        trace_connector_dashes(
            point(px(10.), px(10.)),
            point(px(14.), px(18.)),
            style,
            800.,
            600.,
            |dash| dashes.push(dash),
        );

        // The 7px dash covers the 4px first leg and 3px of the elbow; the
        // 5px gap swallows the rest.
        assert_eq!(
            dashes,
            vec![
                canvas_bounds(9.2, 10., 1.6, 4.),
                canvas_bounds(10., 13.2, 3., 1.6),
            ]
        );
    }

    #[test]
    fn connector_dashes_are_only_walked_on_screen() {
        let style = ConnectorStyle::for_zoom(1.);
        let mut dashes = Vec::new();
        trace_connector_dashes(
            point(px(400.), px(-100_000.)),
            point(px(400.), px(100_000.)),
            style,
            800.,
            600.,
            |dash| dashes.push(dash),
        );

        assert!(dashes.len() <= 52, "walked {} dashes", dashes.len());
        // Clipping does not shift the pattern: dashes still start on the 12px
        // period measured from the connector's origin.
        let top = f32::from(dashes[1].origin.y);
        assert!(((top + 100_000.) % 12.).abs() < 0.05);
    }

    #[test]
    fn node_images_promote_to_originals_only_when_thumbnails_are_undersized() {
        let full_card_image = CardRect::new(0., 0., CARD_WIDTH, CARD_WIDTH);
        assert!(!image_needs_high_resolution(full_card_image, 1., 2.));
        assert!(image_needs_high_resolution(full_card_image, 1.05, 2.));

        let half_width_tile = CardRect::new(0., 0., 169., 169.);
        assert!(!image_needs_high_resolution(half_width_tile, 2., 2.));

        let portrait_hero = CardRect::new(0., 0., CARD_WIDTH, CARD_WIDTH * 2.);
        assert!(!image_needs_high_resolution(portrait_hero, 0.5, 2.));
        assert!(image_needs_high_resolution(portrait_hero, 0.55, 2.));
    }

    #[test]
    fn dot_grid_tile_is_world_anchored_at_every_zoom() {
        let grid = dot_grid_metrics(5., -3., 1.);
        assert!((grid.tile_size - GRID_TILE_SIZE).abs() < f32::EPSILON);
        let dot_phase_x = (grid.origin_x + GRID_GAP / 2.).rem_euclid(GRID_GAP);
        let dot_phase_y = (grid.origin_y + GRID_GAP / 2.).rem_euclid(GRID_GAP);
        assert!((dot_phase_x - 5.).abs() < f32::EPSILON);
        assert!((dot_phase_y - 25.).abs() < f32::EPSILON);

        let one_tile_right = dot_grid_metrics(5. + GRID_TILE_SIZE, -3., 1.);
        assert!((one_tile_right.origin_x - grid.origin_x).abs() < f32::EPSILON);

        let distant = dot_grid_metrics(5., -3., 0.08);
        assert!((distant.tile_size - GRID_TILE_SIZE * 16. * 0.08).abs() < 0.001);
        let distant_phase = (distant.origin_x + distant.dot_gap / 2.).rem_euclid(distant.dot_gap);
        assert!((distant_phase - 5_f32.rem_euclid(distant.dot_gap)).abs() < 0.0001);
    }

    #[test]
    fn dot_grid_keeps_a_readable_spacing_and_a_bounded_tile_count_at_every_zoom() {
        let mut zoom = 2.;
        while zoom >= 0.08 {
            let grid = dot_grid_metrics(0., 0., zoom);
            // Never denser than the readable floor, and never coarser than one
            // doubling past it (or the natural world spacing, when zoomed in).
            assert!(
                grid.dot_gap >= GRID_MIN_SCREEN_GAP
                    && grid.dot_gap <= (GRID_GAP * zoom).max(GRID_MIN_SCREEN_GAP * 2.),
                "zoom {zoom} produced a {}px dot spacing",
                grid.dot_gap
            );
            // A 1600x1000 viewport never needs more than a few dozen tiles.
            let tiles = (1600. / grid.tile_size).ceil() * (1000. / grid.tile_size).ceil();
            assert!(tiles <= 64., "zoom {zoom} needed {tiles} grid tiles");
            zoom *= 0.75;
        }
    }

    #[test]
    fn dot_grid_texture_matches_the_web_canvas_contract() {
        let texture = dot_grid_texture_pixels();
        let expected_size = (GRID_TILE_SIZE * GRID_TEXTURE_SCALE as f32) as u32;
        assert_eq!(texture.dimensions(), (expected_size, expected_size));
        assert_eq!(texture.get_pixel(0, 0).0, [0, 0, 0, 0]);

        let first_dot = texture.get_pixel(27, 27).0;
        assert_eq!(&first_dot[..3], &GRID_COLOR_BGRA);
        assert!(first_dot[3] > 200);

        let last_dot_center = ((GRID_TILE_SIZE - GRID_GAP / 2.) * GRID_TEXTURE_SCALE as f32) as u32;
        let last_dot = texture
            .get_pixel(last_dot_center - 1, last_dot_center - 1)
            .0;
        assert_eq!(&last_dot[..3], &GRID_COLOR_BGRA);
        assert!(last_dot[3] > 200);
    }
}
