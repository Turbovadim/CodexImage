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
/// Independent groups of tiles must agree on the pattern's shape. A fold of
/// random detail also has energy, but does not repeat across the two groups.
const MIN_PATTERN_CORRELATION: f64 = 0.5;
// Local peaks can exceed the image-wide RMS by several times.
const MAX_PHASE_CORRECTION: f64 = 8.0;
/// The artifact is mostly a 2x2 lattice with weaker 4x4 and 8x8 components.
/// Longer periods exist but are too noisy to measure from a single image.
const PERIOD: usize = 8;
/// Edge length of the square the local artifact strength is measured over.
const TILE: usize = 16;
const MAX_GAIN: f64 = 3.0;
/// A high-pass response beyond this is a hard edge, many times stronger than
/// the artifact. Left in, a few straight edges pass for a pattern.
const MAX_RESIDUAL: f64 = 32.0;

type PhasePattern = [[[f64; 3]; PERIOD]; PERIOD];
type PhaseCounts = [[u32; PERIOD]; PERIOD];
type Rgba16Image = ImageBuffer<Rgba<u16>, Vec<u16>>;

pub(crate) fn enabled() -> bool {
    conditioning_strength(env::var(STRENGTH_ENV).ok().as_deref()) > 0.0
}

/// Returns paths suitable for a generation prompt. A source with a measurable
/// repeating phase artifact is copied into `directory` as a corrected 16-bit PNG;
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
            .ok()
            .filter(|value| value.is_finite())
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
    detail: PatternBand,
    /// The four-pixel component can remain constant as local detail changes,
    /// so it gets its own strength estimate.
    period_four: PatternBand,
}

#[derive(Default)]
struct PatternBand {
    /// High-pass response used to measure local strength.
    measured: PhasePattern,
    /// Pixel correction after undoing the high-pass attenuation.
    scaled: PhasePattern,
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
            let band = if repeats_every(4) && !repeats_every(2) {
                &mut parts.period_four
            } else {
                &mut parts.detail
            };
            for (x, y) in cells() {
                let (sin, cos) = angle(frequency, x, y).sin_cos();
                for channel in 0..3 {
                    let wave = (real[channel] * cos - imaginary[channel] * sin) * scale;
                    band.measured[y][x][channel] += wave;
                    band.scaled[y][x][channel] += wave / passed;
                }
            }
        }
        parts
    }
}

/// A multiplier for each frequency band per `TILE`, bilinearly interpolated
/// between tile centers. Detail comes first, then the four-pixel component.
struct GainField {
    columns: usize,
    rows: usize,
    gains: Vec<[f64; 2]>,
}

impl GainField {
    fn estimate(tiles: &[PhaseSums], columns: usize, rows: usize, pattern: &PatternParts) -> Self {
        let bands = [&pattern.detail.measured, &pattern.period_four.measured];
        let energies = bands.map(|band| dot(band, band));
        let gains = (0..columns * rows)
            .map(|index| {
                let (row, column) = (index / columns, index % columns);
                // Pool samples before taking phase means so incomplete edge
                // tiles contribute without mixing the two frequency bands.
                let mut local = PhaseSums::default();
                for near_row in row.saturating_sub(1)..(row + 2).min(rows) {
                    for near_column in column.saturating_sub(1)..(column + 2).min(columns) {
                        let kernel = if near_row == row { 2 } else { 1 }
                            * if near_column == column { 2 } else { 1 };
                        local.add(&tiles[near_row * columns + near_column], kernel);
                    }
                }
                local.pattern().map_or([0.0; 2], |local| {
                    [0, 1].map(|band| {
                        if energies[band] > f64::EPSILON {
                            (dot(&local, bands[band]) / energies[band]).clamp(0.0, MAX_GAIN)
                        } else {
                            0.0
                        }
                    })
                })
            })
            .collect();
        Self {
            columns,
            rows,
            gains,
        }
    }

