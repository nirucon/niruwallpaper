//! The wallpaper state the window manager already knows.
//!
//! This program owns no settings of its own. It reads what `niructl` reports
//! and asks `niructl` to change things, so the shared config file stays the
//! single source of truth and the GUI can never disagree with the WM about
//! what is actually applied.

use std::{
    io::Read,
    os::unix::io::AsRawFd,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

/// How long a question to `niructl` may take before it is given up.
pub const READ_DEADLINE: Duration = Duration::from_secs(10);
/// How long an Apply may take. NIRUWM composes each output with ffmpeg,
/// which takes seconds for 8K pictures on several monitors.
pub const APPLY_DEADLINE: Duration = Duration::from_secs(30);

/// One image the bar and the window manager would accept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wallpaper {
    /// Label shown in the list: a theme name or a file name.
    pub name: String,
    pub path: PathBuf,
    /// True when the image came from the theme directory, so it belongs to a
    /// NIRUX theme rather than to the user's own collection.
    pub official: bool,
    /// True for a theme's Signature picture (`nirux-wallpaper-signature-*`):
    /// the NIRUX photo tinted in that theme's colours.
    pub signature: bool,
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
    /// Which picture a theme shows: `art` (its own artwork) or `signature`
    /// (the tinted NIRUX photo, when the theme has one). `art` when niructl
    /// does not say, as a niructl from before the setting does not.
    pub theme_style: String,
    /// True when niructl reported `theme_style`, so it knows the setting.
    pub theme_style_reported: bool,
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

    /// True when themes show their Signature picture rather than the artwork.
    pub fn signature_style(&self) -> bool {
        self.theme_style == "signature"
    }

    /// The style as the window names it: "Art" or "Signature".
    pub fn style_label(&self) -> &'static str {
        style_label(&self.theme_style)
    }

    /// True when the theme picture in use is a Signature picture. With the
    /// Signature style chosen, a theme without one shows its artwork.
    pub fn theme_image_is_signature(&self) -> bool {
        is_signature_file(Path::new(&self.theme_image))
    }
}

/// Read a `theme_style` value tolerantly: anything but `signature` is the
/// default, `art`.
pub fn normalize_style(value: &str) -> String {
    if value.trim().eq_ignore_ascii_case("signature") {
        "signature".into()
    } else {
        "art".into()
    }
}

/// "Art" or "Signature", for a `theme_style` value.
pub fn style_label(style: &str) -> &'static str {
    if style == "signature" {
        "Signature"
    } else {
        "Art"
    }
}

/// The theme a Signature picture belongs to, as its file names it:
/// `nirux-wallpaper-signature-nord-4k.png` gives `nord`.
pub fn signature_slug(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let rest = stem.strip_prefix("nirux-wallpaper-signature-")?;
    let rest = rest
        .strip_suffix("-4k")
        .or_else(|| rest.strip_suffix("-8k"))
        .unwrap_or(rest);
    (!rest.is_empty()).then(|| rest.to_string())
}

pub fn is_signature_file(path: &Path) -> bool {
    signature_slug(path).is_some()
}

