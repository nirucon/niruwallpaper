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

/// One screen and what the configuration says it shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScreenPlan {
    /// RandR output name, e.g. `eDP-1`.
    pub output: String,
    /// `inherit` or `override`.
    pub source: String,
    /// `theme`, `image`, `colour` or `disabled`.
    pub kind: String,
    pub scale: String,
    pub image: String,
}

impl ScreenPlan {
    pub fn overrides(&self) -> bool {
        self.source == "override"
    }
}

/// One screen and what the running window manager actually rendered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScreenRendered {
    pub output: String,
    /// `theme`, `image`, `colour`, `failed` or `off`.
    pub kind: String,
    pub image: String,
    pub scale: String,
    pub source: String,
    pub note: String,
}

impl ScreenRendered {
    pub fn failed(&self) -> bool {
        self.kind == "failed"
    }
}

/// What `niructl wallpaper status` reports. The human-oriented `key=value`
/// lines and the JSON form describe the same configuration; the JSON form also
/// carries the per-screen plan and what was rendered.
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
    /// The plan per connected screen, from the structured report.
    pub screens: Vec<ScreenPlan>,
    /// What the running window manager rendered, when it says anything.
    pub rendered: Vec<ScreenRendered>,
    /// True when the structured report was available.
    pub structured: bool,
    /// Per-screen overrides as stored in the configuration.
    pub overrides: Vec<ScreenOverride>,
}

/// One `[[wallpaper.outputs]]` entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScreenOverride {
    pub output: String,
    pub mode: String,
    pub file: String,
    pub scale: String,
}

impl ScreenOverride {
    /// True when this entry changes nothing and could be removed.
    pub fn inherits(&self) -> bool {
        self.mode != "theme" && self.file.trim().is_empty() && self.scale.trim().is_empty()
    }
}