    fn at(&self, x: u32, y: u32) -> [f64; 2] {
        let (column, column_weight) = tile_position(x, self.columns);
        let (row, row_weight) = tile_position(y, self.rows);
        let next_column = (column + 1).min(self.columns - 1);
        let next_row = (row + 1).min(self.rows - 1);
        [0, 1].map(|band| {
            let along_row = |row: usize| {
                let gains = &self.gains[row * self.columns..];
                gains[column][band]
                    + (gains[next_column][band] - gains[column][band]) * column_weight
            };
            let top = along_row(row);
            top + (along_row(next_row) - top) * row_weight
        })
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
    fn add(&mut self, other: &Self, weight: u32) {
        for (sum, other_sum) in self
            .sums
            .iter_mut()
            .flatten()
            .flatten()
            .zip(other.sums.iter().flatten().flatten())
        {
            *sum += other_sum * f64::from(weight);
        }
        for (count, other_count) in self
            .counts
            .iter_mut()
            .flatten()
            .zip(other.counts.iter().flatten())
        {
            *count += other_count * weight;
        }
    }

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
}

fn dot(left: &PhasePattern, right: &PhasePattern) -> f64 {
    left.iter()
        .flatten()
        .flatten()
        .zip(right.iter().flatten().flatten())
        .map(|(left, right)| left * right)
        .sum()
}

fn patterns_agree(first: &PhasePattern, second: &PhasePattern) -> bool {
    let energy = dot(first, first) * dot(second, second);
    energy > f64::EPSILON && dot(first, second) / energy.sqrt() >= MIN_PATTERN_CORRELATION
}

/// Measures the repeating component after a small Gaussian high-pass. Real
/// image structure averages away across the pixel phases; the output artifact
/// remains coherent. Independent tile groups must agree, and the image-wide
/// strength must be in the range the artifact produces.
fn detect_phase_artifact(image: &RgbaImage) -> Option<PhaseArtifact> {
    let initial = estimate_phase_artifact(image, None)?;
    // The pattern pushes some real edges across the rejection threshold on
    // one pixel phase more than another, biasing the first estimate. Refit
    // from the original pixels with an edge mask that accounts for it.
    Some(estimate_phase_artifact(image, Some(&initial)).unwrap_or(initial))
}

fn estimate_phase_artifact(
    image: &RgbaImage,
    edge_reference: Option<&PhaseArtifact>,
) -> Option<PhaseArtifact> {
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
            // Hidden RGB and alpha edges are not evidence of a visible pattern.
            if [above, row, below]
                .iter()
                .any(|row| (x - 1..=x + 1).any(|x| row[x * 4 + 3] != 255))
            {
                continue;
            }
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
            let fitted = edge_reference.map_or([0.0; 3], |artifact| {
                let [detail_gain, four_gain] = artifact.gains.at(x as u32, y as u32);
                [0, 1, 2].map(|channel| {
                    artifact.pattern.detail.measured[phase_y][phase_x][channel] * detail_gain
                        + artifact.pattern.period_four.measured[phase_y][phase_x][channel]
                            * four_gain
                })
            });
            if residual
                .iter()
                .zip(fitted)
                .any(|(value, fitted)| (value - fitted).abs() > MAX_RESIDUAL)
            {
                continue;
            }
            for (sum, value) in tile.sums[phase_y][phase_x].iter_mut().zip(residual) {
                *sum += value;
            }
            tile.counts[phase_y][phase_x] += 1;
        }
    }

    let mut groups = [PhaseSums::default(), PhaseSums::default()];
    for (index, tile) in tiles.iter().enumerate() {
        groups[(index / columns + index % columns) % 2].add(tile, 1);
    }
    let (first, second) = (groups[0].pattern()?, groups[1].pattern()?);
    if !patterns_agree(&first, &second) {
        return None;
    }
    let [mut total, other] = groups;
    total.add(&other, 1);
    let fold = total.pattern()?;
    let energy = dot(&fold, &fold);
    let signal_rms = (energy / (PERIOD * PERIOD * 3) as f64).sqrt();
    if !(MIN_SIGNAL_RMS..=MAX_SIGNAL_RMS).contains(&signal_rms) {
        return None;
    }

    let mut pattern = PatternParts::split(&fold);
    let (first, second) = (PatternParts::split(&first), PatternParts::split(&second));
    // A strong band must not authorize subtraction of noise in the other one.
    if !patterns_agree(&first.detail.measured, &second.detail.measured) {
        pattern.detail = PatternBand::default();
    }
    if !patterns_agree(&first.period_four.measured, &second.period_four.measured) {
        pattern.period_four = PatternBand::default();
    }
    let gains = GainField::estimate(&tiles, columns, rows, &pattern);

    Some(PhaseArtifact { pattern, gains })
}

