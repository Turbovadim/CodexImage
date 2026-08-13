//! Removes pixel-phase artifacts from generated images before they are fed
//! back into an edit. Stored originals are never changed.

use anyhow::{Context, Result};
use image::{ColorType, DynamicImage, ImageBuffer, ImageFormat, Rgba, RgbaImage};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const STRENGTH_ENV: &str = "CODEXIMAGE_REINGEST_CONDITIONING";
const MIN_SIGNAL_RMS: f64 = 0.12;
const MAX_SIGNAL_RMS: f64 = 2.0;
const MAX_PHASE_CORRECTION: f64 = 1.5;

type PhasePattern = [[[f64; 3]; 2]; 2];
type Rgba16Image = ImageBuffer<Rgba<u16>, Vec<u16>>;

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
    let decoded = image::ImageReader::open(source)
        .with_context(|| format!("failed to open reingestion source {}", source.display()))?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("failed to decode reingestion source {}", source.display()))?;
    if !matches!(
        decoded.color(),
        ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8
    ) {
        return Ok(false);
    }

    let image = decoded.to_rgba8();
    let Some((pattern, signal_rms)) = estimate_phase_pattern(&image) else {
        return Ok(false);
    };
    if !(MIN_SIGNAL_RMS..=MAX_SIGNAL_RMS).contains(&signal_rms) {
        return Ok(false);
    }

    let conditioned = subtract_phase_pattern(&image, &pattern, strength);
    DynamicImage::ImageRgba16(conditioned)
        .save_with_format(destination, ImageFormat::Png)
        .with_context(|| {
            format!(
                "failed to write conditioned reingestion source {}",
                destination.display()
            )
        })?;
    Ok(true)
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
            let center = image.get_pixel(x, y).0;
            let neighbors = [
                (x - 1, y - 1, 1.0),
                (x, y - 1, 2.0),
                (x + 1, y - 1, 1.0),
                (x - 1, y, 2.0),
                (x, y, 4.0),
                (x + 1, y, 2.0),
                (x - 1, y + 1, 1.0),
                (x, y + 1, 2.0),
                (x + 1, y + 1, 1.0),
            ];
            for channel in 0..3 {
                let blurred = neighbors
                    .iter()
                    .map(|&(neighbor_x, neighbor_y, weight)| {
                        f64::from(image.get_pixel(neighbor_x, neighbor_y).0[channel]) * weight
                    })
                    .sum::<f64>()
                    / 16.0;
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
    Rgba16Image::from_fn(image.width(), image.height(), |x, y| {
        let source = image.get_pixel(x, y).0;
        let phase = pattern[y as usize % 2][x as usize % 2];
        let corrected = std::array::from_fn(|channel| {
            let value = if channel == 3 {
                f64::from(source[channel])
            } else {
                f64::from(source[channel])
                    - phase[channel].clamp(-MAX_PHASE_CORRECTION, MAX_PHASE_CORRECTION) * strength
            };
            (value.clamp(0.0, 255.0) * 257.0).round() as u16
        });
        Rgba(corrected)
    })
}

#[cfg(test)]
mod tests {
    use super::{condition_source, conditioning_strength, estimate_phase_pattern};
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