impl Status {
    /// The file currently shown, as far as the manager can say without a
    /// running window manager to ask.
    pub fn active_image(&self) -> Option<&str> {
        if let Some(first) = self
            .screens
            .iter()
            .filter(|s| s.kind == "image" || s.kind == "theme")
            .map(|s| s.image.as_str())
            .find(|s| !s.is_empty())
        {
            return Some(first);
        }
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

    /// True when a running window manager published what it rendered.
    pub fn has_rendered(&self) -> bool {
        !self.rendered.is_empty()
    }

    /// True when something the user asked for did not reach a screen.
    pub fn has_failure(&self) -> bool {
        self.rendered.iter().any(ScreenRendered::failed)
    }

    pub fn override_for(&self, output: &str) -> Option<&ScreenOverride> {
        self.overrides.iter().find(|o| o.output == output)
    }

    /// Whether the rotation interval can take effect. A single picture keeps
    /// one image by definition, and rotation needs an interval above zero.
    pub fn rotation_possible(&self) -> bool {
        self.mode != "single"
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
        "the niructl command was not found; install NIRUX or run it from a NIRUX session"
            .to_string()
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
        let why = err
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("no explanation");
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

// The JSON report is a flat map of strings, booleans, numbers and arrays of
// objects, so one small reader is enough and no dependency is added for it.
// It follows the same idea as theme.rs: read the few fields the window needs.

fn json_skip_space(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn json_string(bytes: &[u8], i: usize) -> Option<(String, usize)> {
    if *bytes.get(i)? != b'"' {
        return None;
    }
    let mut out = String::new();
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'"' => return Some((out, j + 1)),
            b'\\' => {
                let esc = *bytes.get(j + 1)?;
                j += 2;
                match esc {
                    b'n' => out.push('\n'),
                    b't' => out.push('\t'),
                    b'r' => out.push('\r'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'u' => {
                        let hex = std::str::from_utf8(bytes.get(j..j + 4)?).ok()?;
                        let code = u32::from_str_radix(hex, 16).ok()?;
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        j += 4;
                    }
                    other => out.push(other as char),
                }
            }
            _ => {
                // Copy one UTF-8 character at a time.
                let rest = std::str::from_utf8(&bytes[j..]).ok()?;
                let ch = rest.chars().next()?;
                out.push(ch);
                j += ch.len_utf8();
            }
        }
    }
    None
}

fn json_skip_value(bytes: &[u8], i: usize) -> Option<usize> {
    match *bytes.get(i)? {
        b'"' => json_string(bytes, i).map(|(_, end)| end),
        b'{' | b'[' => {
            let open = bytes[i];
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 0usize;
            let mut j = i;
            while j < bytes.len() {
                match bytes[j] {
                    b'"' => {
                        j = json_string(bytes, j)?.1;
                        continue;
                    }
                    b if b == open => depth += 1,
                    b if b == close => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            None
        }
        _ => {
            let mut j = i;
            while j < bytes.len() && !b",}] \n\r\t".contains(&bytes[j]) {
                j += 1;
            }
            Some(j)
        }
    }
}

/// The members of one object, as (key, raw value text).
fn json_object(text: &str) -> Option<Vec<(String, String)>> {
    let bytes = text.as_bytes();
    let mut i = json_skip_space(bytes, 0);
    if *bytes.get(i)? != b'{' {
        return None;
    }
    i = json_skip_space(bytes, i + 1);
    let mut out = Vec::new();
    if bytes.get(i) == Some(&b'}') {
        return Some(out);
    }
    loop {
        let (key, after) = json_string(bytes, i)?;
        i = json_skip_space(bytes, after);
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i = json_skip_space(bytes, i + 1);
        let end = json_skip_value(bytes, i)?;
        out.push((key, text[i..end].to_string()));
        i = json_skip_space(bytes, end);
        match bytes.get(i) {
            Some(b',') => i = json_skip_space(bytes, i + 1),
            Some(b'}') => return Some(out),
            _ => return None,
        }
    }
}

fn json_string_field(members: &[(String, String)], key: &str) -> String {
    members
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| {
            // The stored value starts with its opening quote, so a string is
            // read from index 0, not 1.
            let bytes = v.as_bytes();
            let start = bytes.iter().position(|b| !b.is_ascii_whitespace())?;
            json_string(bytes, start).map(|(s, _)| s)
        })
        .unwrap_or_default()
}

fn json_bool_field(members: &[(String, String)], key: &str) -> bool {
    members
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v == "true")
        .unwrap_or(false)
}

fn json_number_field(members: &[(String, String)], key: &str) -> u32 {
    members
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0)
}

/// Split a JSON array of objects into its object texts.
fn json_array_objects(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut i = json_skip_space(bytes, 0);
    let mut out = Vec::new();
    if bytes.get(i) != Some(&b'[') {
        return out;
    }
    i = json_skip_space(bytes, i + 1);
    while i < bytes.len() && bytes[i] != b']' {
        let Some(end) = json_skip_value(bytes, i) else {
            break;
        };
        out.push(text[i..end].to_string());
        i = json_skip_space(bytes, end);
        if bytes.get(i) == Some(&b',') {
            i = json_skip_space(bytes, i + 1);
        }
    }
    out
}

/// Parse the structured report from `niructl wallpaper status-json`.
pub fn parse_json(text: &str) -> Option<Status> {
    let members = json_object(text)?;
    let mut status = Status {
        mode: "random".into(),
        scale: "fill".into(),
        structured: true,
        ..Status::default()
    };
    for (key, _) in &members {
        match key.as_str() {
            "enabled" => status.enabled = json_bool_field(&members, key),
            "follow_theme" => status.follow_theme = json_bool_field(&members, key),
            "rotation_minutes" => status.rotation_minutes = json_number_field(&members, key),
            "mode" => status.mode = json_string_field(&members, key),
            "scale" => status.scale = json_string_field(&members, key),
            "file" => status.file = json_string_field(&members, key),
            "dir" => status.dir = json_string_field(&members, key),
            "theme_dir" => status.theme_dir = json_string_field(&members, key),
            "theme" => status.theme = json_string_field(&members, key),
            "theme_image" => status.theme_image = json_string_field(&members, key),
            _ => {}
        }
    }
    if let Some((_, raw)) = members.iter().find(|(k, _)| k == "planned") {
        status.screens = json_array_objects(raw)
            .iter()
            .filter_map(|entry| {
                let fields = json_object(entry)?;
                Some(ScreenPlan {
                    output: json_string_field(&fields, "output"),
                    source: json_string_field(&fields, "source"),
                    kind: json_string_field(&fields, "mode"),
                    scale: json_string_field(&fields, "scale"),
                    image: json_string_field(&fields, "image"),
                })
            })
            .collect();
    }
    if let Some((_, raw)) = members.iter().find(|(k, _)| k == "rendered") {
        status.rendered = json_array_objects(raw)
            .iter()
            .filter_map(|entry| {
                let fields = json_object(entry)?;
                Some(ScreenRendered {
                    output: json_string_field(&fields, "output"),
                    kind: json_string_field(&fields, "mode"),
                    image: json_string_field(&fields, "image"),
                    scale: json_string_field(&fields, "scale"),
                    source: json_string_field(&fields, "source"),
                    note: json_string_field(&fields, "note"),
                })
            })
            .collect();
    }
    if let Some((_, raw)) = members.iter().find(|(k, _)| k == "outputs") {
        status.overrides = json_array_objects(raw)
            .iter()
            .filter_map(|entry| {
                let fields = json_object(entry)?;
                Some(ScreenOverride {
                    output: json_string_field(&fields, "output"),
                    mode: json_string_field(&fields, "mode"),
                    file: json_string_field(&fields, "file"),
                    scale: json_string_field(&fields, "scale"),
                })
            })
            .collect();
    }
    Some(status)
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
    // Prefer the structured report: it carries the per-screen plan and the
    // rendered state. A window manager from before that report still answers
    // the plain status, so the window keeps working after an upgrade.
    if let Ok(out) = run(&["wallpaper", "status-json"]) {
        if let Some(status) = parse_json(&String::from_utf8_lossy(&out.stdout)) {
            return Ok(status);
        }
    }
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

#[allow(dead_code)] // the window sends these through `Change`; kept for scripts
pub fn set_scale(scale: &str) -> Result<(), String> {
    run(&["wallpaper", "scale", scale]).map(|_| ())
}

#[allow(dead_code)] // the window sends these through `Change`; kept for scripts
pub fn set_rotation(minutes: u32) -> Result<(), String> {
    run(&["wallpaper", "rotation", &minutes.to_string()]).map(|_| ())
}

#[allow(dead_code)] // the window sends these through `Change`; kept for scripts
pub fn set_follow_theme(on: bool) -> Result<(), String> {
    run(&["wallpaper", "follow-theme", if on { "on" } else { "off" }]).map(|_| ())
}

/// One change the window wants applied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// A picture for every screen that inherits the common settings.
    pub image: Option<String>,
    pub scale: Option<String>,
    pub rotation: Option<u32>,
    pub follow_theme: Option<bool>,
    /// A picture for one named screen.
    pub output_image: Option<(String, String)>,
    /// A scale for one named screen.
    pub output_scale: Option<(String, String)>,
    /// Drop one screen's own settings.
    pub output_inherit: Option<String>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.image.is_none()
            && self.scale.is_none()
            && self.rotation.is_none()
            && self.follow_theme.is_none()
            && self.output_image.is_none()
            && self.output_scale.is_none()
            && self.output_inherit.is_none()
    }

    /// The argument vector for `niructl wallpaper apply`.
    ///
    /// Built here so the window never assembles a command, and so a test can
    /// check it without a window manager.
    pub fn args(&self) -> Vec<String> {
        let mut out = vec!["wallpaper".to_string(), "apply".into(), "--json".into()];
        if let Some(image) = &self.image {
            out.extend(["--image".into(), image.clone()]);
        }
        if let Some(scale) = &self.scale {
            out.extend(["--scale".into(), scale.clone()]);
        }
        if let Some(rotation) = self.rotation {
            out.extend(["--rotation".into(), rotation.to_string()]);
        }
        if let Some(follow) = self.follow_theme {
            out.extend([
                "--follow-theme".into(),
                if follow { "on" } else { "off" }.into(),
            ]);
        }
        if let Some((output, image)) = &self.output_image {
            out.extend(["--output".into(), output.clone(), image.clone()]);
        }
        if let Some((output, scale)) = &self.output_scale {
            out.extend(["--output-scale".into(), output.clone(), scale.clone()]);
        }
        if let Some(output) = &self.output_inherit {
            out.extend(["--output-inherit".into(), output.clone()]);
        }
        out
    }
}

/// Apply one change as a single validated transaction. The window manager
/// answers only after it has reloaded, so a return value here means the
/// change was accepted rather than merely written.
pub fn apply_change(change: &Change) -> Result<(), String> {
    if change.is_empty() {
        return Ok(());
    }
    let args = change.args();
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&borrowed).map(|_| ())
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

