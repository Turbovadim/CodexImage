//! Removes pixel-phase artifacts from generated images. Raw generations are
//! always kept separately; conditioned copies are used for display and edits.

use anyhow::{Context, Result, bail};
use image::{ColorType, DynamicImage, ImageBuffer, ImageFormat, Rgba, RgbaImage};
use std::borrow::Cow;
use std::env;
use std::f64::consts::{PI, TAU};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

const STRENGTH_ENV: &str = "CODEXIMAGE_REINGEST_CONDITIONING";
const MIN_SIGNAL_RMS: f64 = 0.12;
const MAX_SIGNAL_RMS: f64 = 2.0;
const MAX_PHASE_CORRECTION: f64 = 3.0;
/// The artifact is mostly a 2x2 lattice with weaker 4x4 and 8x8 components.
/// Longer periods exist but are too noisy to measure from a single image.
const PERIOD: usize = 8;
/// Edge length of the square the local artifact strength is measured over.
const TILE: usize = 16;
const MAX_GAIN: f64 = 3.0;
/// A high-pass response beyond this is a hard edge, many times stronger than
/// the artifact. Left in, a few straight edges pass for a pattern.
const MAX_RESIDUAL: f64 = 32.0;
/// Local strength at which the uniform part of the pattern is fully present.
const UNIFORM_FULL_GAIN: f64 = 0.25;

type PhasePattern = [[[f64; 3]; PERIOD]; PERIOD];
type PhaseCounts = [[u32; PERIOD]; PERIOD];
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
    // Already-conditioned images are 16-bit, which the detector never touches;
    // the header says so without decoding the pixels.
    if strength == 0.0 || !is_eight_bit(source) {
        return Ok(false);
    }
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

/// Whether the header reports 8-bit channels. An unreadable header counts as
/// 8-bit so the decode that follows reports the real error.
fn is_eight_bit(source: &Path) -> bool {
    use image::ImageDecoder;
    image::ImageReader::open(source)
        .ok()
        .and_then(|reader| reader.with_guessed_format().ok())
        .and_then(|reader| reader.into_decoder().ok())
        .is_none_or(|decoder| is_eight_bit_color(decoder.color_type()))
}

fn is_eight_bit_color(color: ColorType) -> bool {
    matches!(
        color,
        ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8
    )
}

fn decode_source(source: &Path) -> Result<DynamicImage> {
    image::ImageReader::open(source)
        .with_context(|| format!("failed to open reingestion source {}", source.display()))?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("failed to decode reingestion source {}", source.display()))
}

