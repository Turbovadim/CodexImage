//! A disk cache of decoded BGRA pixels, LZ4-compressed.
//!
//! PNG decoding is the app's dominant CPU cost and sits at its format-imposed
//! ceiling (~11 ms per full image). LZ4 decompression of the decoded pixels
//! runs about five times faster, so every image is PNG-decoded once and then
//! served from these sidecars. Entries are regenerable, validated against the
//! source file's size and mtime, and pruned oldest-first past a byte budget.
//! The originals stay untouched, so nothing stored here weakens losslessness.

use gpui::RenderImage;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAGIC: u32 = 0x4247_5241; // "BGRA"
const VERSION: u32 = 1;
const HEADER_LEN: usize = 32;
const BYTE_BUDGET: u64 = 2 * 1024 * 1024 * 1024;
/// How many writes may pass between prune sweeps.
const PRUNE_INTERVAL: u64 = 16;
/// How old an entry's mtime must be before a hit refreshes it.
const TOUCH_GRANULARITY: std::time::Duration = std::time::Duration::from_secs(15 * 60);

static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();
static WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn init(directory: PathBuf) {
    let _ = fs::create_dir_all(&directory);
    let _ = CACHE_DIR.set(directory);
}

/// Returns the cached decode of `source` at the given cap, if it is present
/// and still matches the source file. A hit also refreshes the entry's mtime,
/// which is what the prune sweep orders evictions by.
pub fn load(source: &Path, max_dimension: Option<u32>) -> Option<RenderImage> {
    let entry = entry_path(source, max_dimension)?;
    let bytes = fs::read(&entry).ok()?;
    let parsed = parse(&bytes, source);
    if parsed.is_none() {
        let _ = fs::remove_file(&entry);
        return None;
    }
    // The mtime only orders LRU pruning, so refreshing it coarsely is fine;
    // touching on every hit doubled the syscall traffic of hot zoom paths.
    let now = SystemTime::now();
    let stale = fs::metadata(&entry)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| {
            now.duration_since(modified)
                .is_ok_and(|age| age > TOUCH_GRANULARITY)
        });
    if stale && let Ok(file) = fs::OpenOptions::new().append(true).open(&entry) {
        let _ = file.set_modified(now);
    }
    parsed
}

fn parse(bytes: &[u8], source: &Path) -> Option<RenderImage> {
    let header: [u8; HEADER_LEN] = bytes.get(..HEADER_LEN)?.try_into().ok()?;
    let field = |index: usize| u32::from_le_bytes(header[index..index + 4].try_into().unwrap());
    let wide = |index: usize| u64::from_le_bytes(header[index..index + 8].try_into().unwrap());
    if field(0) != MAGIC || field(4) != VERSION {
        return None;
    }
    let (width, height) = (field(8), field(12));
    if (wide(16), wide(24)) != fingerprint(source)? {
        return None;
    }
    let pixels = lz4_flex::decompress_size_prepended(&bytes[HEADER_LEN..]).ok()?;
    if pixels.len() != width as usize * height as usize * 4 {
        return None;
    }
    let buffer = image::RgbaImage::from_raw(width, height, pixels)?;
    Some(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// Writes the decoded image beside its peers, atomically. Animations are not
/// cached: multi-frame delays are not reachable through `RenderImage`.
pub fn store(source: &Path, max_dimension: Option<u32>, image: &RenderImage) {
    if image.frame_count() != 1 {
        return;
    }
    let Some(entry) = entry_path(source, max_dimension) else {
        return;
    };
    let size = image.size(0);
    let (width, height) = (size.width.0 as u32, size.height.0 as u32);
    let Some(pixels) = image.as_bytes(0) else {
        return;
    };
    let Some((source_len, source_mtime)) = fingerprint(source) else {
        return;
    };

    let compressed = lz4_flex::compress_prepend_size(pixels);
    let mut bytes = Vec::with_capacity(HEADER_LEN + compressed.len());
    bytes.extend_from_slice(&MAGIC.to_le_bytes());
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&source_len.to_le_bytes());
    bytes.extend_from_slice(&source_mtime.to_le_bytes());
    bytes.extend_from_slice(&compressed);

    let write_serial = WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = entry.with_extension(format!("tmp{write_serial}"));
    if fs::write(&temporary, &bytes).is_ok() && fs::rename(&temporary, &entry).is_err() {
        let _ = fs::remove_file(&temporary);
    }
    if write_serial.is_multiple_of(PRUNE_INTERVAL) {
        prune();
    }
}

/// Removes the oldest entries until the cache fits its byte budget again.
fn prune() {
    let Some(directory) = CACHE_DIR.get() else {
        return;
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| {
                (
                    metadata.modified().unwrap_or(UNIX_EPOCH),
                    metadata.len(),
                    entry.path(),
                )
            })
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    if total <= BYTE_BUDGET {
        return;
    }
    files.sort_by_key(|(modified, ..)| *modified);
    for (_, len, path) in files {
        if total <= BYTE_BUDGET {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

fn entry_path(source: &Path, max_dimension: Option<u32>) -> Option<PathBuf> {
    let directory = CACHE_DIR.get()?;
    let mut hash = fnv1a(source.as_os_str().as_encoded_bytes());
    hash ^= fnv1a(&max_dimension.unwrap_or(0).to_le_bytes()).rotate_left(1);
    Some(directory.join(format!("{hash:016x}.bgra.lz4")))
}

fn fingerprint(source: &Path) -> Option<(u64, u64)> {
    let metadata = fs::metadata(source).ok()?;
    let mtime = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some((metadata.len(), mtime))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::{init, load, store};
    use gpui::RenderImage;
    use std::sync::Arc;

    // A single test body: `init` is once-per-process, so splitting scenarios
    // across #[test] functions would race on the cache directory.
    #[test]
    fn round_trips_and_invalidates_when_the_source_changes() {
        let directory = tempfile::tempdir().expect("tempdir");
        init(directory.path().join("cache"));
        let source = directory.path().join("source.png");
        std::fs::write(&source, b"original bytes").expect("write source");

        let mut buffer = image::RgbaImage::new(6, 3);
        for (index, pixel) in buffer.pixels_mut().enumerate() {
            *pixel = image::Rgba([index as u8, 7, 9, 255]);
        }
        let expected = buffer.clone().into_raw();
        let image = Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]));

        assert!(load(&source, Some(2048)).is_none());
        store(&source, Some(2048), &image);

        let cached = load(&source, Some(2048)).expect("cache hit");
        assert_eq!(cached.as_bytes(0).expect("bytes"), expected.as_slice());
        // The two resolution tiers are distinct entries.
        assert!(load(&source, None).is_none());

        std::fs::write(&source, b"changed content!").expect("rewrite source");
        assert!(load(&source, Some(2048)).is_none(), "stale entry must miss");
    }
}
