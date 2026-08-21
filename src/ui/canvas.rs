//! Painting the infinite canvas: the dot grid, the dashed parent connectors,
//! and each visible card (as a cached sprite when one is ready).

use super::card::{CanvasNode, CardImageFit, CardPrimitive, CardRect, CardScene};
use super::image_cache::{CardSpriteCache, DecodedImageCache};
use super::theme;
use crate::layout::CARD_WIDTH;
use crate::storage::THUMBNAIL_MAX_DIMENSION;
use gpui::{
    App, BorderStyle, Bounds, ContentMask, Entity, ObjectFit, PathBuilder, Pixels, Point, Resource,
    SharedString, TextAlign, TextRun, Window, point, px, quad, size,
};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

const GRID_GAP: f32 = 28.;
const GRID_DOT_SIZE: f32 = 1.4;
const GRID_TILE_CELLS: usize = 32;
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
const CONNECTOR_STROKE_WIDTH: f32 = 1.6;
const CONNECTOR_DASH_LENGTH: f32 = 7.;
const CONNECTOR_GAP_LENGTH: f32 = 5.;

#[derive(Clone)]
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
    // paint_image clips each tile to `bounds` via its image_bounds intersection,
    // replacing the content mask that used to wrap this loop.
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
enum DashCommand {
    MoveTo(Point<Pixels>),
    LineTo(Point<Pixels>),
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
            stroke_width: CONNECTOR_STROKE_WIDTH * zoom,
            dash_length: CONNECTOR_DASH_LENGTH * zoom,
            gap_length: CONNECTOR_GAP_LENGTH * zoom,
        }
    }
}

/// Strokes every parent → child connector in one dashed path.
pub fn paint_connectors(edges: &[(Point<Pixels>, Point<Pixels>)], zoom: f32, window: &mut Window) {
    if edges.is_empty() {
        return;
    }
    let style = ConnectorStyle::for_zoom(zoom);
    let mut builder = PathBuilder::stroke(px(style.stroke_width));
    for (from, to) in edges {
        append_dashed_connector(&mut builder, *from, *to, style);
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, theme::line());
    }
}

fn append_dashed_connector(
    path: &mut PathBuilder,
    from: Point<Pixels>,
    to: Point<Pixels>,
    style: ConnectorStyle,
) {
    let middle_y = px((f32::from(from.y) + f32::from(to.y)) / 2.);
    let points = [from, point(from.x, middle_y), point(to.x, middle_y), to];
    trace_dashed_polyline(
        &points,
        style.dash_length,
        style.gap_length,
        |command| match command {
            DashCommand::MoveTo(point) => path.move_to(point),
            DashCommand::LineTo(point) => path.line_to(point),
        },
    );
}

fn trace_dashed_polyline(
    points: &[Point<Pixels>],
    dash_length: f32,
    gap_length: f32,
    mut emit: impl FnMut(DashCommand),
) {
    debug_assert!(dash_length > 0. && gap_length > 0.);
    let mut drawing = true;
    let mut remaining = dash_length;
    let mut dash_open = false;

    for segment in points.windows(2) {
        let start_x = f32::from(segment[0].x);
        let start_y = f32::from(segment[0].y);
        let delta_x = f32::from(segment[1].x) - start_x;
        let delta_y = f32::from(segment[1].y) - start_y;
        let length = delta_x.hypot(delta_y);
        if length <= f32::EPSILON {
            continue;
        }
        let direction_x = delta_x / length;
        let direction_y = delta_y / length;
        let mut traveled = 0.;

        while traveled < length {
            let step = remaining.min(length - traveled);
            let fragment_start = point(
                px(start_x + direction_x * traveled),
                px(start_y + direction_y * traveled),
            );
            traveled += step;
            let fragment_end = point(
                px(start_x + direction_x * traveled),
                px(start_y + direction_y * traveled),
            );

            if drawing {
                if !dash_open {
                    emit(DashCommand::MoveTo(fragment_start));
                    dash_open = true;
                }
                emit(DashCommand::LineTo(fragment_end));
            }

            remaining -= step;
            if remaining <= f32::EPSILON {
                if drawing {
                    dash_open = false;
                }
                drawing = !drawing;
                remaining = if drawing { dash_length } else { gap_length };
            }
        }
    }
}

fn canvas_bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
}

#[derive(Clone, Copy)]
struct CanvasTextStyle {
    font_size: f32,
    line_height: f32,
    color: gpui::Hsla,
    align: TextAlign,
}

