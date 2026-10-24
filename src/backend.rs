//! The wallpaper state the window manager already knows.
//!
//! This program owns no settings of its own. It reads what `niructl` reports
//! and asks `niructl` to change things, so the shared config file stays the
//! single source of truth and the GUI can never disagree with the WM about
//! what is actually applied.

use std::{
    path::PathBuf,
    process::{Command, Output},
};

/// One image the bar and the window manager would accept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wallpaper {
    /// Label shown in the list: a theme name or a file name.
    pub name: String,
    pub path: PathBuf,
    /// True when the image came from the theme directory, so it belongs to a
    /// NIRUX theme rather than to the user's own collection.
    pub official: bool,
}

/// What `niructl wallpaper status` reports, in the order it prints it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub enabled: bool,
    pub mode: String,
    pub rotation_minutes: u32,
    pub scale: String,
    pub follow_theme: bool,
    pub file: String,
    pub dir: String,
    pub theme_dir: String,
    pub theme: String,
    /// The image the theme would use now, empty when the theme has none.
    pub theme_image: String,
    /// The window manager's own words for why following is not in effect.
    pub theme_following: String,
}

impl Status {
    /// The file currently shown, as far as the manager can say without a
    /// running window manager to ask.
    pub fn active_image(&self) -> Option<&str> {
        if self.follow_theme && self.mode != "single" && !self.theme_image.is_empty() {
            Some(&self.theme_image)
        } else if !self.file.is_empty() {
            Some(&self.file)
        } else if !self.theme_image.is_empty() {
            Some(&self.theme_image)
        } else {
            None
        }
    }

    /// True when the desktop shows the selected theme's own wallpaper.
    pub fn follows_theme(&self) -> bool {
        self.enabled && self.follow_theme && self.mode != "single" && !self.theme_image.is_empty()
    }

    /// True when the theme has no image and the desktop shows its colour.
    pub fn shows_theme_colour(&self) -> bool {
        self.enabled && self.follow_theme && self.mode != "single" && self.theme_image.is_empty()
    }

    /// True when an explicitly chosen image overrides the theme.
    pub fn has_override(&self) -> bool {
        self.mode == "single" && !self.file.is_empty()
    }
}

/// Find the control command the desktop already installed.
///
/// The installed generation keeps it beside the session, and `~/.local/bin`
/// always carries a symlink; `PATH` is tried last so a hand-run build still
/// works from a checkout.
pub fn find_niructl() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join(".local/share/nirux/current/bin/niructl"));
        candidates.push(home.join(".local/bin/niructl"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join("niructl"));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

pub fn niructl() -> Result<PathBuf, String> {
    find_niructl().ok_or_else(|| {
        "the niructl command was not found; install NIRUX or run it from a NIRUX session".to_string()
    })
}

fn run(args: &[&str]) -> Result<Output, String> {
    let bin = niructl()?;
    let out = Command::new(&bin)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let why = err.lines().next().unwrap_or("no explanation").trim();
        return Err(why.to_string());
    }
    Ok(out)
}

/// Parse `niructl wallpaper status` output.
pub fn parse_status(text: &str) -> Status {
    let mut status = Status {
        mode: "random".into(),
        scale: "fill".into(),
        ..Status::default()
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "enabled" => status.enabled = value == "true",
            "follow_theme" => status.follow_theme = value == "true",
            "mode" => status.mode = value,
            "rotation_minutes" => status.rotation_minutes = value.parse().unwrap_or(0),
            "scale" => status.scale = value,
            "file" => status.file = value,
            "dir" => status.dir = value,
            "theme_dir" => status.theme_dir = value,
            "theme" => status.theme = value,
            // `theme_image` is empty when the theme has no image, so an empty
            // value must not be mistaken for an unset key.
            "theme_image" => status.theme_image = value,
            "theme_following" => status.theme_following = value,
            _ => {}
        }
    }
    status
}

/// Parse `niructl wallpaper list`, whose lines are `name<TAB>path`.
pub fn parse_list(text: &str, theme_dir: &str) -> Vec<Wallpaper> {
    let theme_root = PathBuf::from(expand_home(theme_dir));
    text.lines()
        .filter_map(|line| {
            let (name, path) = line.split_once('\t')?;
            if name.trim().is_empty() || path.trim().is_empty() {
                return None;
            }
            let path = PathBuf::from(path.trim());
            // `niructl` reports the theme directory in its own order first, so
            // membership in that directory is enough to call an entry official.
            let official = path.starts_with(&theme_root);
            Some(Wallpaper {
                name: name.trim().to_string(),
                path,
                official,
            })
        })
        .collect()
}

/// Expand a leading `~/` the way the config file writes paths.
pub fn expand_home(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest).display().to_string();
        }
    }
    value.to_string()
}

pub fn read_status() -> Result<Status, String> {
    let out = run(&["wallpaper", "status"])?;
    Ok(parse_status(&String::from_utf8_lossy(&out.stdout)))
}