fn conditioned_image(decoded: &DynamicImage, strength: f64) -> Option<DynamicImage> {
    if strength == 0.0 || !is_eight_bit_color(decoded.color()) {
        return None;
    }

    // Generated PNGs are normally RGBA8. Borrow that buffer for detection so
    // clean images do not pay for a full-resolution copy that is thrown away.
    let image = match decoded {
        DynamicImage::ImageRgba8(image) => Cow::Borrowed(image),
        _ => Cow::Owned(decoded.to_rgba8()),
    };
    let artifact = detect_phase_artifact(&image)?;
    Some(DynamicImage::ImageRgba16(subtract_phase_artifact(
        &image, &artifact, strength,
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

/// The repeating pattern averaged over the whole image, and how strongly each
/// part of the image carries it. The strength grows with local detail and is
/// near zero in flat areas, so a single image-wide subtraction leaves the
/// pattern in busy areas and prints its inverse into flat ones.
struct PhaseArtifact {
    pattern: PatternParts,
    gains: GainField,
}

/// The image-wide pattern, split by how each part behaves across the image.
#[derive(Default)]
struct PatternParts {
    /// The part that follows local detail, as the high-pass measured it. Tiles
    /// are matched against this to find their strength.
    measured: PhasePattern,
    /// The same part at its true size.
    scaled: PhasePattern,
    /// The period-4 part, which is equally strong wherever the artifact is
    /// present at all.
    uniform: PhasePattern,
}

impl PatternParts {
    /// Takes the fold apart by spatial frequency. The 3x3 high-pass passes
    /// only a known fraction of the longer periods, which is divided back out.
    fn split(fold: &PhasePattern) -> Self {
        let mut parts = Self::default();
        let angle = |frequency: (usize, usize), x: usize, y: usize| {
            TAU * ((frequency.0 * x + frequency.1 * y) % PERIOD) as f64 / PERIOD as f64
        };
        let cells = || (0..PERIOD).flat_map(|y| (0..PERIOD).map(move |x| (x, y)));
        let scale = 1.0 / (PERIOD * PERIOD) as f64;
        for frequency in cells().skip(1) {
            let (mut real, mut imaginary) = ([0.0; 3], [0.0; 3]);
            for (x, y) in cells() {
                let (sin, cos) = angle(frequency, x, y).sin_cos();
                for (channel, value) in fold[y][x].iter().enumerate() {
                    real[channel] += value * cos;
                    imaginary[channel] -= value * sin;
                }
            }
            let blurred = |index: usize| (PI * index as f64 / PERIOD as f64).cos().powi(2);
            let passed = 1.0 - blurred(frequency.0) * blurred(frequency.1);
            let repeats_every = |period: usize| {
                frequency.0 % (PERIOD / period) == 0 && frequency.1 % (PERIOD / period) == 0
            };
            let uniform = repeats_every(4) && !repeats_every(2);
            for (x, y) in cells() {
                let (sin, cos) = angle(frequency, x, y).sin_cos();
                for channel in 0..3 {
                    let wave = (real[channel] * cos - imaginary[channel] * sin) * scale;
                    if uniform {
                        parts.uniform[y][x][channel] += wave / passed;
                    } else {
                        parts.measured[y][x][channel] += wave;
                        parts.scaled[y][x][channel] += wave / passed;
                    }
                }
            }
        }
        parts
    }
}

/// One pattern multiplier per `TILE`, bilinearly interpolated between tile
/// centers.
struct GainField {
    columns: usize,
    rows: usize,
    gains: Vec<f64>,
}

impl GainField {
    fn at(&self, x: u32, y: u32) -> f64 {
        let (column, column_weight) = tile_position(x, self.columns);
        let (row, row_weight) = tile_position(y, self.rows);
        let next_column = (column + 1).min(self.columns - 1);
        let next_row = (row + 1).min(self.rows - 1);
        let along_row = |row: usize| {
            let gains = &self.gains[row * self.columns..];
            gains[column] + (gains[next_column] - gains[column]) * column_weight
        };
        let top = along_row(row);
        top + (along_row(next_row) - top) * row_weight
    }
}

fn tile_position(pixel: u32, tiles: usize) -> (usize, f64) {
    let position = ((f64::from(pixel) + 0.5) / TILE as f64 - 0.5).clamp(0.0, (tiles - 1) as f64);
    (position as usize, position.fract())
}

#[derive(Clone, Default)]
struct PhaseSums {
    sums: PhasePattern,
    counts: PhaseCounts,
}

impl PhaseSums {
    /// Per-phase means with each channel's average removed, or `None` when a
    /// phase has no samples.
    fn pattern(&self) -> Option<PhasePattern> {
        if self.counts.iter().flatten().any(|count| *count == 0) {
            return None;
        }
        let mut pattern = self.sums;
        for (phase, count) in pattern
            .iter_mut()
            .flatten()
            .zip(self.counts.iter().flatten())
        {
            *phase = phase.map(|sum| sum / f64::from(*count));
        }
        for channel in 0..3 {
            let mean = pattern
                .iter()
                .flatten()
                .map(|phase| phase[channel])
                .sum::<f64>()
                / (PERIOD * PERIOD) as f64;
            for phase in pattern.iter_mut().flatten() {
                phase[channel] -= mean;
            }
        }
        Some(pattern)
    }

    fn samples(&self) -> f64 {
        f64::from(self.counts.iter().flatten().sum::<u32>())
    }
}

fn dot(left: &PhasePattern, right: &PhasePattern) -> f64 {
    left.iter()
        .flatten()
        .flatten()
        .zip(right.iter().flatten().flatten())
        .map(|(left, right)| left * right)
        .sum()
}

/// Measures the repeating component after a small Gaussian high-pass. Real
/// image structure averages away across the pixel phases; the output artifact
/// remains coherent. Returns `None` unless the image-wide pattern is in the
/// range the artifact produces.
fn detect_phase_artifact(image: &RgbaImage) -> Option<PhaseArtifact> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    if width < 8 || height < 8 {
        return None;
    }

    let columns = width.div_ceil(TILE);
    let rows = height.div_ceil(TILE);
    let mut tiles = vec![PhaseSums::default(); columns * rows];
    let stride = width * 4;
    let raw = image.as_raw();
    for y in 1..height - 1 {
        let [above, row, below] = [y - 1, y, y + 1].map(|y| &raw[y * stride..][..stride]);
        let tile_row = &mut tiles[y / TILE * columns..][..columns];
        for x in 1..width - 1 {
            let tile = &mut tile_row[x / TILE];
            let (phase_y, phase_x) = (y % PERIOD, x % PERIOD);
            let residual = [0, 1, 2].map(|channel| {
                let at = |row: &[u8], x: usize| u32::from(row[x * 4 + channel]);
                let blurred = at(above, x - 1)
                    + 2 * at(above, x)
                    + at(above, x + 1)
                    + 2 * at(row, x - 1)
                    + 4 * at(row, x)
                    + 2 * at(row, x + 1)
                    + at(below, x - 1)
                    + 2 * at(below, x)
                    + at(below, x + 1);
                f64::from(at(row, x)) - f64::from(blurred) / 16.0
            });
            if residual.iter().any(|value| value.abs() > MAX_RESIDUAL) {
                continue;
            }
            for (sum, value) in tile.sums[phase_y][phase_x].iter_mut().zip(residual) {
                *sum += value;
            }
            tile.counts[phase_y][phase_x] += 1;
        }
    }

    let mut total = PhaseSums::default();
    for tile in &tiles {
        for (sum, tile_sum) in total
            .sums
            .iter_mut()
            .flatten()
            .flatten()
            .zip(tile.sums.iter().flatten().flatten())
        {
            *sum += tile_sum;
        }
        for (count, tile_count) in total
            .counts
            .iter_mut()
            .flatten()
            .zip(tile.counts.iter().flatten())
        {
            *count += tile_count;
        }
    }
    let pattern = PatternParts::split(&total.pattern()?);
    let energy = dot(&pattern.measured, &pattern.measured);
    let signal_rms = (energy / (PERIOD * PERIOD * 3) as f64).sqrt();
    if !(MIN_SIGNAL_RMS..=MAX_SIGNAL_RMS).contains(&signal_rms) {
        return None;
    }

    // Each tile's strength is its own pattern projected onto the image-wide
    // one, weighted by sample count so partial edge tiles count for less. A
    // single tile is noisy, so each is averaged with its neighbors.
    let projections: Vec<(f64, f64)> = tiles
        .iter()
        .map(|tile| {
            tile.pattern().map_or((0.0, 0.0), |local| {
                let weight = tile.samples();
                (dot(&local, &pattern.measured) / energy * weight, weight)
            })
        })
        .collect();
    let gains = (0..columns * rows)
        .map(|index| {
            let (row, column) = (index / columns, index % columns);
            let mut projection = 0.0;
            let mut weight = 0.0;
            for near_row in row.saturating_sub(1)..(row + 2).min(rows) {
                for near_column in column.saturating_sub(1)..(column + 2).min(columns) {
                    let kernel = if near_row == row { 2.0 } else { 1.0 }
                        * if near_column == column { 2.0 } else { 1.0 };
                    let (near_projection, near_weight) =
                        projections[near_row * columns + near_column];
                    projection += kernel * near_projection;
                    weight += kernel * near_weight;
                }
            }
            if weight > 0.0 {
                (projection / weight).clamp(0.0, MAX_GAIN)
            } else {
                1.0
            }
        })
        .collect();

    Some(PhaseArtifact {
        pattern,
        gains: GainField {
            columns,
            rows,
            gains,
        },
    })
}

fn subtract_phase_artifact(
    image: &RgbaImage,
    artifact: &PhaseArtifact,
    strength: f64,
) -> Rgba16Image {
    Rgba16Image::from_fn(image.width(), image.height(), |x, y| {
        let source = image.get_pixel(x, y).0;
        let (phase_y, phase_x) = (y as usize % PERIOD, x as usize % PERIOD);
        let scaled = artifact.pattern.scaled[phase_y][phase_x];
        let uniform = artifact.pattern.uniform[phase_y][phase_x];
        let gain = artifact.gains.at(x, y);
        let presence = (gain / UNIFORM_FULL_GAIN).min(1.0);
        let correct = |channel: usize| {
            let correction = (scaled[channel] * gain + uniform[channel] * presence)
                .clamp(-MAX_PHASE_CORRECTION, MAX_PHASE_CORRECTION)
                * strength;
            ((f64::from(source[channel]) - correction).clamp(0.0, 255.0) * 257.0).round() as u16
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
        detect_phase_artifact, subtract_phase_artifact,
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
        assert!(detect_phase_artifact(&image).is_none());
    }

    #[test]
    fn hard_edges_are_not_mistaken_for_the_pattern() {
        // Stripe edges land on every pixel phase, which averages to a pattern.
        let image = RgbaImage::from_fn(256, 256, |x, _| {
            let value = if (x / 37) % 2 == 0 { 30 } else { 220 };
            Rgba([value, value, value, 255])
        });
        assert!(detect_phase_artifact(&image).is_none());
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

    #[test]
    fn conditioning_follows_the_local_pattern_strength() {
        // The right quarter is clean; the rest carries the pattern.
        let image = RgbaImage::from_fn(512, 64, |x, y| {
            let offset = match (x < 384, y % 2, x % 2) {
                (true, 0, 0) => 1,
                (true, 1, 1) => -1,
                _ => 0,
            };
            let value = (128_i16 + offset) as u8;
            Rgba([value, value, value, 255])
        });

        let artifact = detect_phase_artifact(&image).expect("pattern is strong enough");
        let conditioned = subtract_phase_artifact(&image, &artifact, 1.0);

        // Strength is blended across the tiles next to the boundary.
        for (x, _, pixel) in conditioned.enumerate_pixels() {
            if !(360..408).contains(&x) {
                let error = (f64::from(pixel.0[0]) / 257.0 - 128.0).abs();
                assert!(error < 0.05, "x={x} error={error}");
            }
        }
    }
}
