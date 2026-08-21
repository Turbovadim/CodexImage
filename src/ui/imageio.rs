//! Image decoding through macOS ImageIO (`CGImageSource`).
//!
//! This is the fallback decoder for formats the pure-Rust codecs cannot read,
//! HEIC photo attachments above all. It is not the primary path: measured on
//! Apple Silicon, `fdeflate` (PNG) and `zune-jpeg` beat ImageIO once the
//! mandatory CGBitmapContext conversion is included. Multi-frame images
//! (animated GIF/WebP) are declined to keep animation handling in one place.

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_graphics::image::CGImage;
use foreign_types::ForeignType;
use gpui::RenderImage;

type CGImageSourceRef = core_foundation::base::CFTypeRef;

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    static kCGImageSourceCreateThumbnailFromImageAlways: CFStringRef;
    static kCGImageSourceCreateThumbnailWithTransform: CFStringRef;
    static kCGImageSourceThumbnailMaxPixelSize: CFStringRef;

    fn CGImageSourceCreateWithData(
        data: core_foundation::data::CFDataRef,
        options: CFDictionaryRef,
    ) -> CGImageSourceRef;
    fn CGImageSourceGetCount(source: CGImageSourceRef) -> usize;
    fn CGImageSourceCreateImageAtIndex(
        source: CGImageSourceRef,
        index: usize,
        options: CFDictionaryRef,
    ) -> core_graphics::sys::CGImageRef;
    fn CGImageSourceCreateThumbnailAtIndex(
        source: CGImageSourceRef,
        index: usize,
        options: CFDictionaryRef,
    ) -> core_graphics::sys::CGImageRef;
}

// CGBitmapInfo for a memory layout of B, G, R, A: little-endian 32-bit pixels
// with alpha first. Core Graphics only renders premultiplied 8-bit alpha, so
// the buffer is un-premultiplied afterwards to match GPUI's blend mode.
const BITMAP_INFO_BGRA_PREMULTIPLIED: u32 = (2 << 12) | 2;

/// Decodes `bytes` into the straight-alpha BGRA `RenderImage` GPUI paints
/// with. When `max_dimension` is given, ImageIO downscales during the decode
/// so the long edge fits it (it never upscales). Returns `None` whenever this
/// decoder cannot reproduce what GPUI's own loader would (animations, SVG,
/// unknown formats), signalling the caller to fall back.
pub fn decode_render_image(bytes: &[u8], max_dimension: Option<u32>) -> Option<RenderImage> {
    let data = CFData::from_buffer(bytes);
    let source = unsafe {
        let source = CGImageSourceCreateWithData(data.as_concrete_TypeRef(), std::ptr::null());
        if source.is_null() {
            return None;
        }
        CFType::wrap_under_create_rule(source)
    };
    if unsafe { CGImageSourceGetCount(source.as_CFTypeRef()) } != 1 {
        return None;
    }

    let image = match max_dimension {
        Some(max_dimension) => {
            let truthy = CFBoolean::true_value();
            let options = CFDictionary::from_CFType_pairs(&[
                (
                    unsafe {
                        CFString::wrap_under_get_rule(kCGImageSourceCreateThumbnailFromImageAlways)
                    }
                    .as_CFType(),
                    truthy.as_CFType(),
                ),
                (
                    unsafe {
                        CFString::wrap_under_get_rule(kCGImageSourceCreateThumbnailWithTransform)
                    }
                    .as_CFType(),
                    truthy.as_CFType(),
                ),
                (
                    unsafe { CFString::wrap_under_get_rule(kCGImageSourceThumbnailMaxPixelSize) }
                        .as_CFType(),
                    CFNumber::from(max_dimension as i64).as_CFType(),
                ),
            ]);
            unsafe {
                CGImageSourceCreateThumbnailAtIndex(
                    source.as_CFTypeRef(),
                    0,
                    options.as_concrete_TypeRef(),
                )
            }
        }
        None => unsafe {
            CGImageSourceCreateImageAtIndex(source.as_CFTypeRef(), 0, std::ptr::null())
        },
    };
    if image.is_null() {
        return None;
    }
    let image = unsafe { CGImage::from_ptr(image) };

    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 || width > u32::MAX as usize || height > u32::MAX as usize {
        return None;
    }
    let mut context = CGContext::create_bitmap_context(
        None,
        width,
        height,
        8,
        width * 4,
        &CGColorSpace::create_device_rgb(),
        BITMAP_INFO_BGRA_PREMULTIPLIED,
    );
    context.draw_image(
        CGRect::new(
            &CGPoint::new(0., 0.),
            &CGSize::new(width as f64, height as f64),
        ),
        &image,
    );

    let mut pixels = context.data().to_vec();
    unpremultiply(&mut pixels);
    let buffer = image::RgbaImage::from_raw(width as u32, height as u32, pixels)?;
    Some(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// GPUI's sprite pipeline blends with straight alpha, so the premultiplied
/// output of Core Graphics has to be divided back out. Opaque images (the
/// common case here) skip the division entirely.
fn unpremultiply(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        if alpha == 255 || alpha == 0 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::decode_render_image;
    use std::io::Cursor;

    fn encoded_png(image: &image::RgbaImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("encoding a PNG in memory cannot fail");
        bytes
    }

    #[test]
    fn imageio_decodes_at_the_requested_size_and_keeps_bgra_channels() {
        let mut source = image::RgbaImage::new(100, 50);
        for pixel in source.pixels_mut() {
            *pixel = image::Rgba([255, 0, 0, 255]); // opaque red
        }
        let bytes = encoded_png(&source);

        let full = decode_render_image(&bytes, None).expect("full decode");
        assert_eq!((full.size(0).width.0, full.size(0).height.0), (100, 50));

        let capped = decode_render_image(&bytes, Some(32)).expect("capped decode");
        assert_eq!((capped.size(0).width.0, capped.size(0).height.0), (32, 16));
        // Red in BGRA order: blue channel low, red channel high.
        let pixel = &capped.as_bytes(0).expect("frame bytes")[..4];
        assert!(pixel[0] < 8 && pixel[2] > 247 && pixel[3] == 255);
    }

    #[test]
    fn transparency_survives_the_premultiplied_round_trip() {
        let mut source = image::RgbaImage::new(8, 8);
        for pixel in source.pixels_mut() {
            *pixel = image::Rgba([200, 80, 40, 128]);
        }
        let bytes = encoded_png(&source);

        let decoded = decode_render_image(&bytes, None).expect("decode");
        let pixel = &decoded.as_bytes(0).expect("frame bytes")[..4];
        // BGRA order, straight alpha: each channel within rounding distance.
        assert!((pixel[0] as i32 - 40).abs() <= 3, "blue was {}", pixel[0]);
        assert!((pixel[1] as i32 - 80).abs() <= 3, "green was {}", pixel[1]);
        assert!((pixel[2] as i32 - 200).abs() <= 3, "red was {}", pixel[2]);
        assert_eq!(pixel[3], 128);
    }
}