impl CanvasTextStyle {
    fn new(font_size: f32, line_height: f32, color: gpui::Hsla, align: TextAlign) -> Self {
        Self {
            font_size,
            line_height,
            color,
            align,
        }
    }
}

fn paint_canvas_text(
    text: SharedString,
    bounds: Bounds<Pixels>,
    style: CanvasTextStyle,
    window: &mut Window,
    cx: &mut App,
) {
    // Below ~3 logical px, text is an illegible smear; skipping it spares the
    // shaping pass, which dominates a full direct redraw of a large board at
    // far-out zoom (hundreds of cards, thousands of runs per frame).
    if text.is_empty() || style.font_size < 3. {
        return;
    }
    let run = TextRun {
        len: text.len(),
        color: style.color,
        ..Default::default()
    };
    let line = window
        .text_system()
        .shape_line(text, px(style.font_size), &[run], None);
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let _ = line.paint(
            bounds.origin,
            px(style.line_height),
            style.align,
            Some(bounds.size.width),
            window,
            cx,
        );
    });
}

struct CanvasImageStyle {
    fit: ObjectFit,
    corner_radius: f32,
    blurred: bool,
}

fn paint_canvas_image(
    path: &Arc<Path>,
    bounds: Bounds<Pixels>,
    style: CanvasImageStyle,
    image_cache: &Entity<DecodedImageCache>,
    window: &mut Window,
    cx: &mut App,
) {
    let resource = Resource::Path(path.clone());
    let data = if style.blurred {
        let Some(data) =
            image_cache.update(cx, |cache, cx| cache.load_blurred(&resource, window, cx))
        else {
            return;
        };
        data
    } else {
        let Some(Ok(data)) = image_cache.update(cx, |cache, cx| cache.load(&resource, window, cx))
        else {
            return;
        };
        data
    };
    if data.frame_count() == 0 {
        return;
    }
    let image_bounds = style.fit.get_bounds(bounds, data.size(0));
    // paint_image now clips to bounds ∩ image_bounds itself, so no content mask needed.
    let _ = window.paint_image(
        bounds,
        image_bounds,
        px(style.corner_radius).into(),
        data,
        0,
        false,
    );
}

#[expect(clippy::too_many_arguments)]
pub fn paint_canvas_node(
    frame: &CanvasNodeFrame,
    canvas_node: &CanvasNode,
    zoom: f32,
    zoom_settled: bool,
    image_cache: &Entity<DecodedImageCache>,
    sprite_cache: &Entity<CardSpriteCache>,
    window: &mut Window,
    cx: &mut App,
) {
    let bounds = canvas_bounds(
        frame.screen_x,
        frame.screen_y,
        CARD_WIDTH * zoom,
        frame.height,
    );
    let tier = if zoom <= 0.25 {
        0
    } else if zoom <= 0.5 {
        1
    } else if zoom <= 1. {
        2
    } else {
        3
    };
    // resvg implements large feGaussianBlur filters with quantized box-blur
    // passes, which become visible as contour bands over dark images. Running
    // cards are few and short-lived, so paint their shared CPU-blurred image
    // directly instead of rasterizing the blurred image into a card sprite.
    let has_blurred_images = canvas_node
        .scene
        .primitives
        .iter()
        .any(|primitive| matches!(primitive, CardPrimitive::Image { blurred: true, .. }));
    // While the zoom gesture is still moving, keep blitting whichever tier is
    // already rendered instead of requesting the target tier: every tier
    // crossing otherwise re-rasterizes each visible card's sprite, which is
    // the gesture's dominant CPU and I/O cost. The correct tier is requested
    // once zoom settles.
    let previous_tier = canvas_node.last_ready_sprite_tier.load(Ordering::Relaxed) as usize;
    let mut sprite = if !has_blurred_images
        && !zoom_settled
        && previous_tier < canvas_node.sprite_images.len()
    {
        sprite_cache.update(cx, |cache, _| {
            cache.ready(&canvas_node.sprite_images[previous_tier], window)
        })
    } else {
        None
    };
    if !has_blurred_images && sprite.is_none() {
        sprite = canvas_node.sprite_images.get(tier).and_then(|image| {
            sprite_cache.update(cx, |cache, cx| cache.load(image.clone(), window, cx))
        });
        if sprite.is_some() {
            canvas_node
                .last_ready_sprite_tier
                .store(tier as u8, Ordering::Relaxed);
        } else if previous_tier < canvas_node.sprite_images.len() && previous_tier != tier {
            sprite = sprite_cache.update(cx, |cache, _| {
                cache.ready(&canvas_node.sprite_images[previous_tier], window)
            });
        }
    }
    if let Some(sprite) = sprite {
        let _ = window.paint_image(bounds, bounds, px(20. * zoom).into(), sprite, 0, false);
    } else {
        paint_card_scene(frame, &canvas_node.scene, zoom, image_cache, window, cx);
    }

    paint_high_resolution_card_images(frame, &canvas_node.scene, zoom, image_cache, window, cx);

    if let Some(status_line) = &frame.status_line {
        // A visible running card is the only thing that keeps the canvas
        // repainting: its elapsed-time counter and its shimmer both move every
        // frame. Asking here — rather than from a timer that ran whether or
        // not anything was animating — means an idle app never wakes up, and a
        // card whose media area has not appeared yet still ticks.
        let view = window.current_view();
        window.on_next_frame(move |_, cx| cx.notify(view));

        if let Some(media) = canvas_node.scene.generating_media {
            paint_generating_shimmer(transform_card_rect(media, frame, zoom), window);
        }
        paint_canvas_text(
            status_line.clone(),
            canvas_bounds(
                frame.screen_x + 14. * zoom,
                frame.screen_y + (canvas_node.scene.height - 42.) * zoom,
                (CARD_WIDTH - 28.) * zoom,
                42. * zoom,
            ),
            CanvasTextStyle::new(10.8 * zoom, 42. * zoom, theme::dim(), TextAlign::Left),
            window,
            cx,
        );
    }

    if frame.targeted {
        window.paint_quad(quad(
            bounds,
            px(20. * zoom),
            gpui::transparent_black(),
            px(zoom),
            theme::accent(),
            BorderStyle::Solid,
        ));
    }
}

