//! Encodes a card scene as an SVG sprite so the canvas can blit one image per
//! card instead of replaying every primitive at low zoom.

use super::card::CARD_SPRITE_WIDTHS;
use super::card_scene::{CardImageFit, CardPrimitive, CardScene};
use crate::layout::CARD_WIDTH;
use gpui::TextAlign;
use std::fmt::Write as _;

pub fn card_scene_svg(scene: &CardScene, rendered_width: f32) -> String {
    let rendered_height = scene.height * rendered_width / CARD_WIDTH;
    let mut svg = String::with_capacity(scene.primitives.len() * 180);
    write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{rendered_width}\" height=\"{rendered_height}\" viewBox=\"0 0 {CARD_WIDTH} {}\">",
        scene.height
    )
    .expect("writing to a String cannot fail");
    write!(
        svg,
        "<defs><clipPath id=\"card\"><rect x=\"0\" y=\"0\" width=\"{CARD_WIDTH}\" height=\"{}\" rx=\"20\"/></clipPath></defs><g clip-path=\"url(#card)\">",
        scene.height
    )
    .expect("writing to a String cannot fail");
    for (index, primitive) in scene.primitives.iter().enumerate() {
        match primitive {
            CardPrimitive::Quad {
                bounds,
                radius,
                fill,
                border,
            } => {
                let (fill_color, fill_opacity) = fill.svg();
                write!(
                    svg,
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{radius}\" fill=\"{fill_color}\" fill-opacity=\"{fill_opacity}\"",
                    bounds.x, bounds.y, bounds.width, bounds.height
                )
                .expect("writing to a String cannot fail");
                if let Some((width, color)) = border {
                    let (stroke, opacity) = color.svg();
                    write!(
                        svg,
                        " stroke=\"{stroke}\" stroke-opacity=\"{opacity}\" stroke-width=\"{width}\""
                    )
                    .expect("writing to a String cannot fail");
                }
                svg.push_str("/>");
            }
            CardPrimitive::Text {
                text,
                bounds,
                font_size,
                line_height,
                color,
                align,
            } => {
                let clip_id = format!("text-{index}");
                write!(
                    svg,
                    "<clipPath id=\"{clip_id}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></clipPath>",
                    bounds.x, bounds.y, bounds.width, bounds.height
                )
                .expect("writing to a String cannot fail");
                let (anchor, x) = match align {
                    TextAlign::Left => ("start", bounds.x),
                    TextAlign::Center => ("middle", bounds.x + bounds.width / 2.),
                    TextAlign::Right => ("end", bounds.x + bounds.width),
                };
                let baseline = bounds.y + (line_height - font_size) * 0.5 + font_size * 0.82;
                let (fill, opacity) = color.svg();
                write!(
                    svg,
                    "<text x=\"{x}\" y=\"{baseline}\" clip-path=\"url(#{clip_id})\" font-family=\"system-ui,sans-serif\" font-size=\"{font_size}\" font-weight=\"400\" text-anchor=\"{anchor}\" fill=\"{fill}\" fill-opacity=\"{opacity}\">"
                )
                .expect("writing to a String cannot fail");
                push_xml_escaped(&mut svg, text);
                svg.push_str("</text>");
            }
            CardPrimitive::Image {
                asset,
                bounds,
                fit,
                radius,
                blurred,
            } => {
                // Sprites rasterize at 2x their nominal width, so the two
                // smallest tiers (85/170) show the media at ~320 px or less
                // and can embed the tiny thumbnail; the larger tiers need the
                // full-size one to stay sharp near zoom 1.
                let href = if rendered_width <= CARD_SPRITE_WIDTHS[1] {
                    &asset.sprite
                } else {
                    &asset.thumbnail
                };
                if href.as_os_str().is_empty() {
                    continue;
                }
                let mut filter = String::new();
                if *blurred {
                    let filter_id = format!("blur-{index}");
                    write!(
                        svg,
                        "<filter id=\"{filter_id}\" x=\"-20%\" y=\"-20%\" width=\"140%\" height=\"140%\"><feGaussianBlur stdDeviation=\"14\"/></filter>",
                    )
                    .expect("writing to a String cannot fail");
                    filter = format!(" filter=\"url(#{filter_id})\"");
                }
                let clip_id = format!("image-{index}");
                write!(
                    svg,
                    "<clipPath id=\"{clip_id}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{radius}\"/></clipPath><image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" clip-path=\"url(#{clip_id})\"{filter} preserveAspectRatio=\"xMidYMid {}\" href=\"",
                    bounds.x,
                    bounds.y,
                    bounds.width,
                    bounds.height,
                    bounds.x,
                    bounds.y,
                    bounds.width,
                    bounds.height,
                    match fit {
                        CardImageFit::Contain => "meet",
                        CardImageFit::Cover => "slice",
                    }
                )
                .expect("writing to a String cannot fail");
                push_xml_escaped(&mut svg, &href.to_string_lossy());
                svg.push_str("\"/>");
            }
        }
    }
    svg.push_str("</g></svg>");
    svg
}