pub fn read_list(theme_dir: &str) -> Result<Vec<Wallpaper>, String> {
    let out = run(&["wallpaper", "list"])?;
    Ok(parse_list(&String::from_utf8_lossy(&out.stdout), theme_dir))
}

/// The commands the actions in the window issue.
///
/// Kept together so the window code reads as actions, and so a test can check
/// the exact argument vector without a window manager.
pub fn set_image(path: &str) -> Result<(), String> {
    run(&["wallpaper", "set", path]).map(|_| ())
}

pub fn set_scale(scale: &str) -> Result<(), String> {
    run(&["wallpaper", "scale", scale]).map(|_| ())
}

pub fn set_rotation(minutes: u32) -> Result<(), String> {
    run(&["wallpaper", "rotation", &minutes.to_string()]).map(|_| ())
}

pub fn set_follow_theme(on: bool) -> Result<(), String> {
    run(&["wallpaper", "follow-theme", if on { "on" } else { "off" }]).map(|_| ())
}

/// Theme slugs NIRUX installs, in the order the suite presents them.
pub const THEMES: [(&str, &str); 10] = [
    ("niru-noir", "Noir"),
    ("niru-nord", "Nord"),
    ("niru-satie", "Satie"),
    ("niru-carl-larsson", "Carl Larsson"),
    ("niru-forest-vigil", "Forest Vigil"),
    ("niru-blood-of-urdr", "Blood of Urdr"),
    ("niru-root-and-stone", "Root and Stone"),
    ("niru-blue-horizon", "Blue Horizon"),
    ("niru-the-norns", "The Norns"),
    ("niru-winter-nights", "Winter Nights"),
];

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "enabled=true\nmode=random\nrotation_minutes=10\n\
scale=fill\nfollow_theme=true\nfile=\ndir=~/Pictures/Wallpapers\n\
theme_dir=~/Pictures/Wallpapers/NIRU\ntheme=niru-nord\n\
theme_image=/home/u/Pictures/Wallpapers/NIRU/nirux-wallpaper-nord-4k.png\n\
theme_following=enabled\n";

    #[test]
    fn status_fields_are_read_in_order() {
        let s = parse_status(SAMPLE);
        assert!(s.enabled);
        assert!(s.follow_theme);
        assert_eq!(s.mode, "random");
        assert_eq!(s.rotation_minutes, 10);
        assert_eq!(s.scale, "fill");
        assert_eq!(s.theme, "niru-nord");
        assert!(s.theme_image.ends_with("nirux-wallpaper-nord-4k.png"));
        assert_eq!(s.theme_following, "enabled");
    }

    #[test]
    fn following_the_theme_reports_the_theme_image() {
        let s = parse_status(SAMPLE);
        assert!(s.follows_theme());
        assert!(!s.has_override());
        assert!(s.active_image().unwrap().ends_with("nord-4k.png"));
    }

    #[test]
    fn an_explicit_image_overrides_the_theme() {
        let text = "enabled=true\nmode=single\nfollow_theme=true\nfile=/home/u/mine.png\n\
theme=niru-noir\ntheme_image=/home/u/Pictures/Wallpapers/NIRU/nirux-wallpaper-noir-4k.png\n";
        let s = parse_status(text);
        assert!(s.has_override());
        assert!(!s.follows_theme());
        assert_eq!(s.active_image(), Some("/home/u/mine.png"));
    }

    #[test]
    fn a_theme_without_an_image_shows_its_colour() {
        let text = "enabled=true\nmode=random\nfollow_theme=true\ntheme=niru-satie\n\
theme_image=\ntheme_following=theme-image-missing\n";
        let s = parse_status(text);
        assert!(s.shows_theme_colour());
        assert!(!s.follows_theme());
        assert_eq!(s.active_image(), None);
    }

    #[test]
    fn a_disabled_wallpaper_owner_reports_nothing_active() {
        let text = "enabled=false\nmode=random\nfollow_theme=true\ntheme=niru-noir\n";
        let s = parse_status(text);
        assert!(!s.enabled);
        assert!(!s.follows_theme());
    }

    #[test]
    fn list_lines_split_on_a_tab() {
        let text = "Niru Nord\t/home/u/Pictures/Wallpapers/NIRU/nirux-wallpaper-nord-4k.png\n\
Holiday\t/home/u/Pictures/holiday.png\n\nbroken line without tab\n";
        let dir = "/home/u/Pictures/Wallpapers/NIRU";
        // The test must not depend on the real HOME, so build the root the same
        // way the parser does from the directory it is given.
        let entries = parse_list(text, dir);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "Niru Nord");
        assert_eq!(entries[1].name, "Holiday");
        assert!(entries[0].official == entries[0].path.starts_with(expand_home(dir)));
        assert_eq!(entries[1].path, PathBuf::from("/home/u/Pictures/holiday.png"));
    }

    #[test]
    fn every_installed_theme_has_a_slug_and_a_label() {
        assert_eq!(THEMES.len(), 10);
        for (slug, label) in THEMES {
            assert!(slug.starts_with("niru-"), "{slug} is not a theme slug");
            assert!(!label.is_empty(), "{slug} has no label");
        }
    }
}