/// One shimmer cycle across the media area, as in a skeleton placeholder. The
/// phase comes from wall-clock time, so every running card sweeps in unison.
fn paint_generating_shimmer(bounds: Bounds<Pixels>, window: &mut Window) {
    const SWEEP_PERIOD_SECONDS: f32 = 1.8;
    static SHIMMER_EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
    let elapsed = SHIMMER_EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f32();
    let phase = (elapsed / SWEEP_PERIOD_SECONDS).fract();

    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if width <= 0. || height <= 0. {
        return;
    }
    let band_width = width * 0.55;
    let band_left = f32::from(bounds.left()) + phase * (width + band_width) - band_width;
    let top = f32::from(bounds.top());
    let peak = theme::ink().opacity(0.08);
    let edge = theme::ink().opacity(0.);
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let half = band_width / 2.;
        for (offset, from, to) in [(0., edge, peak), (half, peak, edge)] {
            window.paint_quad(quad(
                canvas_bounds(band_left + offset, top, half, height),
                px(0.),
                gpui::linear_gradient(
                    90.,
                    gpui::linear_color_stop(from, 0.),
                    gpui::linear_color_stop(to, 1.),
                ),
                px(0.),
                gpui::transparent_black(),
                BorderStyle::Solid,
            ));
        }
    });
}

pub struct ToolbarButtonPaint {
    pub bounds: Bounds<Pixels>,
    pub label: SharedString,
    pub color: gpui::Hsla,
    pub hovered: bool,
}

/// Draws the hovered card's action buttons in the same paint pass as the cards
/// themselves. Painting them (rather than laying them out as elements) keeps
/// them glued to the card during zoom: element layout re-rounds fractional
/// sizes every frame and shimmers, painted quads and text do not.
pub fn paint_node_toolbar(
    buttons: &[ToolbarButtonPaint],
    zoom: f32,
    window: &mut Window,
    cx: &mut App,
) {
    for button in buttons {
        let (background, border) = if button.hovered {
            (theme::hover(), theme::faint())
        } else {
            (theme::raised().opacity(0.96), theme::line())
        };
        window.paint_quad(quad(
            button.bounds,
            px(5. * zoom),
            background,
            px(zoom),
            border,
            BorderStyle::Solid,
        ));
        paint_canvas_text(
            button.label.clone(),
            button.bounds,
            CanvasTextStyle::new(
                11. * zoom,
                f32::from(button.bounds.size.height),
                button.color,
                TextAlign::Center,
            ),
            window,
            cx,
        );
    }
}