fn subtract_phase_artifact(
    image: &RgbaImage,
    artifact: &PhaseArtifact,
    strength: f64,
) -> Rgba16Image {
    Rgba16Image::from_fn(image.width(), image.height(), |x, y| {
        let source = image.get_pixel(x, y).0;
        if source[3] != 255 {
            return Rgba(source.map(|value| u16::from(value) * 257));
        }
        let (phase_y, phase_x) = (y as usize % PERIOD, x as usize % PERIOD);
        let scaled = artifact.pattern.detail.scaled[phase_y][phase_x];
        let period_four = artifact.pattern.period_four.scaled[phase_y][phase_x];
        let [gain, period_four_gain] = artifact.gains.at(x, y);
        let correct = |channel: usize| {
            let correction = (scaled[channel] * gain + period_four[channel] * period_four_gain)
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
        for value in ["NaN", "inf", "-inf"] {
            assert_eq!(conditioning_strength(Some(value)), 1.0);
        }
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

    fn noise_image(size: u32) -> RgbaImage {
        let mut state = 1337_u32;
        RgbaImage::from_fn(size, size, |_, _| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let value = (112 + state % 33) as u8;
            Rgba([value, value, value, 255])
        })
    }

    #[test]
    fn nonrepeating_detail_is_not_mistaken_for_a_pattern() {
        for size in [32, 64, 128, 256] {
            assert!(
                detect_phase_artifact(&noise_image(size)).is_none(),
                "random detail at {size}x{size}"
            );
        }
    }

    #[test]
    fn conditioning_removes_two_four_and_eight_pixel_patterns() {
        // Odd dimensions and shifted phases also exercise partial edge tiles.
        for (period, (dx, dy)) in [2, 4, 8]
            .into_iter()
            .flat_map(|period| [(1, 0), (0, 1), (1, 1)].map(|direction| (period, direction)))
        {
            let image = RgbaImage::from_fn(97, 83, |x, y| {
                let offset = if ((dx * x + dy * y + 3) % period) < period / 2 {
                    1
                } else {
                    -1
                };
                Rgba([
                    (64 + x as i16 + offset) as u8,
                    (80 + y as i16 - offset) as u8,
                    128,
                    255,
                ])
            });
            let artifact = detect_phase_artifact(&image).expect("repeating pattern");
            for strength in [0.5, 1.0] {
                let conditioned = subtract_phase_artifact(&image, &artifact, strength);
                for (x, y, pixel) in conditioned.enumerate_pixels() {
                    let clean = [64.0 + f64::from(x), 80.0 + f64::from(y), 128.0];
                    for channel in 0..3 {
                        let source = f64::from(image.get_pixel(x, y)[channel]);
                        let expected = source + (clean[channel] - source) * strength;
                        let error = (f64::from(pixel[channel]) / 257.0 - expected).abs();
                        assert!(
                            error < 0.02,
                            "period={period} direction={dx},{dy} x={x} y={y} error={error}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn four_pixel_strength_is_measured_independently() {
        let image = RgbaImage::from_fn(512, 64, |x, _| {
            let detail = if x < 128 { 2 } else { 1 } * if x % 2 == 0 { 1 } else { -1 };
            let four = if x < 256 {
                [1, 0, -1, 0][x as usize % 4]
            } else {
                0
            };
            let value = (128_i16 + detail + four) as u8;
            Rgba([value, value, value, 255])
        });
        let artifact = detect_phase_artifact(&image).expect("repeating pattern");
        let conditioned = subtract_phase_artifact(&image, &artifact, 1.0);
        for (x, _, pixel) in conditioned.enumerate_pixels() {
            if !(104..152).contains(&x) && !(232..280).contains(&x) {
                let error = (f64::from(pixel[0]) / 257.0 - 128.0).abs();
                assert!(error < 0.05, "x={x} error={error}");
            }
        }
    }

    #[test]
    fn local_pattern_peaks_are_not_clipped_to_the_old_correction_limit() {
        let image = RgbaImage::from_fn(512, 128, |x, y| {
            let amplitude = if x < 128 { 4 } else { 1 };
            let offset = if (x + y) % 2 == 0 {
                amplitude
            } else {
                -amplitude
            };
            let value = (128_i16 + offset) as u8;
            Rgba([value, value, value, 255])
        });
        let artifact = detect_phase_artifact(&image).expect("repeating pattern");
        let conditioned = subtract_phase_artifact(&image, &artifact, 1.0);
        for (x, _, pixel) in conditioned.enumerate_pixels() {
            if !(104..152).contains(&x) {
                let error = (f64::from(pixel[0]) / 257.0 - 128.0).abs();
                assert!(error < 0.1, "x={x} error={error}");
            }
        }
    }

    #[test]
    fn hidden_rgb_cannot_trigger_conditioning() {
        let image = RgbaImage::from_fn(128, 128, |x, _| {
            if x < 64 {
                let value = 128 + (x % 2) as u8;
                Rgba([value, value, value, 0])
            } else {
                Rgba([128, 128, 128, 255])
            }
        });
        assert!(detect_phase_artifact(&image).is_none());
    }

    #[test]
    fn correction_preserves_transparent_and_translucent_pixels() {
        let image = RgbaImage::from_fn(128, 128, |x, _| {
            let value = if x % 2 == 0 { 129 } else { 127 };
            let alpha = if x < 32 {
                0
            } else if x < 64 {
                128
            } else {
                255
            };
            Rgba([value, value, value, alpha])
        });
        let artifact = detect_phase_artifact(&image).expect("opaque part carries the pattern");
        let conditioned = subtract_phase_artifact(&image, &artifact, 1.0);
        for (x, y, pixel) in conditioned.enumerate_pixels() {
            let source = image.get_pixel(x, y);
            assert_eq!(pixel[3], u16::from(source[3]) * 257);
            if source[3] < 255 {
                assert_eq!(pixel.0, source.0.map(|value| u16::from(value) * 257));
            } else if x >= 80 {
                assert!((f64::from(pixel[0]) / 257.0 - 128.0).abs() < 0.02);
            }
        }
    }

    #[test]
    fn regions_without_usable_samples_are_not_corrected() {
        let image = RgbaImage::from_fn(256, 128, |x, y| {
            let amplitude = if x < 128 { 1 } else { 80 };
            let offset = if (x + y) % 2 == 0 {
                amplitude
            } else {
                -amplitude
            };
            let value = (128_i16 + offset) as u8;
            Rgba([value, value, value, 255])
        });
        let artifact = detect_phase_artifact(&image).expect("subtle pattern in the left half");
        let conditioned = subtract_phase_artifact(&image, &artifact, 1.0);
        // The strong texture has no residuals that pass the edge threshold.
        for y in 0..image.height() {
            for x in 160..image.width() {
                assert_eq!(
                    conditioned.get_pixel(x, y).0,
                    image.get_pixel(x, y).0.map(|value| u16::from(value) * 257)
                );
            }
        }
    }

    #[test]
    fn conditioning_removes_a_pattern_from_nonrepeating_detail() {
        let clean = noise_image(256);
        let image = RgbaImage::from_fn(256, 256, |x, y| {
            let offset = if x % 2 == 0 { 1 } else { -1 };
            let value = (i16::from(clean.get_pixel(x, y)[0]) + offset) as u8;
            Rgba([value, value, value, 255])
        });
        let artifact = detect_phase_artifact(&image).expect("pattern remains detectable in noise");
        let conditioned = subtract_phase_artifact(&image, &artifact, 1.0);
        let error = conditioned
            .pixels()
            .zip(clean.pixels())
            .map(|(output, clean)| (f64::from(output[0]) / 257.0 - f64::from(clean[0])).powi(2))
            .sum::<f64>();
        let rms = (error / f64::from(image.width() * image.height())).sqrt();
        assert!(rms < 0.4, "remaining error RMS: {rms}");
    }

    #[test]
    fn edge_rejection_does_not_hide_the_injected_lattice() {
        // Nonrepeating detail straddles the edge threshold. Adding the lattice
        // must not selectively reject samples on its positive or negative phase.
        let mut state = 1337_u32;
        let clean = RgbaImage::from_fn(1024, 512, |_, _| {
            let mut value = 66;
            for _ in 0..4 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                value += state % 32;
            }
            Rgba([value as u8, value as u8, value as u8, 255])
        });
        assert!(detect_phase_artifact(&clean).is_none());
        let image = RgbaImage::from_fn(clean.width(), clean.height(), |x, y| {
            let offset = if x % 2 == 0 { 2 } else { -2 };
            let value = (i16::from(clean.get_pixel(x, y)[0]) + offset) as u8;
            Rgba([value, value, value, 255])
        });
        let artifact = detect_phase_artifact(&image).expect("injected two-pixel lattice");
        let conditioned = subtract_phase_artifact(&image, &artifact, 1.0);
        let mut squared_error = 0.0;
        let mut remaining_lattice = 0.0;
        for (x, y, pixel) in conditioned.enumerate_pixels() {
            let error = f64::from(pixel[0]) / 257.0 - f64::from(clean.get_pixel(x, y)[0]);
            squared_error += error * error;
            remaining_lattice += error * if x % 2 == 0 { 1.0 } else { -1.0 };
        }
        let count = f64::from(clean.width()) * f64::from(clean.height());
        let rms = (squared_error / count).sqrt();
        let amplitude = (remaining_lattice / count).abs();
        assert!(rms < 0.5, "remaining error RMS: {rms}");
        assert!(amplitude < 0.1, "remaining lattice amplitude: {amplitude}");
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
        let second_pass = directory.path().join("conditioned-again.png");
        assert!(!condition_source(&destination, &second_pass, 1.0).unwrap());
        assert!(!second_pass.exists());
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
