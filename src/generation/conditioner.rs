//! Removes pixel-phase artifacts from generated images. Raw generations are
//! always kept separately; conditioned copies are used for display and edits.

use anyhow::{Context, Result, bail};
use image::{ColorType, DynamicImage, ImageBuffer, ImageFormat, Rgba, RgbaImage};
use std::borrow::Cow;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

const STRENGTH_ENV: &str = "CODEXIMAGE_REINGEST_CONDITIONING";
const MIN_SIGNAL_RMS: f64 = 0.12;
const MAX_SIGNAL_RMS: f64 = 2.0;
const MAX_PHASE_CORRECTION: f64 = 1.5;

type PhasePattern = [[[f64; 3]; 2]; 2];
type Rgba16Image = ImageBuffer<Rgba<u16>, Vec<u16>>;

pub(crate) fn enabled() -> bool {
    conditioning_strength(env::var(STRENGTH_ENV).ok().as_deref()) > 0.0
}

/// Returns paths suitable for a generation prompt. A source with a measurable
/// 2x2 phase artifact is copied into `directory` as a corrected 16-bit PNG;
/// clean, unsupported, or unreadable sources continue using their originals.
pub fn prepare_source_images(sources: &[PathBuf], directory: &Path) -> Vec<PathBuf> {
    let strength = conditioning_strength(env::var(STRENGTH_ENV).ok().as_deref());
    if strength == 0.0 || sources.is_empty() || fs::create_dir_all(directory).is_err() {
        return sources.to_vec();
    }

    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let destination = directory.join(format!("source-{}.png", index + 1));
            match condition_source(source, &destination, strength) {
                Ok(true) => destination,
                Ok(false) | Err(_) => source.clone(),
            }
        })
        .collect()
}

fn conditioning_strength(value: Option<&str>) -> f64 {
    match value.map(str::trim) {
        Some("") | None => 1.0,
        Some(value) if matches!(value.to_ascii_lowercase().as_str(), "off" | "false") => 0.0,
        Some(value) => value
            .parse::<f64>()
            .map_or(1.0, |value| value.clamp(0.0, 1.0)),
    }
}

fn condition_source(source: &Path, destination: &Path, strength: f64) -> Result<bool> {
    let decoded = decode_source(source)?;
    let Some(conditioned) = conditioned_image(&decoded, strength) else {
        return Ok(false);
    };
    save_png(&conditioned, destination)?;
    Ok(true)
}

/// Writes a conditioned PNG only when the configured artifact detector fires.
/// Storage uses the boolean to preserve clean files byte-for-byte.
pub(crate) fn condition_generated_image(source: &Path, destination: &Path) -> Result<bool> {
    condition_source(
        source,
        destination,
        conditioning_strength(env::var(STRENGTH_ENV).ok().as_deref()),
    )
}

/// Synchronously writes a PNG that is safe to use as a later image input in
/// the same Codex run. A clean source is losslessly converted to PNG so the
/// caller can always use `destination`; generated originals are never changed.
pub fn condition_image_for_reingestion(source: &Path, destination: &Path) -> Result<bool> {
    let decoded = decode_source(source)?;
    let strength = conditioning_strength(env::var(STRENGTH_ENV).ok().as_deref());
    let conditioned = conditioned_image(&decoded, strength);
    let applied = conditioned.is_some();
    save_png(conditioned.as_ref().unwrap_or(&decoded), destination)?;
    Ok(applied)
}

/// Runs the `--condition-image <source> <destination>` command line shared by
/// the app binary and the Windows console helper. Returns `false` when the
/// arguments are not a conditioning invocation.
pub fn run_condition_image_cli(mut arguments: impl Iterator<Item = OsString>) -> Result<bool> {
    if arguments.next().as_deref() != Some(OsStr::new("--condition-image")) {
        return Ok(false);
    }
    let source = PathBuf::from(arguments.next().context("missing source image path")?);
    let destination = PathBuf::from(arguments.next().context("missing output image path")?);
    if arguments.next().is_some() {
        bail!("--condition-image accepts exactly a source and output path");
    }
    let applied = condition_image_for_reingestion(&source, &destination)?;
    println!("{}", if applied { "conditioned" } else { "unchanged" });
    Ok(true)
}