    const JSON: &str = r#"{
      "theme": "niru-nord",
      "enabled": true,
      "mode": "random",
      "rotation_minutes": 10,
      "scale": "fill",
      "follow_theme": true,
      "file": "",
      "dir": "~/Pictures/Wallpapers",
      "theme_dir": "/home/u/P/W/NIRU",
      "theme_image": "/home/u/P/W/NIRU/nirux-wallpaper-nord-4k.png",
      "outputs": [
        {"output": "eDP-1", "mode": "inherit", "file": "", "scale": ""},
        {"output": "DP-1", "mode": "single", "file": "/home/u/mine.png", "scale": "fit"}
      ],
      "planned": [
        {"output": "eDP-1", "source": "inherit", "mode": "theme", "scale": "fill", "image": "/home/u/P/W/NIRU/nirux-wallpaper-nord-4k.png"},
        {"output": "DP-1", "source": "override", "mode": "image", "scale": "fit", "image": "/home/u/mine.png"}
      ],
      "rendered": [
        {"output": "eDP-1", "mode": "theme", "image": "/home/u/P/W/NIRU/nirux-wallpaper-nord-4k.png", "scale": "fill", "source": "inherit", "note": ""},
        {"output": "DP-1", "mode": "failed", "image": "/home/u/mine.png", "scale": "fit", "source": "override", "note": "feh: exit status 1"}
      ]
    }"#;

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
        assert!(!s.structured);
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
        assert_eq!(
            entries[1].path,
            PathBuf::from("/home/u/Pictures/holiday.png")
        );
    }

    #[test]
    fn the_structured_report_carries_the_screen_plan_and_the_rendered_state() {
        let s = parse_json(JSON).expect("valid report");
        assert!(s.structured);
        assert!(s.enabled);
        assert_eq!(s.theme, "niru-nord");
        assert_eq!(s.screens.len(), 2);
        assert_eq!(s.screens[1].output, "DP-1");
        assert!(s.screens[1].overrides());
        assert_eq!(s.screens[0].kind, "theme");
        assert_eq!(s.overrides.len(), 2);
        assert!(s.override_for("DP-1").is_some_and(|o| o.mode == "single"));
        assert!(s
            .override_for("eDP-1")
            .is_some_and(ScreenOverride::inherits));
        assert!(s.has_rendered());
        assert!(
            s.has_failure(),
            "the failed screen must be visible as a failure"
        );
        // The active picture comes from the plan, not from `follow_theme`.
        assert_eq!(
            s.active_image(),
            Some("/home/u/P/W/NIRU/nirux-wallpaper-nord-4k.png")
        );
    }

    #[test]
    fn every_field_of_a_real_report_is_read() {
        // Shape produced by `niructl wallpaper status-json` in a real session.
        let text = r#"{
      "theme": "niru-noir",
      "enabled": true,
      "mode": "fixed",
      "rotation_minutes": 0,
      "scale": "fit",
      "follow_theme": true,
      "file": "",
      "dir": "~/Pictures/Wallpapers/mine",
      "theme_dir": "~/Pictures/Wallpapers/NIRU",
      "theme_image": "/w/nirux-wallpaper-noir-4k.png",
      "outputs": [],
      "planned": [{"output": "eDP-1", "source": "inherit", "mode": "theme",
                   "scale": "fit", "image": "/w/nirux-wallpaper-noir-4k.png"}],
      "rendered": null
    }"#;
        let s = parse_json(text).expect("valid report");
        assert_eq!(s.theme, "niru-noir");
        assert!(s.enabled);
        assert_eq!(s.mode, "fixed");
        assert_eq!(s.scale, "fit");
        assert!(s.follow_theme);
        assert_eq!(s.theme_dir, "~/Pictures/Wallpapers/NIRU");
        assert_eq!(s.screens.len(), 1);
        assert_eq!(s.screens[0].scale, "fit");
        assert!(s.overrides.is_empty());
        assert!(!s.has_rendered());
    }

    #[test]
    fn a_null_rendered_list_means_no_running_window_manager() {
        let text = r#"{"theme": "niru-noir", "enabled": true, "mode": "single",
        "file": "/home/u/a.png", "planned": [], "rendered": null}"#;
        let s = parse_json(text).expect("valid report");
        assert!(!s.has_rendered());
        assert_eq!(s.mode, "single");
        assert!(!s.rotation_possible(), "a single picture ignores rotation");
    }

    #[test]
    fn one_change_becomes_one_apply_command() {
        let change = Change {
            scale: Some("fit".into()),
            rotation: Some(0),
            output_image: Some(("DP-1".into(), "/w/my photo.png".into())),
            ..Change::default()
        };
        assert_eq!(
            change.args(),
            vec![
                "wallpaper",
                "apply",
                "--json",
                "--scale",
                "fit",
                "--rotation",
                "0",
                "--output",
                "DP-1",
                "/w/my photo.png",
            ]
        );
        let inherit = Change {
            output_inherit: Some("HDMI-1".into()),
            ..Change::default()
        };
        assert!(inherit
            .args()
            .ends_with(&["--output-inherit".to_string(), "HDMI-1".to_string()]));
        assert!(Change::default().is_empty());
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
