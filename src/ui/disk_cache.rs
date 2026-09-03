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
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAGIC: u32 = 0x4247_5241; // "BGRA"
const VERSION: u32 = 1;
const HEADER_LEN: usize = 32;
const BYTE_BUDGET: u64 = 2 * 1024 * 1024 * 1024;
// One pathological native-resolution image must not allocate most of the
// process again while LZ4 is compressing or reading a regenerable sidecar.
const MAX_DECODED_ENTRY_BYTES: usize = 512 * 1024 * 1024;
const MAX_ENCODED_ENTRY_BYTES: u64 = (HEADER_LEN
    + size_of::<u32>()
    + lz4_flex::block::get_maximum_output_size(MAX_DECODED_ENTRY_BYTES))
    as u64;
// A cold board can write hundreds of sidecars. Coalesce that burst into one
// sweep, but still enforce the budget during a sustained stream of writes.
const PRUNE_QUIET_PERIOD: Duration = Duration::from_millis(500);
const PRUNE_MAX_DELAY: Duration = Duration::from_secs(5);
/// How old an entry's mtime must be before a hit refreshes it.
const TOUCH_GRANULARITY: Duration = Duration::from_secs(15 * 60);

static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();
static PRUNE_SENDER: OnceLock<SyncSender<()>> = OnceLock::new();
static WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn init(directory: PathBuf) {
    let _ = fs::create_dir_all(&directory);
    if CACHE_DIR.set(directory).is_err() {
        return;
    }

    // Pruning is app-lifetime maintenance. A dedicated, sleeping worker keeps
    // directory scans off image decode workers and collapses startup bursts.
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name("decoded-cache-pruner".into())
        .spawn(move || prune_worker(receiver))
        .is_ok()
    {
        let _ = PRUNE_SENDER.set(sender);
        request_prune();
    }
}

/// Returns the cached decode of `source` at the given cap, if it is present
/// and still matches the source file. A hit also refreshes the entry's mtime,
/// which is what the prune sweep orders evictions by.
pub fn load(source: &Path, max_dimension: Option<u32>) -> Option<RenderImage> {
    let entry = entry_path(source, max_dimension)?;
    let entry_metadata = fs::metadata(&entry).ok()?;
    if entry_metadata.len() > MAX_ENCODED_ENTRY_BYTES {
        let _ = fs::remove_file(&entry);
        return None;
    }
    let bytes = fs::read(&entry).ok()?;
    let parsed = parse(&bytes, source);
    if parsed.is_none() {
        let _ = fs::remove_file(&entry);
        return None;
    }
    // The mtime only orders LRU pruning, so refreshing it coarsely is fine;
    // touching on every hit doubled the syscall traffic of hot zoom paths.
    let now = SystemTime::now();
    let stale = entry_metadata.modified().is_ok_and(|modified| {
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
    let pixel_len = decoded_len(width, height)?;
    let encoded = bytes.get(HEADER_LEN..)?;
    let stored_len = u32::from_le_bytes(encoded.get(..4)?.try_into().ok()?) as usize;
    if stored_len != pixel_len {
        return None;
    }
    let mut pixels = vec![0; pixel_len];
    let written = lz4_flex::block::decompress_into(&encoded[4..], &mut pixels).ok()?;
    if written != pixel_len {
        return None;
    }
    let buffer = image::RgbaImage::from_raw(width, height, pixels)?;
    Some(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// Writes the decoded image beside its peers, atomically. Animations are not
/// cached: the v1 sidecar format stores only one frame and has no delay table.
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
    if pixels.len() > MAX_DECODED_ENTRY_BYTES || decoded_len(width, height) != Some(pixels.len()) {
        return;
    }
    let Some((source_len, source_mtime)) = fingerprint(source) else {
        return;
    };

    // Compress directly after the header. The previous two-Vec path held two
    // full compressed copies at once and copied every byte between them.
    let compressed_offset = HEADER_LEN + size_of::<u32>();
    let Some(buffer_len) =
        compressed_offset.checked_add(lz4_flex::block::get_maximum_output_size(pixels.len()))
    else {
        return;
    };
    let mut bytes = vec![0; buffer_len];
    bytes[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    bytes[4..8].copy_from_slice(&VERSION.to_le_bytes());
    bytes[8..12].copy_from_slice(&width.to_le_bytes());
    bytes[12..16].copy_from_slice(&height.to_le_bytes());
    bytes[16..24].copy_from_slice(&source_len.to_le_bytes());
    bytes[24..32].copy_from_slice(&source_mtime.to_le_bytes());
    bytes[HEADER_LEN..compressed_offset].copy_from_slice(&(pixels.len() as u32).to_le_bytes());
    let Ok(compressed_len) =
        lz4_flex::block::compress_into(pixels, &mut bytes[compressed_offset..])
    else {
        return;
    };
    bytes.truncate(compressed_offset + compressed_len);

    let write_serial = WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = entry.with_extension(format!("tmp{write_serial}"));
    let stored = match fs::write(&temporary, &bytes) {
        Ok(()) if crate::platform::replace_file(&temporary, &entry).is_ok() => true,
        Ok(()) | Err(_) => {
            // Failed writes are regenerable, but a partial temporary file is
            // otherwise permanent because pruning deliberately ignores it.
            let _ = fs::remove_file(&temporary);
            false
        }
    };
    if stored {
        // The bounded channel and quiet period collapse a cold fill into one
        // scan. Requesting on every successful store also guarantees a short
        // or native-image-heavy burst cannot remain over budget indefinitely.
        request_prune();
    }
}

fn decoded_len(width: u32, height: u32) -> Option<usize> {
    let len = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    (len <= MAX_DECODED_ENTRY_BYTES).then_some(len)
}

fn request_prune() {
    if let Some(sender) = PRUNE_SENDER.get() {
        let _ = sender.try_send(());
    }
}

fn prune_worker(receiver: Receiver<()>) {
    while receiver.recv().is_ok() {
        let started = Instant::now();
        loop {
            if started.elapsed() >= PRUNE_MAX_DELAY {
                break;
            }
            match receiver.recv_timeout(PRUNE_QUIET_PERIOD) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    prune();
                    return;
                }
            }
        }
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
            if !entry.file_name().to_string_lossy().ends_with(".bgra.lz4") {
                return None;
            }
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
    use super::{HEADER_LEN, entry_path, init, load, prune, store};
    use gpui::RenderImage;
    use std::fs;
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

        // A corrupt size prefix used to request an attacker-controlled
        // allocation before the dimensions in our own header were checked.
        let entry = entry_path(&source, Some(2048)).expect("cache path");
        let mut corrupt = fs::read(&entry).expect("cached file");
        corrupt[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(&entry, corrupt).expect("corrupt cache entry");
        assert!(load(&source, Some(2048)).is_none());

        store(&source, Some(2048), &image);
        let temporary = directory.path().join("cache").join("active.tmp7");
        fs::write(&temporary, b"in-flight write").expect("temporary cache file");
        prune();
        assert!(temporary.exists(), "pruning must ignore active writes");

        std::fs::write(&source, b"changed content!").expect("rewrite source");
        assert!(load(&source, Some(2048)).is_none(), "stale entry must miss");
    }
}