fn decode_source(source: &Path) -> Result<DynamicImage> {
    image::ImageReader::open(source)
        .with_context(|| format!("failed to open reingestion source {}", source.display()))?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("failed to decode reingestion source {}", source.display()))
}

fn conditioned_image(decoded: &DynamicImage, strength: f64) -> Option<DynamicImage> {
    if strength == 0.0
        || !matches!(
            decoded.color(),
            ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8
        )
    {
        return None;
    }

    // Generated PNGs are normally RGBA8. Borrow that buffer for detection so
    // clean images do not pay for a full-resolution copy that is thrown away.
    let image = match decoded {
        DynamicImage::ImageRgba8(image) => Cow::Borrowed(image),
        _ => Cow::Owned(decoded.to_rgba8()),
    };
    let (pattern, signal_rms) = estimate_phase_pattern(&image)?;
    if !(MIN_SIGNAL_RMS..=MAX_SIGNAL_RMS).contains(&signal_rms) {
        return None;
    }
    Some(DynamicImage::ImageRgba16(subtract_phase_pattern(
        &image, &pattern, strength,
    )))
}

fn save_png(image: &DynamicImage, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    image
        .save_with_format(destination, ImageFormat::Png)
        .with_context(|| {
            format!(
                "failed to write conditioned reingestion source {}",
                destination.display()
            )
        })
}

/// Estimates the repeating component after a small Gaussian high-pass. Real
/// image structure averages away across the four pixel phases; the output
/// artifact remains coherent over the full image.
fn estimate_phase_pattern(image: &RgbaImage) -> Option<(PhasePattern, f64)> {
    let (width, height) = image.dimensions();
    if width < 8 || height < 8 {
        return None;
    }

    let mut sums = [[[0.0; 3]; 2]; 2];
    let mut counts = [[0_u64; 2]; 2];
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let phase_y = y as usize % 2;
            let phase_x = x as usize % 2;
            // Load each pixel once. The old per-channel iterator fetched this
            // 3x3 neighborhood three times for every source pixel.
            let top_left = image.get_pixel(x - 1, y - 1).0;
            let top = image.get_pixel(x, y - 1).0;
            let top_right = image.get_pixel(x + 1, y - 1).0;
            let left = image.get_pixel(x - 1, y).0;
            let center = image.get_pixel(x, y).0;
            let right = image.get_pixel(x + 1, y).0;
            let bottom_left = image.get_pixel(x - 1, y + 1).0;
            let bottom = image.get_pixel(x, y + 1).0;
            let bottom_right = image.get_pixel(x + 1, y + 1).0;
            for channel in 0..3 {
                let blurred = f64::from(
                    u32::from(top_left[channel])
                        + 2 * u32::from(top[channel])
                        + u32::from(top_right[channel])
                        + 2 * u32::from(left[channel])
                        + 4 * u32::from(center[channel])
                        + 2 * u32::from(right[channel])
                        + u32::from(bottom_left[channel])
                        + 2 * u32::from(bottom[channel])
                        + u32::from(bottom_right[channel]),
                ) / 16.0;
                sums[phase_y][phase_x][channel] += f64::from(center[channel]) - blurred;
            }
            counts[phase_y][phase_x] += 1;
        }
    }

    let mut pattern = [[[0.0; 3]; 2]; 2];
    let total_count = counts.iter().flatten().sum::<u64>() as f64;
    for channel in 0..3 {
        let global_mean = sums
            .iter()
            .flatten()
            .map(|phase| phase[channel])
            .sum::<f64>()
            / total_count;
        for phase_y in 0..2 {
            for phase_x in 0..2 {
                pattern[phase_y][phase_x][channel] =
                    sums[phase_y][phase_x][channel] / counts[phase_y][phase_x] as f64 - global_mean;
            }
        }
    }

    let signal_rms = (pattern
        .iter()
        .flatten()
        .flat_map(|phase| phase.iter())
        .map(|value| value * value)
        .sum::<f64>()
        / 12.0)
        .sqrt();
    Some((pattern, signal_rms))
}