/// The list label of a Signature picture: "Signature" and the theme's name,
/// so the ten of them read as one group and not as ten more themes.
pub fn signature_label(slug: &str) -> String {
    let theme = THEMES
        .iter()
        .find(|(s, _)| s.strip_prefix("niru-") == Some(slug))
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| {
            slug.split('-')
                .filter(|p| !p.is_empty())
                .map(|p| {
                    let mut c = p.chars();
                    c.next()
                        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(" ")
        });
    format!("Signature {theme}")
}

/// Find the control command the desktop already installed.
///
/// `NIRUWALLPAPER_NIRUCTL` names one explicitly. Otherwise the installed
/// generation keeps it beside the session, and `~/.local/bin`
/// always carries a symlink; `PATH` is tried last so a hand-run build still
/// works from a checkout.
pub fn find_niructl() -> Option<PathBuf> {
    // An explicit command wins, so a test (or a packager) can point the
    // window at another niructl without changing HOME or PATH.
    if let Some(path) = std::env::var_os("NIRUWALLPAPER_NIRUCTL").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
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
    run_within(args, READ_DEADLINE)
}

/// Run `program` and collect its output, but never for longer than
/// `deadline`. The pipes are read without blocking, so a program that
/// stops answering (or leaves a child holding its stdout) cannot hang the
/// caller; it is killed and reaped when the time is up.
pub fn output_within(program: &mut Command, deadline: Duration) -> std::io::Result<Output> {
    let started = Instant::now();
    let mut child = program
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut pipes = [
        child.stdout.take().map(Pipe::new),
        child.stderr.take().map(Pipe::new),
    ];
    let mut expired = false;
    while pipes.iter().any(|p| p.as_ref().is_some_and(|p| p.open)) {
        let Some(left) = deadline.checked_sub(started.elapsed()) else {
            expired = true;
            break;
        };
        let mut fds: Vec<libc::pollfd> = pipes
            .iter()
            .flatten()
            .filter(|p| p.open)
            .map(|p| libc::pollfd {
                fd: p.fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: a live array of pollfd structures and its length.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
        for pipe in pipes.iter_mut().flatten() {
            pipe.read_available();
        }
    }
    // The pipes are closed: wait for the exit, still within the deadline.
    let status = loop {
        if expired {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("no answer within {} s", deadline.as_secs()),
            ));
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= deadline {
            expired = true;
            continue;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let [stdout, stderr] = pipes.map(|p| p.map(|p| p.data).unwrap_or_default());
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// One output pipe of a child, read without blocking.
struct Pipe {
    reader: Box<dyn Read>,
    fd: libc::c_int,
    data: Vec<u8>,
    open: bool,
}

impl Pipe {
    fn new(reader: impl Read + AsRawFd + 'static) -> Self {
        let fd = reader.as_raw_fd();
        // SAFETY: changing the flags of a descriptor this process owns.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Self {
            reader: Box::new(reader),
            fd,
            data: Vec::new(),
            open: true,
        }
    }

    fn fd(&self) -> libc::c_int {
        self.fd
    }

    fn read_available(&mut self) {
        let mut buf = [0u8; 8192];
        while self.open {
            match self.reader.read(&mut buf) {
                Ok(0) => self.open = false,
                Ok(n) => self.data.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => self.open = false,
            }
        }
    }
}

fn run_within(args: &[&str], deadline: Duration) -> Result<Output, String> {
    let bin = niructl()?;
    let out = output_within(Command::new(&bin).args(args), deadline).map_err(|e| {
        if e.kind() == std::io::ErrorKind::TimedOut {
            format!(
                "niructl gave no answer within {} s and was stopped",
                deadline.as_secs()
            )
        } else {
            format!("cannot run {}: {e}", bin.display())
        }
    })?;
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
        theme_style: "art".into(),
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
            "theme_style" => {
                status.theme_style = normalize_style(&value);
                status.theme_style_reported = true;
            }
            _ => {}
        }
    }
    status
}

/// Parse `niructl wallpaper list`, whose lines are `name<TAB>path`.
pub fn parse_list(text: &str, theme_dir: &str) -> Vec<Wallpaper> {
    let theme_root = PathBuf::from(expand_home(theme_dir));
    let theme_root = theme_root.canonicalize().unwrap_or(theme_root);
    let mut out: Vec<Wallpaper> = text
        .lines()
        .filter_map(|line| {
            let (name, path) = line.split_once('\t')?;
            if name.trim().is_empty() || path.trim().is_empty() {
                return None;
            }
            let path = PathBuf::from(path.trim());
            // `niructl` reports the theme directory in its own order first, so
            // membership in that directory is enough to call an entry official.
            let identity = path.canonicalize().unwrap_or_else(|_| path.clone());
            let official = identity.starts_with(&theme_root);
            // A theme's Signature picture is labelled as such, so it does not
            // read as a duplicate of the theme.
            let slug = official.then(|| signature_slug(&path)).flatten();
            Some(Wallpaper {
                name: slug
                    .as_deref()
                    .map(signature_label)
                    .unwrap_or_else(|| name.trim().to_string()),
                path,
                official,
                signature: slug.is_some(),
            })
        })
        .collect();
    // The Signature pictures follow the theme artwork as one group; the
    // order within each group, and of the user's own pictures, is kept.
    out.sort_by_key(|w| (!w.official, w.signature));
    out
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
        theme_style: "art".into(),
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
            "theme_style" => {
                status.theme_style = normalize_style(&json_string_field(&members, key));
                status.theme_style_reported = true;
            }
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

/// The arguments that switch every theme between its artwork and its
/// Signature picture. niructl writes the setting and reloads NIRUWM.
pub const STYLE_TOGGLE: [&str; 3] = ["wallpaper", "style", "toggle"];

/// Switch the theme picture style and return the style niructl reports
/// (`art` or `signature`), or `None` when it printed nothing recognisable.
/// The reload composes every screen again, so it gets the Apply deadline.
pub fn toggle_style() -> Result<Option<String>, String> {
    let out = run_within(&STYLE_TOGGLE, APPLY_DEADLINE)?;
    Ok(parse_style_answer(&String::from_utf8_lossy(&out.stdout)))
}

/// Read `theme_style=<value>` from what `niructl wallpaper style` printed.
pub fn parse_style_answer(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim() == "theme_style")
        .map(|(_, v)| normalize_style(v))
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
    run_within(&borrowed, APPLY_DEADLINE).map(|_| ())
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

    #[test]
    fn official_images_remain_official_through_a_symlinked_theme_directory() {
        let root = std::env::temp_dir().join(format!("niru-wallpaper-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("actual")).unwrap();
        std::fs::write(root.join("actual/image.png"), b"fixture").unwrap();
        std::os::unix::fs::symlink(root.join("actual"), root.join("Pictures")).unwrap();
        let list = format!("Example\t{}", root.join("actual/image.png").display());
        let entries = parse_list(&list, root.join("Pictures").to_str().unwrap());
        assert!(entries[0].official);
        std::fs::remove_dir_all(root).unwrap();
    }

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
    fn a_program_that_does_not_answer_is_stopped_at_the_deadline() {
        let started = Instant::now();
        let err = output_within(
            Command::new("sh").args(["-c", "echo started; sleep 30"]),
            Duration::from_millis(300),
        )
        .expect_err("the deadline must stop it");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
        // A child that keeps stdout open after its parent exits cannot hold
        // the caller either.
        let started = Instant::now();
        let _ = output_within(
            Command::new("sh").args(["-c", "sleep 30 & echo done"]),
            Duration::from_millis(300),
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_quick_program_is_read_completely() {
        let out = output_within(
            Command::new("sh").args(["-c", "seq 1 20000; echo oops >&2; exit 3"]),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 20000);
        assert_eq!(out.stderr, b"oops\n");
    }

    #[test]
    fn a_status_without_theme_style_means_art() {
        // An older niructl prints no theme_style line and no JSON field.
        let s = parse_status(SAMPLE);
        assert_eq!(s.theme_style, "art");
        assert!(!s.theme_style_reported);
        assert!(!s.signature_style());
        assert_eq!(s.style_label(), "Art");
        let j = parse_json(JSON).expect("valid report");
        assert_eq!(j.theme_style, "art");
        assert!(!j.theme_style_reported);
    }

    #[test]
    fn theme_style_is_read_from_both_reports() {
        let text = format!(
            "{SAMPLE}theme_style=signature\n\
theme_image=/w/NIRU/nirux-wallpaper-signature-nord-4k.png\n"
        );
        let s = parse_status(&text);
        assert_eq!(s.theme_style, "signature");
        assert!(s.theme_style_reported);
        assert_eq!(s.style_label(), "Signature");
        assert!(s.theme_image_is_signature());
        let json = JSON.replacen(
            "\"theme\": \"niru-nord\",",
            "\"theme\": \"niru-nord\", \"theme_style\": \"signature\",",
            1,
        );
        let j = parse_json(&json).expect("valid report");
        assert!(j.signature_style());
        assert!(j.theme_style_reported);
        assert!(!j.theme_image_is_signature(), "the artwork is still shown");
        let art = parse_json(r#"{"theme_style": "art"}"#).unwrap();
        assert_eq!(art.theme_style, "art");
        assert!(art.theme_style_reported);
    }

    #[test]
    fn theme_style_parsing_is_tolerant() {
        for (value, want) in [
            ("signature", "signature"),
            (" Signature ", "signature"),
            ("art", "art"),
            ("", "art"),
            ("photo", "art"),
        ] {
            let s = parse_status(&format!("theme_style={value}\n"));
            assert_eq!(s.theme_style, want, "{value:?}");
        }
        assert_eq!(
            parse_style_answer("theme_style=signature\n"),
            Some("signature".into())
        );
        assert_eq!(parse_style_answer("reloaded\n"), None);
        assert_eq!(STYLE_TOGGLE, ["wallpaper", "style", "toggle"]);
    }

    #[test]
    fn signature_pictures_are_labelled_and_grouped_after_the_artwork() {
        let dir = "/fixture/NIRU";
        let text = "Blood of Urdr\t/fixture/NIRU/nirux-wallpaper-blood-of-urdr-4k.png\n\
Signature Blood Of Urdr\t/fixture/NIRU/nirux-wallpaper-signature-blood-of-urdr-4k.png\n\
Signature Nord\t/fixture/NIRU/nirux-wallpaper-signature-nord-4k.png\n\
The Norns\t/fixture/NIRU/nirux-wallpaper-the-norns-4k.png\n\
Mine\t/home/u/Pictures/Wallpapers/nirux-wallpaper-signature-nord-4k.png\n";
        let entries = parse_list(text, dir);
        let names: Vec<&str> = entries.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Blood of Urdr",
                "The Norns",
                "Signature Blood of Urdr",
                "Signature Nord",
                "Mine"
            ]
        );
        assert!(entries[2].signature && entries[2].official);
        assert!(!entries[0].signature);
        assert!(
            !entries[4].signature,
            "only the theme directory holds Signature pictures"
        );
        assert_eq!(signature_label("some-new-one"), "Signature Some New One");
        assert_eq!(
            signature_slug(Path::new("/x/nirux-wallpaper-signature-noir.png")),
            Some("noir".into())
        );
        assert_eq!(
            signature_slug(Path::new("/x/nirux-wallpaper-noir-4k.png")),
            None
        );
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