fn paint_card_scene(
    frame: &CanvasNodeFrame,
    scene: &CardScene,
    zoom: f32,
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
    window.with_content_mask(
        Some(ContentMask {
            bounds: card_bounds,
        }),
        |window| {
            for primitive in &scene.primitives {
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
                        CanvasTextStyle::new(
                            font_size * zoom,
                            line_height * zoom,
                            color.hsla(),
                            *align,
                        ),
                        window,
                        cx,
                    ),
                    CardPrimitive::Image {
                        asset,
                        bounds,
                        fit,
                        radius,
                        blurred,
                    } => paint_canvas_image(
                        // Direct painting is a transient stand-in until the
                        // tier's sprite lands, so it always uses the small
                        // thumbnail. Loading the full-size one for those few
                        // frames is what used to re-read hundreds of MB of
                        // sidecars per zoom tier crossing; sharpness at rest
                        // comes from sprites and the high-resolution overlay.
                        &asset.sprite,
                        transform_card_rect(*bounds, frame, zoom),
                        CanvasImageStyle {
                            fit: match fit {
                                CardImageFit::Contain => ObjectFit::Contain,
                                CardImageFit::Cover => ObjectFit::Cover,
                            },
                            corner_radius: radius * zoom,
                            blurred: *blurred,
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

fn paint_high_resolution_card_images(
    frame: &CanvasNodeFrame,
    scene: &CardScene,
    zoom: f32,
    image_cache: &Entity<DecodedImageCache>,
    window: &mut Window,
    cx: &mut App,
) {
    let scale_factor = window.scale_factor();
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
                fit: match fit {
                    CardImageFit::Contain => ObjectFit::Contain,
                    CardImageFit::Cover => ObjectFit::Cover,
                },
                corner_radius: radius * zoom,
                blurred: false,
            },
            image_cache,
            window,
            cx,
        );
    }
}

fn image_needs_high_resolution(bounds: CardRect, zoom: f32, scale_factor: f32) -> bool {
    bounds.width.max(bounds.height) * zoom * scale_factor > THUMBNAIL_MAX_DIMENSION as f32
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
        CARD_WIDTH, CardRect, ConnectorStyle, DashCommand, GRID_COLOR_BGRA, GRID_GAP,
        GRID_MIN_SCREEN_GAP, GRID_TEXTURE_SCALE, GRID_TILE_SIZE, dot_grid_metrics,
        dot_grid_texture_pixels, edge_is_visible, image_needs_high_resolution, rect_is_visible,
        trace_dashed_polyline,
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
                stroke_width: 0.4,
                dash_length: 1.75,
                gap_length: 1.25,
            }
        );
    }

    #[test]
    fn connector_dash_remains_continuous_around_elbows() {
        let points = [
            point(px(0.), px(0.)),
            point(px(0.), px(4.)),
            point(px(4.), px(4.)),
        ];
        let mut commands = Vec::new();
        let style = ConnectorStyle::for_zoom(1.);
        trace_dashed_polyline(&points, style.dash_length, style.gap_length, |command| {
            commands.push(command)
        });

        assert_eq!(
            commands,
            vec![
                DashCommand::MoveTo(point(px(0.), px(0.))),
                DashCommand::LineTo(point(px(0.), px(4.))),
                DashCommand::LineTo(point(px(3.), px(4.))),
            ]
        );
    }
    #[test]
    fn node_images_promote_to_originals_only_when_thumbnails_are_undersized() {
        let full_card_image = CardRect::new(0., 0., CARD_WIDTH, CARD_WIDTH);
        assert!(!image_needs_high_resolution(full_card_image, 1.5, 2.));
        assert!(image_needs_high_resolution(full_card_image, 1.6, 2.));

        let half_width_tile = CardRect::new(0., 0., 169., 169.);
        assert!(!image_needs_high_resolution(half_width_tile, 2., 2.));

        let portrait_hero = CardRect::new(0., 0., CARD_WIDTH, CARD_WIDTH * 2.);
        assert!(!image_needs_high_resolution(portrait_hero, 0.6, 2.));
        assert!(image_needs_high_resolution(portrait_hero, 0.8, 2.));
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
            // A 1600x1000 viewport never needs more than a handful of tiles.
            let tiles = (1600. / grid.tile_size).ceil() * (1000. / grid.tile_size).ceil();
            assert!(tiles <= 12., "zoom {zoom} needed {tiles} grid tiles");
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