fn push_xml_escaped(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ => output.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::card_scene_svg;
    use crate::layout::CARD_WIDTH;
    use crate::ui::card::{CARD_SPRITE_WIDTHS, CanvasImageAsset};
    use crate::ui::card_scene::{CardColor, CardImageFit, CardRect, CardScene};
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn sprite_tiers_preserve_one_world_space_scene() {
        let mut scene = CardScene {
            height: 510.,
            primitives: Vec::new(),
            generating_media: None,
        };
        scene.quad(
            CardRect::new(0., 0., CARD_WIDTH, scene.height),
            20.,
            CardColor::Raised,
            Some((1., CardColor::Line)),
        );
        scene.text(
            "A <stable> & exact card",
            CardRect::new(14., 12., CARD_WIDTH - 28., 24.),
            14.,
            18.,
            CardColor::Ink,
            gpui::TextAlign::Left,
        );

        for width in CARD_SPRITE_WIDTHS {
            let svg = card_scene_svg(&scene, width);
            assert!(svg.contains("viewBox=\"0 0 340 510\""));
            assert!(svg.contains("A &lt;stable&gt; &amp; exact card"));
            assert!(svg.contains(&format!("width=\"{width}\"")));
        }
    }

    #[test]
    fn only_blurred_images_receive_a_gaussian_filter() {
        let asset = || CanvasImageAsset {
            original: Arc::from(Path::new("/images/full.png")),
            thumbnail: Arc::from(Path::new("/images/thumb.png")),
            sprite: Arc::from(Path::new("/images/small.png")),
        };
        let bounds = CardRect::new(0., 0., CARD_WIDTH, CARD_WIDTH);
        let mut scene = CardScene {
            height: CARD_WIDTH,
            primitives: Vec::new(),
            generating_media: None,
        };
        scene.image(asset(), bounds, CardImageFit::Contain, 0., false);
        let svg = card_scene_svg(&scene, CARD_WIDTH);
        assert!(!svg.contains("feGaussianBlur"));

        scene.primitives.clear();
        scene.image(asset(), bounds, CardImageFit::Contain, 0., true);
        let svg = card_scene_svg(&scene, CARD_WIDTH);
        assert!(svg.contains("feGaussianBlur"));
        assert!(svg.contains("filter=\"url(#blur-0)\""));

        // Small tiers embed the tiny thumbnail, large tiers the full one.
        for width in CARD_SPRITE_WIDTHS {
            let svg = card_scene_svg(&scene, width);
            let expected = if width <= CARD_SPRITE_WIDTHS[1] {
                "/images/small.png"
            } else {
                "/images/thumb.png"
            };
            assert!(svg.contains(expected), "tier {width} should use {expected}");
        }
    }
}
