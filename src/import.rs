//! Adding the user's own pictures: a short list of the images in Pictures
//! and Downloads, copied (never moved) into the configured wallpaper
//! directory.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// How many pictures the list offers at most, newest first.
pub const MAX_CANDIDATES: usize = 500;

/// One picture that can be added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    /// The folder it is in, as shown: "Pictures" or "Downloads".
    pub folder: String,
    pub bytes: u64,
    modified: SystemTime,
}

impl Candidate {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

/// A directory from `~/.config/user-dirs.dirs` (`XDG_<NAME>_DIR`), so a
/// localized Pictures or Downloads folder (Bilder, Hämtningar) is found.
pub fn xdg_user_dir(name: &str) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let text = fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    parse_user_dir(&text, name, &home)
}

fn parse_user_dir(text: &str, name: &str, home: &Path) -> Option<PathBuf> {
    let key = format!("XDG_{name}_DIR");
    text.lines().find_map(|line| {
        let line = line.trim();
        let value = line.strip_prefix(&key)?.trim_start().strip_prefix('=')?;
        let value = value.trim().trim_matches('"');
        let path = match value.strip_prefix("$HOME") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None if value.starts_with('/') => PathBuf::from(value),
            None => return None,
        };
        // `$HOME/` alone means "disabled" in user-dirs.dirs.
        (path != home).then_some(path)
    })
}

/// The folders the list is made from: Pictures and Downloads, localized
/// when user-dirs.dirs says so.
pub fn source_dirs() -> Vec<(PathBuf, String)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out = Vec::new();
    for (key, fallback, label) in [
        ("PICTURES", "Pictures", "Pictures"),
        ("DOWNLOAD", "Downloads", "Downloads"),
    ] {
        if let Some(dir) = xdg_user_dir(key).or_else(|| home.as_ref().map(|h| h.join(fallback))) {
            out.push((dir, label.to_string()));
        }
    }
    out
}

/// The file types NIRUWM shows, the same list `niructl` uses.
pub fn is_picture(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "bmp")
    )
}

/// The pictures directly in `dirs` (not in subfolders), newest first.
/// Anything already in `target`, the wallpaper directory, is left out.
pub fn candidates(dirs: &[(PathBuf, String)], target: &Path) -> Vec<Candidate> {
    let target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let mut out = Vec::new();
    for (dir, folder) in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let hidden = path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'));
            if hidden || !is_picture(&path) {
                continue;
            }
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let parent = path
                .parent()
                .and_then(|p| p.canonicalize().ok())
                .unwrap_or_default();
            if parent == target {
                continue;
            }
            out.push(Candidate {
                path,
                folder: folder.clone(),
                bytes: meta.len(),
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.path.cmp(&b.path)));
    out.truncate(MAX_CANDIDATES);
    out
}

/// The name a copy gets in `dir`: the file's own name, or with "-2", "-3"
/// before the extension when that name is taken.
fn numbered(name: &str, n: u32) -> String {
    if n < 2 {
        return name.to_string();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}-{n}.{ext}"),
        _ => format!("{name}-{n}"),
    }
}

/// Copy one picture into `dir`, never replacing a file that is there and
/// never touching the original. Returns where the copy is.
///
/// The bytes go to a hidden temporary file first and are then linked to
/// the final name, which fails instead of overwriting when another program
/// took the name meanwhile.
pub fn copy_into(source: &Path, dir: &Path) -> Result<PathBuf, String> {
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| format!("{} has no file name", source.display()))?;
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let temp = dir.join(format!(".{name}.niruwallpaper-{}.part", std::process::id()));
    if let Err(e) = fs::copy(source, &temp) {
        let _ = fs::remove_file(&temp);
        return Err(format!("cannot copy {name}: {e}"));
    }
    let mut result = Err(format!("no free name for {name}"));
    for n in 1..1000 {
        let target = dir.join(numbered(&name, n));
        match fs::hard_link(&temp, &target) {
            Ok(()) => {
                result = Ok(target);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                result = Err(format!("cannot add {name}: {e}"));
                break;
            }
        }
    }
    let _ = fs::remove_file(&temp);
    result
}

/// A size for the list: "2.4 MB".
pub fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / f64::from(1u32 << 30)),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / f64::from(1u32 << 20)),
        b if b >= 1 << 10 => format!("{} kB", b >> 10),
        b => format!("{b} B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox(PathBuf);

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sandbox(name: &str) -> Sandbox {
        let dir = std::env::temp_dir().join(format!("nw-import-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Sandbox(dir)
    }

    #[test]
    fn user_dirs_are_read_with_home_expanded() {
        let text = "# comment\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\nXDG_DOWNLOAD_DIR=\"$HOME/\"\n";
        let home = Path::new("/home/u");
        assert_eq!(
            parse_user_dir(text, "PICTURES", home),
            Some(PathBuf::from("/home/u/Bilder"))
        );
        assert_eq!(parse_user_dir(text, "DOWNLOAD", home), None, "disabled");
        assert_eq!(parse_user_dir(text, "MUSIC", home), None);
    }

    #[test]
    fn a_copy_never_replaces_a_file_and_keeps_the_original() {
        let sb = sandbox("copy");
        let pictures = sb.0.join("Pictures");
        let walls = sb.0.join("Wallpapers");
        fs::create_dir_all(&pictures).unwrap();
        fs::create_dir_all(&walls).unwrap();
        let source = pictures.join("beach.jpg");
        fs::write(&source, b"new picture").unwrap();
        fs::write(walls.join("beach.jpg"), b"already there").unwrap();
        fs::write(walls.join("beach-2.jpg"), b"also there").unwrap();
        let copy = copy_into(&source, &walls).unwrap();
        assert_eq!(copy, walls.join("beach-3.jpg"));
        assert_eq!(fs::read(&copy).unwrap(), b"new picture");
        assert_eq!(fs::read(walls.join("beach.jpg")).unwrap(), b"already there");
        assert_eq!(fs::read(&source).unwrap(), b"new picture", "never moved");
        // No temporary file is left behind.
        let left: Vec<_> = fs::read_dir(&walls)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        // A missing wallpaper directory is created.
        let fresh = sb.0.join("new/Wallpapers");
        assert_eq!(copy_into(&source, &fresh).unwrap(), fresh.join("beach.jpg"));
        assert_eq!(numbered("archive", 2), "archive-2");
        assert_eq!(numbered(".hidden", 2), ".hidden-2");
    }

    #[test]
    fn the_list_has_pictures_only_and_not_those_already_added() {
        let sb = sandbox("list");
        let pictures = sb.0.join("Pictures");
        let walls = pictures.join("Wallpapers");
        let downloads = sb.0.join("Downloads");
        fs::create_dir_all(&walls).unwrap();
        fs::create_dir_all(downloads.join("sub")).unwrap();
        for file in [
            pictures.join("a.png"),
            pictures.join("notes.txt"),
            pictures.join(".hidden.png"),
            walls.join("already.png"),
            downloads.join("B.JPG"),
            downloads.join("sub/deep.png"),
        ] {
            fs::write(file, b"x").unwrap();
        }
        let list = candidates(
            &[
                (pictures.clone(), "Pictures".into()),
                (downloads.clone(), "Downloads".into()),
                (walls.clone(), "Wallpapers".into()),
            ],
            &walls,
        );
        let mut names: Vec<String> = list.iter().map(Candidate::name).collect();
        names.sort();
        assert_eq!(names, ["B.JPG", "a.png"]);
        assert_eq!(human_size(2_500_000), "2.4 MB");
        assert_eq!(human_size(900), "900 B");
    }
}