fn subtract_phase_pattern(image: &RgbaImage, pattern: &PhasePattern, strength: f64) -> Rgba16Image {
    let correction = pattern.map(|row| {
        row.map(|phase| {
            phase.map(|value| value.clamp(-MAX_PHASE_CORRECTION, MAX_PHASE_CORRECTION) * strength)
        })
    });
    Rgba16Image::from_fn(image.width(), image.height(), |x, y| {
        let source = image.get_pixel(x, y).0;
        let phase = correction[y as usize % 2][x as usize % 2];
        let correct = |channel: usize| {
            ((f64::from(source[channel]) - phase[channel]).clamp(0.0, 255.0) * 257.0).round() as u16
        };
        Rgba([
            correct(0),
            correct(1),
            correct(2),
            u16::from(source[3]) * 257,
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::{
        condition_image_for_reingestion, condition_source, conditioning_strength,
        estimate_phase_pattern,
    };
    use image::{DynamicImage, Rgba, RgbaImage};
    use tempfile::TempDir;

    #[test]
    fn strength_defaults_to_on_and_can_be_disabled_or_reduced() {
        assert_eq!(conditioning_strength(None), 1.0);
        assert_eq!(conditioning_strength(Some("off")), 0.0);
        assert_eq!(conditioning_strength(Some("false")), 0.0);
        assert_eq!(conditioning_strength(Some("0")), 0.0);
        assert_eq!(conditioning_strength(Some("0.35")), 0.35);
        assert_eq!(conditioning_strength(Some("2")), 1.0);
        assert_eq!(conditioning_strength(Some("invalid")), 1.0);
    }

    #[test]
    fn phase_estimator_ignores_a_uniform_image() {
        let image = RgbaImage::from_pixel(32, 32, Rgba([80, 120, 160, 255]));
        let (_, signal) = estimate_phase_pattern(&image).expect("large enough image");
        assert!(signal < 1e-9);
    }

    #[test]
    fn synchronous_helper_always_writes_a_valid_png() {
        let directory = TempDir::new().unwrap();
        let source = directory.path().join("clean.jpg");
        let destination = directory.path().join("nested/conditioned.png");
        let image = RgbaImage::from_pixel(32, 32, Rgba([80, 120, 160, 255]));
        DynamicImage::ImageRgba8(image)
            .save_with_format(&source, image::ImageFormat::Jpeg)
            .unwrap();

        assert!(!condition_image_for_reingestion(&source, &destination).unwrap());
        assert_eq!(
            image::ImageFormat::from_path(&destination).unwrap(),
            image::ImageFormat::Png
        );
        assert_eq!(image::image_dimensions(destination).unwrap(), (32, 32));
    }

    #[test]
    fn conditioning_removes_a_repeating_phase_pattern_without_touching_the_original() {
        let directory = TempDir::new().unwrap();
        let source = directory.path().join("source.png");
        let destination = directory.path().join("conditioned.png");
        let image = RgbaImage::from_fn(64, 64, |x, y| {
            let offset = match (y % 2, x % 2) {
                (0, 0) => 1,
                (1, 1) => -1,
                _ => 0,
            };
            Rgba([
                (128_i16 + offset) as u8,
                (96_i16 + offset) as u8,
                (64_i16 + offset) as u8,
                255,
            ])
        });
        DynamicImage::ImageRgba8(image.clone())
            .save(&source)
            .unwrap();

        assert!(condition_source(&source, &destination, 1.0).unwrap());
        assert_eq!(image::open(&source).unwrap().to_rgba8(), image);

        let conditioned = image::open(&destination).unwrap().to_rgba16();
        for pixel in conditioned.pixels() {
            assert!((pixel.0[0] as i32 - 128 * 257).abs() <= 1);
            assert!((pixel.0[1] as i32 - 96 * 257).abs() <= 1);
            assert!((pixel.0[2] as i32 - 64 * 257).abs() <= 1);
            assert_eq!(pixel.0[3], u16::MAX);
        }
    }
}
