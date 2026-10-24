//! Small previews of large images.
//!
//! A wallpaper is 4K or 8K, so decoding one to show it in a list would cost
//! tens of megabytes per row. The decoder here always asks for a thumbnail
//! size, caches the result under the XDG cache directory, and keys the cache on
//! the file's identity and modification time so a replaced image cannot show a
//! stale preview.

use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Longest side of a cached preview, in pixels.
pub const THUMB_SIZE: u32 = 320;

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache")
        })
        .join("niruwallpaper")
        .join("thumbs")
}

/// A cache key that changes when the file's path, size or timestamp changes.
fn key(path: &Path) -> String {
    let meta = fs::metadata(path).ok();
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    if let Some(meta) = meta {
        meta.len().hash(&mut hasher);
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// The cached thumbnail for an image, generating it on first use.
///
/// Returns `None` for a missing or undecodable file: the list then shows a
/// placeholder instead of failing.
pub fn thumbnail(path: &Path) -> Option<PathBuf> {
    let cached = cache_dir().join(format!("{}-{THUMB_SIZE}.png", key(path)));
    if cached.is_file() {
        return Some(cached);
    }
    let image = image::open(path).ok()?;
    // Ask the decoder for the preview size; it does not allocate the full
    // 8K surface just to shrink it afterwards.
    let small = image.thumbnail(THUMB_SIZE, THUMB_SIZE);
    if let Some(dir) = cached.parent() {
        fs::create_dir_all(dir).ok()?;
    }
    // Write beside the target and rename, so a second instance never reads a
    // half-written preview.
    let temp = cached.with_file_name(format!(
        ".{}.{}.tmp.png",
        std::process::id(),
        cached.file_stem()?.to_string_lossy()
    ));
    small.save(&temp).ok()?;
    fs::rename(&temp, &cached).ok()?;
    Some(cached)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nw-thumb-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_CACHE_HOME", dir.join(".cache"));
        dir
    }

    fn write_png(path: &Path, w: u32, h: u32, shade: u8) {
        let mut img = image::RgbaImage::new(w, h);
        for pixel in img.pixels_mut() {
            *pixel = image::Rgba([shade, shade, shade, 255]);
        }
        img.save(path).unwrap();
    }

    #[test]
    fn a_large_image_yields_a_small_preview() {
        let dir = sandbox("large");
        let src = dir.join("big.png");
        write_png(&src, 1920, 1080, 40);
        let thumb = thumbnail(&src).expect("thumbnail");
        let meta = image::image_dimensions(&thumb).unwrap();
        assert!(meta.0 <= THUMB_SIZE && meta.1 <= THUMB_SIZE, "{meta:?}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_preview_is_cached_until_the_image_changes() {
        let dir = sandbox("cached");
        let src = dir.join("wall.png");
        write_png(&src, 640, 360, 10);
        let first = thumbnail(&src).unwrap();
        let stamp = fs::metadata(&first).unwrap().modified().unwrap();
        // A second call must reuse the file rather than regenerate it.
        let second = thumbnail(&src).unwrap();
        assert_eq!(first, second);
        assert_eq!(fs::metadata(&second).unwrap().modified().unwrap(), stamp);

        // Replacing the image must produce a different cache entry.
        write_png(&src, 640, 360, 200);
        let third = thumbnail(&src).unwrap();
        assert_ne!(first, third, "a changed image kept its old preview");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_or_broken_file_has_no_preview() {
        let dir = sandbox("broken");
        assert!(thumbnail(&dir.join("absent.png")).is_none());
        let junk = dir.join("junk.png");
        fs::write(&junk, b"not an image").unwrap();
        assert!(thumbnail(&junk).is_none(), "a broken file must not panic");
        fs::remove_dir_all(&dir).ok();
    }
}
