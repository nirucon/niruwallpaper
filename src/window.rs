//! The window: layout, keys and painting.
//!
//! Drawing is immediate mode into a backing pixmap. Every event that changes
//! state schedules a full repaint, which is then copied to the window in one
//! request, so nothing flickers and the state machine stays small.

use std::{
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use x11rb::{
    connection::Connection,
    protocol::{randr::ConnectionExt as _, xproto::*, Event},
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    COPY_DEPTH_FROM_PARENT,
};

use crate::backend::{self, Change, Status, Wallpaper};
use crate::screens::{self, Screen};
use crate::theme::{self, Palette};
use crate::thumbs;

pub const APP_ID: &str = "niruwallpaper";
pub const APP_NAME: &str = "NIRU Wallpaper";

const WIDTH: u16 = 1000;
const HEIGHT: u16 = 690;
const MIN_WIDTH: u16 = 660;
const MIN_HEIGHT: u16 = 500;

const PAD: i16 = 20;
const ROW: i16 = 26;
const HEADER: i16 = 100;
const TILE_W: i16 = 148;
const TILE_H: i16 = 132;
const PANEL_W: i16 = 300;
const FOOTER: i16 = 86;

/// How often the window asks the desktop whether anything changed.
///
/// Five calls to a local program a second with no user interaction is cheap,
/// and it is what a theme change, another program's edit or a monitor change
/// needs to appear without a restart. Nothing is rendered unless it changed.
const REFRESH: Duration = Duration::from_millis(1000);

/// Which list is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Official,
    Mine,
}

/// Which screen a choice is for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Target {
    /// Every screen without its own settings.
    All,
    /// One named screen.
    Screen(String),
}

impl Target {
    pub fn label(&self) -> String {
        match self {
            Target::All => "All screens".to_string(),
            Target::Screen(name) => screen_label(name),
        }
    }
}

/// Changes the user has staged but not applied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    pub image: Option<PathBuf>,
    pub scale: Option<String>,
    pub rotation: Option<u32>,
    pub follow_theme: Option<bool>,
    /// Drop the target screen's own settings.
    pub inherit: bool,
}

impl Pending {
    pub fn is_empty(&self) -> bool {
        self.image.is_none()
            && self.scale.is_none()
            && self.rotation.is_none()
            && self.follow_theme.is_none()
            && !self.inherit
    }
}

pub struct State {
    pub status: Status,
    pub list: Vec<Wallpaper>,
    pub tab: Tab,
    pub target: Target,
    pub selected: usize,
    pub scroll: usize,
    pub pending: Pending,
    pub message: String,
    pub message_is_error: bool,
    /// What the desktop said when the window last loaded, so a change made
    /// elsewhere is noticed. Pending edits are never discarded by a refresh.
    pub seen: String,
    /// Set while applying, so the window cannot start a second transaction.
    pub busy: bool,
    /// The palette the window was last drawn with.
    pub palette: Palette,
}

impl State {
    pub fn visible(&self) -> Vec<&Wallpaper> {
        self.list
            .iter()
            .filter(|w| match self.tab {
                Tab::Official => w.official,
                Tab::Mine => !w.official,
            })
            .collect()
    }

    pub fn current(&self) -> Option<&Wallpaper> {
        self.visible().get(self.selected).copied()
    }

    fn set_message(&mut self, text: impl Into<String>, error: bool) {
        self.message = text.into();
        self.message_is_error = error;
    }

    /// The names of every connected screen, in the order the desktop reports.
    pub fn screens(&self) -> Vec<String> {
        self.status
            .screens
            .iter()
            .map(|s| s.output.clone())
            .collect()
    }

    /// True when the desktop has more than one screen.
    pub fn multi_screen(&self) -> bool {
        self.screens().len() > 1
    }

    pub fn screen_plan(&self, name: &str) -> Option<&backend::ScreenPlan> {
        self.status.screens.iter().find(|s| s.output == name)
    }

    pub fn screen_rendered(&self, name: &str) -> Option<&backend::ScreenRendered> {
        self.status.rendered.iter().find(|s| s.output == name)
    }

    /// Whether changes now apply to one screen's own settings.
    pub fn override_target(&self) -> bool {
        matches!(self.target, Target::Screen(_))
    }

    fn active_screen_name(&self) -> Option<&str> {
        match &self.target {
            Target::Screen(name) => Some(name),
            Target::All => None,
        }
    }

    /// The state changes a refresh should not silently overwrite.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// A snapshot of everything the window shows.
fn snapshot(status: &Status, list: &[Wallpaper]) -> String {
    let mut out = format!(
        "{:?}|{:?}|{:?}|{}|{}|{}|{}|{:?}|",
        status.enabled,
        status.mode,
        status.rotation_minutes,
        status.scale,
        status.follow_theme,
        status.file,
        status.theme,
        status.theme_image
    );
    for screen in &status.screens {
        out.push_str(&format!(
            "{}:{}:{}:{}:{};",
            screen.output, screen.source, screen.kind, screen.scale, screen.image
        ));
    }
    for rendered in &status.rendered {
        out.push_str(&format!(
            "{}:{}:{}:{}:{};",
            rendered.output, rendered.kind, rendered.image, rendered.scale, rendered.note
        ));
    }
    for entry in &status.overrides {
        out.push_str(&format!(
            "{}:{}:{}:{};",
            entry.output, entry.mode, entry.file, entry.scale
        ));
    }
    for entry in list {
        out.push_str(&format!("{}:{};", entry.path.display(), entry.official));
    }
    out
}

/// Read everything the window shows from the desktop.
pub fn fetch() -> Result<(Status, Vec<Wallpaper>), String> {
    let status = backend::read_status()?;
    let list = backend::read_list(&status.theme_dir)?;
    Ok((status, list))
}

/// Load everything the window shows.
pub fn load() -> Result<State, String> {
    let (status, list) = fetch()?;
    let seen = snapshot(&status, &list);
    let mut state = State {
        status,
        list,
        tab: Tab::Official,
        target: Target::All,
        selected: 0,
        scroll: 0,
        pending: Pending::default(),
        message: String::new(),
        message_is_error: false,
        seen,
        busy: false,
        palette: theme::load(),
    };
    if state.multi_screen() {
        // A second screen is the reason this window grew a screen choice.
        state.target = Target::All;
    }
    if let Some(active) = state.status.active_image().map(Path::new) {
        if let Some(pos) = state
            .visible()
            .iter()
            .position(|w| w.path.as_path() == active)
        {
            state.selected = pos;
        } else if state.visible().iter().any(|w| !w.official) {
            state.tab = Tab::Mine;
            if let Some(pos) = state
                .visible()
                .iter()
                .position(|w| w.path.as_path() == active)
            {
                state.selected = pos;
            }
        }
    }
    Ok(state)
}

/// Take a fresh report from the desktop.
///
/// Returns whether anything the window shows changed. A staged change is
/// kept, and the footer says that new settings arrived meanwhile instead of
/// silently replacing the user's own.
pub fn refresh(state: &mut State) -> Result<bool, String> {
    let (status, list) = fetch()?;
    let snapshot = snapshot(&status, &list);
    let changed = snapshot != state.seen;
    let selected_path = state.current().map(|w| w.path.clone());
    let target_still_there = match &state.target {
        Target::Screen(name) => status.screens.iter().any(|s| &s.output == name),
        Target::All => true,
    };
    state.palette = theme::load();
    if changed && state.has_pending() {
        state.message = "The desktop changed elsewhere. Your staged changes were kept; Apply uses them as they are, Discard takes the new state.".into();
        state.message_is_error = false;
    }
    state.status = status;
    state.list = list;
    state.seen = snapshot;
    if !target_still_there {
        state.target = Target::All;
        state.pending = Pending::default();
    }
    if let Some(path) = selected_path {
        let visible = state.visible();
        if visible.is_empty() {
            state.selected = 0;
            state.scroll = 0;
        } else {
            state.tab = if visible
                .iter()
                .any(|w| w.official == (state.tab == Tab::Official))
            {
                state.tab
            } else if visible.iter().any(|w| w.official) {
                Tab::Official
            } else {
                Tab::Mine
            };
            let visible = state.visible();
            let pos = visible
                .iter()
                .position(|w| w.path == path)
                .unwrap_or_else(|| visible.len().saturating_sub(1));
            state.selected = pos;
        }
    }
    if state.selected >= state.visible().len() {
        state.selected = state.visible().len().saturating_sub(1);
    }
    Ok(changed)
}

/// Refuse to run over a screen that cannot take a 24-bit image.
///
/// Returns the error text; the caller decides whether it is fatal.
fn check_screen(c: &RustConnection) -> Result<(), String> {
    let root = c
        .get_geometry(c.setup().roots[0].root)
        .map_err(|e| e.to_string())?
        .reply();
    let root = root.map_err(|e| e.to_string())?;
    if ![24, 32].contains(&root.depth) {
        return Err(format!(
            "NIRU Wallpaper needs a 24 or 32-bit screen, this one is {}",
            root.depth
        ));
    }
    Ok(())
}

fn intern(c: &RustConnection) -> Result<Vec<(&'static str, u32)>, String> {
    const NAMES: [&str; 5] = [
        "UTF8_STRING",
        "WM_PROTOCOLS",
        "WM_DELETE_WINDOW",
        "_NET_WM_NAME",
        "_NET_WM_WINDOW_TYPE",
    ];
    let mut out = Vec::new();
    for name in NAMES {
        let atom = c
            .intern_atom(false, name.as_bytes())
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .atom;
        out.push((name, atom));
    }
    Ok(out)
}

fn atom_id(atoms: &[(&'static str, u32)], name: &str) -> u32 {
    atoms
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, a)| *a)
        .unwrap_or(0)
}

/// A thumbnail that arrived from a worker thread.
pub struct Thumb {
    pub path: PathBuf,
    pub image: Option<image::RgbaImage>,
}

pub struct App {
    c: RustConnection,
    win: Window,
    gc: Gcontext,
    back: x11rb::protocol::xproto::Pixmap,
    w: u16,
    h: u16,
    atoms: Vec<(&'static str, u32)>,
    pub palette: Palette,
    font: crate::text::Font,
    keys: Keys,
    /// True when the server wants the least significant byte first, which
    /// decides whether a pixel is uploaded as BGRX or RGBX.
    image_lsb: bool,
    root: Window,
    depth: u8,
    /// Decoded previews, so a repaint never decodes an image.
    thumbs: std::collections::HashMap<PathBuf, Option<image::RgbaImage>>,
    /// Previews that failed, and when, so a broken file is not retried on
    /// every repaint but a replaced file is.
    failed: std::collections::HashMap<PathBuf, Instant>,
    /// The screens RandR reports now.
    pub screens: Vec<Screen>,
    /// The screens seen on the last refresh, so a cable change is noticed.
    seen_screens: String,
    /// The screen index of the X connection, kept for re-reading RandR.
    xscreen: usize,
    requests: Sender<(u64, PathBuf)>,
    results: Receiver<(u64, Thumb)>,
    request_serial: u64,
    /// The refresh timer restarts when a key or click happens, so it never
    /// interrupts typing.
    last_activity: Instant,
    /// True when a monitor configuration change was seen; the screens are
    /// then re-read from RandR without waiting for the next refresh.
    screens_dirty: bool,
}

impl App {
    pub fn open() -> Result<Self, String> {
        let (c, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
        check_screen(&c)?;
        let screens = screens::read(&c, screen)?;
        let seen_screens = screens
            .iter()
            .map(|s| format!("{}:{},{},{}x{}", s.name, s.x, s.y, s.width, s.height))
            .collect::<Vec<_>>()
            .join(";");
        let root = c.setup().roots[screen].root;
        let atoms = intern(&c)?;
        let palette = theme::load();
        let font = crate::text::Font::load(palette);
        let keys = Keys::new(&c)?;
        let image_lsb =
            c.setup().image_byte_order == x11rb::protocol::xproto::ImageOrder::LSB_FIRST;

        let win = c.generate_id().map_err(|e| e.to_string())?;
        c.create_window(
            COPY_DEPTH_FROM_PARENT,
            win,
            root,
            0,
            0,
            WIDTH,
            HEIGHT,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(palette.background)
                .event_mask(
                    EventMask::EXPOSURE
                        | EventMask::KEY_PRESS
                        | EventMask::BUTTON_PRESS
                        | EventMask::POINTER_MOTION
                        | EventMask::STRUCTURE_NOTIFY,
                ),
        )
        .map_err(|e| e.to_string())?;
        // The drawing resources are created after the window exists: a pixmap
        // created before it is not tied to the window's screen and every
        // operation on it fails with a BadDrawable.
        let gc = c.generate_id().map_err(|e| e.to_string())?;
        let back = c.generate_id().map_err(|e| e.to_string())?;
        c.create_gc(gc, win, &CreateGCAux::new())
            .map_err(|e| e.to_string())?;
        // The backing pixmap belongs to the root window, not to this window.
        // A pixmap created on the window with depth 0 looks valid but every
        // draw into it fails with BadDrawable, and one created on the window
        // rejects 32-bit PutImage with BadMatch.
        let depth = c
            .get_geometry(root)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .depth;
        c.create_pixmap(depth, back, root, WIDTH, HEIGHT)
            .map_err(|e| e.to_string())?;
        c.sync().map_err(|e| e.to_string())?;

        let name = APP_NAME.as_bytes();
        c.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            name,
        )
        .map_err(|e| e.to_string())?;
        c.change_property8(
            PropMode::REPLACE,
            win,
            atom_id(&atoms, "_NET_WM_NAME"),
            atom_id(&atoms, "UTF8_STRING"),
            name,
        )
        .map_err(|e| e.to_string())?;
        c.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            format!("{APP_ID}\0{APP_ID}\0").as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        // A resizable window within the sizes the layout supports, so it stays
        // usable on a small screen and gains room on a wide one.
        let mut hints = vec![0u32; 18];
        hints[0] = 1 << 4 | 1 << 5 | 1 << 10 | 1 << 11; // min, max, base
        hints[5] = u32::from(MIN_WIDTH);
        hints[6] = u32::from(MIN_HEIGHT);
        hints[7] = 1;
        hints[8] = 1;
        hints[9] = PAD as u32;
        hints[10] = PAD as u32;
        c.change_property32(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NORMAL_HINTS,
            AtomEnum::WM_SIZE_HINTS,
            &hints,
        )
        .map_err(|e| e.to_string())?;
        c.change_property32(
            PropMode::REPLACE,
            win,
            atom_id(&atoms, "WM_PROTOCOLS"),
            AtomEnum::ATOM,
            &[atom_id(&atoms, "WM_DELETE_WINDOW")],
        )
        .map_err(|e| e.to_string())?;
        c.map_window(win).map_err(|e| e.to_string())?;
        c.sync().map_err(|e| e.to_string())?;
        // Ask for RandR events so a monitor change makes the window re-read
        // its screens; a server without RandR keeps working without them.
        let _ = c.randr_select_input(root, x11rb::protocol::randr::NotifyMask::SCREEN_CHANGE);

        let (requests, work) = mpsc::channel::<(u64, PathBuf)>();
        let (done, results) = mpsc::channel::<(u64, Thumb)>();
        spawn_workers(work, done);

        Ok(Self {
            c,
            win,
            gc,
            back,
            w: WIDTH,
            h: HEIGHT,
            atoms,
            palette,
            font,
            keys,
            image_lsb,
            root,
            depth,
            thumbs: std::collections::HashMap::new(),
            failed: std::collections::HashMap::new(),
            screens,
            seen_screens,
            xscreen: screen,
            requests,
            results,
            request_serial: 0,
            last_activity: Instant::now(),
            screens_dirty: false,
        })
    }

    /// Re-read the screens from RandR. Returns true when they changed.
    pub fn reread_screens(&mut self) -> bool {
        let Ok(screens) = screens::read(&self.c, self.xscreen) else {
            return false;
        };
        let seen = screens
            .iter()
            .map(|s| format!("{}:{},{},{}x{}", s.name, s.x, s.y, s.width, s.height))
            .collect::<Vec<_>>()
            .join(";");
        if seen == self.seen_screens {
            return false;
        }
        self.seen_screens = seen;
        self.screens = screens;
        true
    }

    fn resize_backing(&mut self) -> Result<(), String> {
        let pixmap = self.c.generate_id().map_err(|e| e.to_string())?;
        self.c
            .create_pixmap(self.depth, pixmap, self.root, self.w, self.h)
            .map_err(|e| e.to_string())?;
        let old = std::mem::replace(&mut self.back, pixmap);
        self.c.free_pixmap(old).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// A one-pixel focus ring just outside a control.
    fn ring(&self, r: Rectangle) -> Result<(), String> {
        let outer = Rectangle {
            x: r.x - 2,
            y: r.y - 2,
            width: r.width + 4,
            height: r.height + 4,
        };
        for edge in [
            Rectangle {
                width: outer.width,
                height: 1,
                ..outer
            },
            Rectangle {
                y: outer.y + outer.height as i16 - 1,
                width: outer.width,
                height: 1,
                ..outer
            },
            Rectangle {
                width: 1,
                height: outer.height,
                ..outer
            },
            Rectangle {
                x: outer.x + outer.width as i16 - 1,
                width: 1,
                height: outer.height,
                ..outer
            },
        ] {
            self.fill(self.palette.foreground, edge)?;
        }
        Ok(())
    }

    fn fill(&self, colour: u32, rect: Rectangle) -> Result<(), String> {
        if rect.width == 0 || rect.height == 0 {
            return Ok(());
        }
        self.c
            .change_gc(self.gc, &ChangeGCAux::new().foreground(colour))
            .map_err(|e| e.to_string())?;
        self.c
            .poly_fill_rectangle(self.back, self.gc, &[rect])
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())
    }

    /// Copy RGBA pixels into the backing pixmap.
    ///
    /// Three bytes per pixel with each scanline padded to four, which is the
    /// layout a 24-bit ZPixmap has. Asking for 32 bits on a 24-deep pixmap is
    /// rejected with BadMatch; leaving the padding implicit is rejected with
    /// BadLength.
    fn upload(&self, x: i16, y: i16, w: u16, h: u16, pixels: &[u8]) -> Result<(), String> {
        let bgr = self.image_lsb;
        let mut data = Vec::with_capacity(pixels.len());
        for chunk in pixels.chunks_exact(4) {
            if bgr {
                data.extend([chunk[2], chunk[1], chunk[0], 255]);
            } else {
                data.extend([chunk[0], chunk[1], chunk[2], 255]);
            }
        }
        self.c
            .put_image(
                ImageFormat::Z_PIXMAP,
                self.back,
                self.gc,
                w,
                h,
                x,
                y,
                0,
                self.depth,
                data.as_slice(),
            )
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())
    }

    /// Draw a picture scaled by cropping so it fills the rectangle, the way
    /// `fill` scales a wallpaper. Nothing is stretched out of shape.
    fn image_fill(&self, r: Rectangle, image: &image::RgbaImage) -> Result<(), String> {
        let (iw, ih) = (image.width(), image.height());
        if iw == 0 || ih == 0 || r.width == 0 || r.height == 0 {
            return Ok(());
        }
        let scale = f64::from(r.width) / f64::from(iw);
        let scale = scale.max(f64::from(r.height) / f64::from(ih));
        let tw = (f64::from(iw) * scale).round().max(f64::from(r.width)) as u32;
        let th = (f64::from(ih) * scale).round().max(f64::from(r.height)) as u32;
        let scaled = image::imageops::resize(image, tw, th, image::imageops::FilterType::Triangle);
        let x0 = (tw - u32::from(r.width)) / 2;
        let y0 = (th - u32::from(r.height)) / 2;
        let mut pixels = Vec::with_capacity(usize::from(r.width) * usize::from(r.height) * 4);
        for y in y0..y0 + u32::from(r.height) {
            for x in x0..x0 + u32::from(r.width) {
                let p = scaled.get_pixel(x, y);
                pixels.extend_from_slice(&p.0);
            }
        }
        self.upload(r.x, r.y, r.width, r.height, &pixels)
    }

    fn label(&self, x: i16, y: i16, text: &str, colour: u32, size: f32) -> Result<(), String> {
        let (w, h, pixels) = self
            .font
            .size(size)
            .render(text, colour, self.palette.background);
        if w == 0 || h == 0 {
            return Ok(());
        }
        self.upload(x, y, w as u16, h as u16, &pixels)
    }

    fn label_on(
        &self,
        x: i16,
        y: i16,
        text: &str,
        colour: u32,
        bg: u32,
        size: f32,
    ) -> Result<(), String> {
        let (w, h, pixels) = self.font.size(size).render(text, colour, bg);
        if w == 0 || h == 0 {
            return Ok(());
        }
        self.upload(x, y, w as u16, h as u16, &pixels)
    }

    fn button(&self, r: Rectangle, text: &str, active: bool, enabled: bool) -> Result<(), String> {
        let (bg, fg) = if !enabled {
            (self.palette.background, self.palette.border)
        } else if active {
            (self.palette.accent, self.palette.background)
        } else {
            (self.palette.surface, self.palette.foreground)
        };
        self.fill(bg, r)?;
        self.fill(
            self.palette.border,
            Rectangle {
                width: r.width,
                height: 1,
                ..r
            },
        )?;
        let (_, text_h, _) = self.font.size(13.0).render(text, fg, bg);
        let y = r.y + (i16::try_from(text_h).unwrap_or(0) - 8).max(0) / 2;
        self.label_on(r.x + 8, y, text, fg, bg, 13.0)
    }

    fn selected(&self, hit: Option<Hit>, want: Hit, focus: Option<Hit>) -> bool {
        hit == Some(want) || focus == Some(want)
    }

    /// Ask a worker for a preview if it is neither decoded nor known broken.
    fn want_thumb(&mut self, path: &Path) {
        if self.thumbs.contains_key(path) {
            return;
        }
        if let Some(at) = self.failed.get(path) {
            if at.elapsed() < Duration::from_secs(10) {
                return;
            }
        }
        self.request_serial += 1;
        let serial = self.request_serial;
        let _ = self.requests.send((serial, path.to_path_buf()));
    }

    /// Collect finished previews without blocking.
    fn collect_thumbs(&mut self) -> bool {
        let mut changed = false;
        while let Ok((_, thumb)) = self.results.try_recv() {
            if thumb.image.is_some() {
                self.failed.remove(&thumb.path);
                self.thumbs.insert(thumb.path.clone(), thumb.image);
            } else {
                self.failed.insert(thumb.path.clone(), Instant::now());
                self.thumbs.insert(thumb.path, None);
            }
            changed = true;
        }
        changed
    }

    fn thumb(&self, path: &Path) -> Option<&image::RgbaImage> {
        self.thumbs.get(path).and_then(|t| t.as_ref())
    }

    /// Draw the whole window. One function so a click and a repaint can never
    /// disagree about where a control is.
    pub fn paint(&self, state: &State, focus: Option<Hit>) -> Result<(), String> {
        let l = layout(self.w, self.h);
        self.fill(
            self.palette.background,
            Rectangle {
                x: 0,
                y: 0,
                width: self.w,
                height: self.h,
            },
        )?;
        self.label(PAD, PAD - 8, APP_NAME, self.palette.foreground, 20.0)?;
        let theme_line = match state.screens().as_slice() {
            [] => format!(
                "Theme: {}   No screens reported",
                theme_label(&state.status.theme)
            ),
            [one] => format!(
                "Theme: {}   Screen: {}",
                theme_label(&state.status.theme),
                one
            ),
            many => format!(
                "Theme: {}   {} screens",
                theme_label(&state.status.theme),
                many.len()
            ),
        };
        self.label(PAD, PAD + 22, &theme_line, self.palette.muted, 12.0)?;
        // Tabs.
        for (rect, tab, text) in [
            (l.tabs.0, Tab::Official, "Official themes"),
            (l.tabs.1, Tab::Mine, "My wallpapers"),
        ] {
            self.button(rect, text, state.tab == tab, true)?;
        }
        if state.multi_screen() {
            // The screen choice only exists when there is more than one screen.
            let targets = targets(state);
            for (i, rect) in screen_rects(&l, targets.len()).iter().enumerate() {
                let label = target_short(&targets[i]);
                self.button(*rect, &label, state.target == targets[i], true)?;
            }
        }

        // Tiles.
        let visible = state.visible();
        if visible.is_empty() {
            let text = match state.tab {
                Tab::Official => "No theme wallpapers are installed. Choose a theme in NIRUX settings, or add an image under My wallpapers.",
                Tab::Mine => "No images of your own. Put PNG, JPEG, WebP or BMP files in ~/Pictures/Wallpapers and open this window again.",
            };
            for (i, line) in wrap(text, l.left.width as usize / 7).iter().enumerate() {
                self.label(
                    l.left.x + 8,
                    l.left.y + 16 + i as i16 * 22,
                    line,
                    self.palette.muted,
                    12.0,
                )?;
            }
        }
        for (i, entry) in visible.iter().enumerate() {
            let rect = tile_rect(&l, i, state.scroll);
            if (rect.y + rect.height as i16) < (l.left.y + 4) || rect.y > self.h as i16 {
                continue;
            }
            let active = state
                .status
                .active_image()
                .map(|a| Path::new(a) == entry.path)
                .unwrap_or(false);
            let chosen = state.pending.image.as_deref() == Some(entry.path.as_path());
            let hovered = focus == Some(Hit::Tile(i));
            let border = if hovered {
                self.palette.foreground
            } else if chosen || active {
                self.palette.accent
            } else {
                self.palette.border
            };
            self.fill(self.palette.surface, rect)?;
            self.fill(
                border,
                Rectangle {
                    width: rect.width,
                    height: 2,
                    ..rect
                },
            )?;
            let inner = Rectangle {
                x: rect.x + 2,
                y: rect.y + 2,
                width: rect.width - 4,
                height: rect.height.saturating_sub(20),
            };
            match self.thumb(&entry.path) {
                Some(image) => self.image_fill(inner, image)?,
                None => {
                    let text = if self.failed.contains_key(&entry.path) {
                        "(no preview)"
                    } else {
                        "Loading preview"
                    };
                    self.label(rect.x + 6, rect.y + 8, text, self.palette.muted, 11.0)?;
                }
            }
            let label = truncate(&entry.name, 18);
            self.label(
                rect.x + 4,
                rect.y + rect.height as i16 - 18,
                &label,
                if active {
                    self.palette.accent
                } else {
                    self.palette.foreground
                },
                11.0,
            )?;
        }

        self.paint_panel(state, &l, focus)?;

        if let Some(hit) = focus {
            if let Some(rect) = hit_rect(&l, hit, state) {
                self.ring(rect)?;
            }
        }

        // Footer message.
        let message = if state.busy {
            "Applying… the window manager is applying the change".to_string()
        } else if state.message.is_empty() {
            describe(state)
        } else {
            state.message.clone()
        };
        let colour = if state.message_is_error {
            self.palette.accent
        } else {
            self.palette.muted
        };
        self.label(
            PAD,
            l.footer + 6,
            &truncate(&message, (self.w as usize / 7).saturating_sub(2)),
            colour,
            12.0,
        )?;
        self.label(
            PAD,
            l.footer + 26,
            "Tab move  arrows select  Space choose  Enter apply  Ctrl+D discard  F1 help  Esc/q close",
            self.palette.muted,
            11.0,
        )?;
        Ok(())
    }

    fn paint_panel(&self, state: &State, l: &Layout, focus: Option<Hit>) -> Result<(), String> {
        self.fill(self.palette.surface, l.panel)?;
        let x = l.panel.x + 12;
        let target = state.target.label();
        self.label(
            x,
            l.panel.y + 4,
            &truncate(&format!("Settings for {target}"), 40),
            self.palette.muted,
            12.0,
        )?;

        let own = state
            .active_screen_name()
            .and_then(|name| state.status.override_for(name))
            .is_some_and(|o| !o.inherits());
        // The focus ring is drawn around the focused control, so the keyboard
        // always shows what Space or Enter would act on.
        self.button(
            settings_rect(l),
            if own {
                "This screen follows the theme"
            } else {
                "This screen follows the shared settings"
            },
            own,
            state.override_target() && state.override_target(),
        )?;
        let targets_set = state.override_target();
        self.label(
            x,
            l.panel.y + 22,
            if targets_set {
                "Pick a picture below to give this screen its own"
            } else {
                "Pick a screen in the header to set it alone"
            },
            self.palette.muted,
            10.0,
        )?;

        self.label(x, l.panel.y + 78, "Scale", self.palette.muted, 12.0)?;
        let scale = state
            .pending
            .scale
            .clone()
            .unwrap_or_else(|| match state.target {
                Target::All => state.status.scale.clone(),
                Target::Screen(ref name) => state
                    .screen_plan(name)
                    .map(|s| s.scale.clone())
                    .unwrap_or_else(|| state.status.scale.clone()),
            });
        for (i, rect) in scale_rects(l).iter().enumerate() {
            let hit = Hit::Scale(i as u8);
            self.button(*rect, SCALES[i], SCALES[i] == scale, !state.busy)?;
            let _ = self.selected(Some(hit), hit, focus);
        }
        let effective = match &state.target {
            Target::All => state.status.scale.clone(),
            Target::Screen(name) => state
                .screen_rendered(name)
                .map(|r| r.scale.clone())
                .filter(|s| !s.is_empty())
                .or_else(|| state.screen_plan(name).map(|s| s.scale.clone()))
                .unwrap_or_default(),
        };
        self.label(
            x,
            l.panel.y + 144,
            &truncate(&format!("In effect: {effective}"), 38),
            self.palette.muted,
            11.0,
        )?;

        self.label(
            x,
            l.panel.y + 168,
            "Rotate (minutes)",
            self.palette.muted,
            12.0,
        )?;
        let rotation = state
            .pending
            .rotation
            .unwrap_or(state.status.rotation_minutes);
        for (i, rect) in rotation_rects(l).iter().enumerate() {
            let text = if ROTATIONS[i] == 0 {
                "Off".to_string()
            } else {
                ROTATIONS[i].to_string()
            };
            self.button(*rect, &text, ROTATIONS[i] == rotation, !state.busy)?;
        }
        if !state.status.rotation_possible() {
            self.label(
                x,
                l.panel.y + 224,
                "A single picture ignores rotation.",
                self.palette.muted,
                11.0,
            )?;
        }

        let follow = state
            .pending
            .follow_theme
            .unwrap_or(state.status.follow_theme);
        self.button(
            follow_rect(l),
            if follow {
                "Follow theme: on"
            } else {
                "Follow theme: off"
            },
            follow,
            !state.busy,
        )?;

        // What this target shows, and why.
        let lines = describe_target(state, &state.target);
        for (i, line) in lines.iter().enumerate() {
            self.label(
                x,
                l.panel.y + 292 + i as i16 * 18,
                &truncate(line, 42),
                if i == 0 {
                    self.palette.foreground
                } else {
                    self.palette.muted
                },
                11.0,
            )?;
        }

        self.button(
            apply_rect(l),
            "Apply",
            !state.pending.is_empty() && !state.busy,
            !state.pending.is_empty() && !state.busy,
        )?;
        self.button(
            cancel_rect(l),
            "Discard",
            !state.pending.is_empty() && !state.busy,
            !state.pending.is_empty() && !state.busy,
        )?;
        Ok(())
    }

    pub fn flush(&self) -> Result<(), String> {
        self.c
            .copy_area(self.back, self.win, self.gc, 0, 0, 0, 0, self.w, self.h)
            .map_err(|e| e.to_string())?;
        self.c.flush().map_err(|e| e.to_string())
    }
}

/// Two workers are enough for a list of previews; more only competes for the
/// same disk and decoder. Each one exits when the window drops the sender.
fn spawn_workers(work: Receiver<(u64, PathBuf)>, done: Sender<(u64, Thumb)>) {
    let work = Arc::new(Mutex::new(work));
    for _ in 0..2 {
        let work = work.clone();
        let done = done.clone();
        thread::spawn(move || {
            let take = |work: &Arc<Mutex<Receiver<(u64, PathBuf)>>>| {
                work.lock().ok().and_then(|rx| rx.recv().ok())
            };
            loop {
                let Ok((serial, path)) = take(&work).ok_or(()) else {
                    return;
                };
                let image = thumbs::decode_preview(&path);
                if done.send((serial, Thumb { path, image })).is_err() {
                    return;
                }
            }
        });
    }
}

/// Human name for a theme slug.
pub fn theme_label(slug: &str) -> String {
    backend::THEMES
        .iter()
        .find(|(s, _)| *s == slug)
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| slug.trim_start_matches("niru-").to_string())
}

/// A short, readable name for a RandR output: `eDP-1` stays as it is.
pub fn screen_label(name: &str) -> String {
    name.to_string()
}

fn target_short(target: &Target) -> String {
    match target {
        Target::All => "All screens".to_string(),
        Target::Screen(name) => truncate(name, 12),
    }
}

/// The screen choices shown in the header: all screens first, then each one.
pub fn targets(state: &State) -> Vec<Target> {
    let mut out = vec![Target::All];
    for name in state.screens() {
        out.push(Target::Screen(name));
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// Break a paragraph at word boundaries, roughly `width` characters per line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(20);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + word.chars().count() + 1 > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// What one target shows, in up to three short lines.
pub fn describe_target(state: &State, target: &Target) -> Vec<String> {
    let status = &state.status;
    if !status.enabled {
        return vec![
            "NIRUWM does not own the wallpaper.".into(),
            "Enable it in the NIRUX configuration.".into(),
        ];
    }
    match target {
        Target::All => {
            let mut out = vec![describe_common(state)];
            if state.multi_screen() {
                let inherited = status.screens.iter().filter(|s| !s.overrides()).count();
                let own = status.screens.iter().filter(|s| s.overrides()).count();
                out.push(format!(
                    "{} screen(s) inherit this, {} have their own",
                    inherited, own
                ));
            }
            if status.has_failure() {
                out.push("A screen failed to show its picture.".into());
            }
            out
        }
        Target::Screen(name) => {
            let mut out = Vec::new();
            if let Some(rendered) = state.screen_rendered(name) {
                match rendered.kind.as_str() {
                    "theme" => out.push(format!("Showing theme {}", file_name(&rendered.image))),
                    "image" => out.push(format!("Showing {}", file_name(&rendered.image))),
                    "colour" => out.push("Theme colour, no theme picture".into()),
                    "failed" => {
                        out.push(format!("Could not set: {}", truncate(&rendered.note, 30)))
                    }
                    "off" => out.push("Not set: NIRUWM does not own it".into()),
                    _ => out.push("Not reported by the window manager".into()),
                }
            } else if let Some(plan) = state.screen_plan(name) {
                out.push(match plan.kind.as_str() {
                    "theme" => format!("Configured: theme {}", file_name(&plan.image)),
                    "image" => format!("Configured: {}", file_name(&plan.image)),
                    "colour" => "Configured: theme colour".into(),
                    _ => "Configured: nothing".into(),
                });
                out.push("No running window manager to confirm".into());
            } else {
                out.push("Screen not reported".into());
            }
            if let Some(entry) = status.override_for(name) {
                if entry.inherits() {
                    out.push("Inherits the shared settings".into());
                } else {
                    out.push(match entry.mode.as_str() {
                        "theme" => "Own setting: follows the theme".into(),
                        _ if !entry.file.is_empty() => {
                            format!("Own setting: {}", file_name(&entry.file))
                        }
                        _ => "Own setting".into(),
                    });
                }
            } else {
                out.push("Inherits the shared settings".into());
            }
            out
        }
    }
}

/// One line about what the desktop is showing on the inheriting screens.
fn describe_common(state: &State) -> String {
    let status = &state.status;
    if status.follows_theme() {
        format!("Showing the {} theme wallpaper", theme_label(&status.theme))
    } else if status.shows_theme_colour() {
        format!(
            "The {} theme has no wallpaper; its background colour is used",
            theme_label(&status.theme)
        )
    } else if status.has_override() {
        format!("Showing your image: {}", file_name(&status.file))
    } else if status.mode == "fixed" && !status.file.is_empty() {
        format!("Keeping {}", file_name(&status.file))
    } else {
        "No picture chosen yet".to_string()
    }
}

/// One line about what the desktop is showing.
fn describe(state: &State) -> String {
    if state.status.enabled {
        describe_common(state)
    } else {
        "NIRUWM does not own the wallpaper".to_string()
    }
}

fn file_name(path: &str) -> String {
    if path.is_empty() {
        return "nothing".into();
    }
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Where each clickable region sits, so painting and clicking cannot disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Tab(Tab),
    /// The screen choice at this position.
    Screen(u8),
    Tile(usize),
    Scale(u8),
    Rotation(u8),
    FollowTheme,
    ScreenFollow,
    Apply,
    Cancel,
}

/// The layout for one window size.
pub struct Layout {
    pub left: Rectangle,
    pub panel: Rectangle,
    pub footer: i16,
    pub tiles_per_row: usize,
    pub visible_rows: usize,
    pub tabs: (Rectangle, Rectangle),
    /// The y of the header tabs, and of the screen choice beneath them.
    pub screen_row: i16,
}

pub fn layout(w: u16, h: u16) -> Layout {
    let w = i16::try_from(w).unwrap_or(1000);
    let h = i16::try_from(h).unwrap_or(690);
    let list_w = w - PAD * 3 - PANEL_W;
    let top = HEADER + ROW;
    let footer = h - FOOTER;
    let tiles_per_row = ((list_w / TILE_W).max(1)) as usize;
    let visible_rows = ((footer - top) / TILE_H).max(1) as usize;
    Layout {
        left: Rectangle {
            x: PAD,
            y: top,
            width: (list_w + PAD).max(1) as u16,
            height: (footer - top).max(1) as u16,
        },
        panel: Rectangle {
            x: w - PAD - PANEL_W,
            y: HEADER - 20,
            width: PANEL_W as u16,
            height: (footer - HEADER + 20).max(1) as u16,
        },
        footer,
        tiles_per_row,
        visible_rows,
        tabs: (
            Rectangle {
                x: PAD,
                y: 66,
                width: 130,
                height: 28,
            },
            Rectangle {
                x: PAD + 140,
                y: 66,
                width: 130,
                height: 28,
            },
        ),
        screen_row: 66,
    }
}

/// The rectangle a control occupies, for the focus ring and for keys.
pub fn hit_rect(l: &Layout, hit: Hit, state: &State) -> Option<Rectangle> {
    Some(match hit {
        Hit::Tab(Tab::Official) => l.tabs.0,
        Hit::Tab(Tab::Mine) => l.tabs.1,
        Hit::Screen(i) => *screen_rects(l, targets(state).len()).get(i as usize)?,
        Hit::Tile(i) => tile_rect(l, i, state.scroll),
        Hit::Scale(i) => *scale_rects(l).get(i as usize)?,
        Hit::Rotation(i) => *rotation_rects(l).get(i as usize)?,
        Hit::FollowTheme => follow_rect(l),
        Hit::ScreenFollow => settings_rect(l),
        Hit::Apply => apply_rect(l),
        Hit::Cancel => cancel_rect(l),
    })
}

/// The clickable region at a point, if any.
pub fn hit(w: u16, h: u16, x: i16, y: i16, state: &State) -> Option<Hit> {
    let l = layout(w, h);
    for (rect, hit) in [
        (l.tabs.0, Hit::Tab(Tab::Official)),
        (l.tabs.1, Hit::Tab(Tab::Mine)),
    ] {
        if inside(rect, x, y) {
            return Some(hit);
        }
    }
    if state.multi_screen() {
        let targets = targets(state);
        for (i, rect) in screen_rects(&l, targets.len()).iter().enumerate() {
            if inside(*rect, x, y) {
                return Some(Hit::Screen(i as u8));
            }
        }
    }
    // Tiles, in the order they are painted.
    let count = state.visible().len();
    for i in 0..count {
        let rect = tile_rect(&l, i, state.scroll);
        if inside(rect, x, y) {
            return Some(Hit::Tile(i));
        }
    }
    let scale = scale_rects(&l);
    for (i, rect) in scale.iter().enumerate() {
        if inside(*rect, x, y) {
            return Some(Hit::Scale(i as u8));
        }
    }
    let rotation = rotation_rects(&l);
    for (i, rect) in rotation.iter().enumerate() {
        if inside(*rect, x, y) {
            return Some(Hit::Rotation(i as u8));
        }
    }
    if inside(follow_rect(&l), x, y) {
        return Some(Hit::FollowTheme);
    }
    if inside(settings_rect(&l), x, y) {
        return Some(Hit::ScreenFollow);
    }
    if inside(apply_rect(&l), x, y) {
        return Some(Hit::Apply);
    }
    if inside(cancel_rect(&l), x, y) {
        return Some(Hit::Cancel);
    }
    None
}

fn screen_rects(l: &Layout, count: usize) -> Vec<Rectangle> {
    let mut out = Vec::new();
    let mut x = l.tabs.1.x + l.tabs.1.width as i16 + 20;
    for i in 0..count {
        let width = if i == 0 { 100 } else { 92 };
        out.push(Rectangle {
            x,
            y: l.screen_row,
            width,
            height: 28,
        });
        x += width as i16 + 6;
    }
    out
}

fn inside(r: Rectangle, x: i16, y: i16) -> bool {
    x >= r.x && x < r.x + r.width as i16 && y >= r.y && y < r.y + r.height as i16
}

fn tile_rect(l: &Layout, index: usize, scroll: usize) -> Rectangle {
    let row = index / l.tiles_per_row;
    let col = index % l.tiles_per_row;
    let first = scroll * l.tiles_per_row;
    let row_visible = index >= first;
    let shown_row = row.saturating_sub(scroll);
    let y = if row_visible {
        l.left.y + 8 + shown_row as i16 * TILE_H
    } else {
        -TILE_H
    };
    Rectangle {
        x: l.left.x + 8 + col as i16 * TILE_W,
        y,
        width: (TILE_W - 8) as u16,
        height: (TILE_H - 8) as u16,
    }
}

const SCALES: [&str; 4] = ["fill", "fit", "stretch", "center"];
const ROTATIONS: [u32; 5] = [0, 5, 10, 30, 60];

fn scale_rects(l: &Layout) -> Vec<Rectangle> {
    let mut out = Vec::new();
    let y = l.panel.y + 96;
    for i in 0..SCALES.len() {
        out.push(Rectangle {
            x: l.panel.x + 12 + (i as i16 % 2) * 138,
            y: y + (i as i16 / 2) * 30,
            width: 130,
            height: 26,
        });
    }
    out
}

fn rotation_rects(l: &Layout) -> Vec<Rectangle> {
    let mut out = Vec::new();
    let y = l.panel.y + 186;
    for i in 0..ROTATIONS.len() {
        out.push(Rectangle {
            x: l.panel.x + 12 + (i as i16 % 3) * 92,
            y: y + (i as i16 / 3) * 28,
            width: 86,
            height: 24,
        });
    }
    out
}

fn follow_rect(l: &Layout) -> Rectangle {
    Rectangle {
        x: l.panel.x + 12,
        y: l.panel.y + 250,
        width: PANEL_W as u16 - 24,
        height: 28,
    }
}

fn settings_rect(l: &Layout) -> Rectangle {
    Rectangle {
        x: l.panel.x + 12,
        y: l.panel.y + 40,
        width: PANEL_W as u16 - 24,
        height: 28,
    }
}

fn apply_rect(l: &Layout) -> Rectangle {
    Rectangle {
        x: l.panel.x + 12,
        y: l.panel.y + l.panel.height as i16 - 96,
        width: PANEL_W as u16 - 24,
        height: 36,
    }
}

fn cancel_rect(l: &Layout) -> Rectangle {
    Rectangle {
        x: l.panel.x + 12,
        y: l.panel.y + l.panel.height as i16 - 52,
        width: PANEL_W as u16 - 24,
        height: 32,
    }
}

/// Stage the currently selected image for the current target.
pub fn select_current(state: &mut State) {
    let picked = state
        .current()
        .map(|entry| (entry.name.clone(), entry.path.clone()));
    if let Some((name, path)) = picked {
        state.pending.image = Some(path);
        // An explicit picture is what a screen shows instead of the theme, so
        // leaving theme following staged would contradict it.
        if state
            .pending
            .follow_theme
            .unwrap_or(state.status.follow_theme)
        {
            state.pending.follow_theme = Some(false);
            state.set_message(
                format!("Stage {name} — this replaces the theme wallpaper"),
                false,
            );
        } else {
            state.set_message(format!("Staged {name}"), false);
        }
    }
}

/// Build the change the staged settings describe for the current target.
pub fn staged_change(state: &State) -> Result<Change, String> {
    let pending = &state.pending;
    if pending.is_empty() {
        return Ok(Change::default());
    }
    if pending.image.is_some() && pending.follow_theme == Some(true) {
        return Err("choose either a picture or the theme wallpaper, not both".into());
    }
    let mut change = Change::default();
    match &state.target {
        Target::All => {
            change.image = pending.image.as_ref().map(|p| p.display().to_string());
            change.scale = pending.scale.clone();
            change.rotation = pending.rotation;
            change.follow_theme = pending.follow_theme;
        }
        Target::Screen(name) => {
            // A screen gets its own picture or follows the theme. Scale and
            // inheritance are its own too; rotation stays a common setting
            // because one desktop rotates together.
            if pending.inherit {
                change.output_inherit = Some(name.clone());
                change.rotation = pending.rotation;
            }
            if let Some(image) = &pending.image {
                change.output_image = Some((name.clone(), image.display().to_string()));
            } else if pending.follow_theme == Some(true) {
                change.output_image = None;
                // Mode `theme` is expressed by dropping a picture and keeping
                // an entry that follows the theme.
                change.output_inherit = change.output_inherit.or_else(|| Some(name.clone()));
            }
            if let Some(scale) = &pending.scale {
                change.output_scale = Some((name.clone(), scale.clone()));
            }
        }
    }
    Ok(change)
}

/// Apply the staged changes as one transaction and take the desktop's answer.
///
/// Kept separate from the event loop so a test can drive it without a window:
/// the `command` argument is what really runs it.
pub fn apply_with<F>(state: &mut State, command: F) -> Result<(), String>
where
    F: FnOnce(&Change) -> Result<(), String>,
{
    let change = staged_change(state)?;
    if change.is_empty() {
        return Ok(());
    }
    command(&change)?;
    let (status, list) = fetch()?;
    let target = state.target.clone();
    state.status = status;
    state.list = list;
    state.pending = Pending::default();
    state.seen = snapshot(&state.status, &state.list);
    if let Target::Screen(name) = &target {
        if !state.screens().iter().any(|s| s == name) {
            state.target = Target::All;
        }
    }
    if state.selected >= state.visible().len() {
        state.selected = state.visible().len().saturating_sub(1);
    }
    let failure = state
        .status
        .rendered
        .iter()
        .find(|r| r.failed())
        .map(|r| format!("Applied, but {} failed: {}", r.output, r.note))
        .or_else(|| {
            if state.status.enabled && !state.status.has_rendered() {
                Some("Saved. No running window manager confirmed the result.".into())
            } else {
                None
            }
        });
    match failure {
        Some(text) => state.set_message(text, true),
        None => state.set_message("Applied", false),
    }
    Ok(())
}

/// The real transaction: one `niructl wallpaper apply`.
pub fn apply(state: &mut State) -> Result<(), String> {
    apply_with(state, backend::apply_change)
}

/// Keysyms for the keys the window answers to.
///
/// Named keys live in X's fixed range; printable keys use their Latin-1 code
/// point, so one table serves every layout.
const K_ESCAPE: u32 = 0xff1b;
const K_TAB: u32 = 0xff09;
const K_RETURN: u32 = 0xff0d;
const K_LEFT: u32 = 0xff51;
const K_UP: u32 = 0xff52;
const K_RIGHT: u32 = 0xff53;
const K_DOWN: u32 = 0xff54;
const K_SPACE: u32 = 0x0020;
const K_Q: u32 = b'q' as u32;
const K_Q_SHIFTED: u32 = b'Q' as u32;
const K_H: u32 = b'h' as u32;
const K_J: u32 = b'j' as u32;
const K_K: u32 = b'k' as u32;
const K_L: u32 = b'l' as u32;
const K_A: u32 = b'a' as u32;
const K_D: u32 = b'd' as u32;
const K_F1: u32 = 0xffbe;

/// Keycode-to-keysym table for the connected keyboard.
///
/// Built once from the server's mapping so a keypress resolves to the same
/// keysym on a US and a Swedish layout.
pub struct Keys {
    /// Keysym for each keycode, indexed from `min`.
    table: Vec<u32>,
    min: u8,
}

impl Keys {
    pub fn new(c: &RustConnection) -> Result<Self, String> {
        let setup = c.setup();
        let count = setup.max_keycode - setup.min_keycode + 1;
        let map = c
            .get_keyboard_mapping(setup.min_keycode, count)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        let per = map.keysyms_per_keycode.max(1) as usize;
        // The first level of each key is the unshifted character, which is what
        // navigation and letter shortcuts need.
        let table = map
            .keysyms
            .chunks(per)
            .map(|syms| syms.first().copied().unwrap_or(0))
            .collect();
        Ok(Self {
            table,
            min: setup.min_keycode,
        })
    }

    pub fn sym(&self, code: u8) -> u32 {
        code.checked_sub(self.min)
            .and_then(|i| self.table.get(i as usize))
            .copied()
            .unwrap_or(0)
    }
}

/// The order Tab moves through, so every control is keyboard reachable.
pub fn focus_order(state: &State) -> Vec<Hit> {
    let mut out = vec![Hit::Tab(Tab::Official), Hit::Tab(Tab::Mine)];
    if state.multi_screen() {
        for i in 0..targets(state).len() {
            out.push(Hit::Screen(i as u8));
        }
    }
    if !state.visible().is_empty() {
        out.push(Hit::Tile(0));
    }
    for i in 0..SCALES.len() {
        out.push(Hit::Scale(i as u8));
    }
    for i in 0..ROTATIONS.len() {
        out.push(Hit::Rotation(i as u8));
    }
    out.push(Hit::FollowTheme);
    out.push(Hit::ScreenFollow);
    out.push(Hit::Apply);
    out.push(Hit::Cancel);
    out
}

/// Move keyboard focus forward or backward in `focus_order`.
pub fn move_focus(state: &State, focus: Option<Hit>, forward: bool) -> Option<Hit> {
    let order = focus_order(state);
    if order.is_empty() {
        return None;
    }
    let current = focus.and_then(|f| order.iter().position(|h| *h == f));
    let next = match (current, forward) {
        (Some(i), true) => (i + 1) % order.len(),
        (Some(i), false) => (i + order.len() - 1) % order.len(),
        (None, true) => 0,
        (None, false) => order.len() - 1,
    };
    Some(order[next])
}

/// The keyboard controls the window answers to.
pub enum KeyAction {
    None,
    Redraw,
    Select,
    Apply,
    Discard,
    Quit,
    /// Enter on a focused control, the same as clicking it.
    Activate(Hit),
    /// Start at the first control, or move back to the grid.
    Focus(Option<Hit>),
    Help,
}

pub fn handle_key(state: &mut State, key: u32, _l: &Layout, focus: Option<Hit>) -> KeyAction {
    match key {
        K_ESCAPE => KeyAction::Quit,
        K_Q | K_Q_SHIFTED => KeyAction::Quit,
        K_F1 => KeyAction::Help,
        K_TAB => KeyAction::Focus(move_focus(state, focus, true)),
        K_D => {
            if state.has_pending() && focus.is_none() {
                KeyAction::Discard
            } else {
                KeyAction::Focus(move_focus(state, focus, false))
            }
        }
        K_A => KeyAction::Focus(None),
        K_RETURN => match focus {
            Some(control) => KeyAction::Activate(control),
            None => KeyAction::Apply,
        },
        K_SPACE => match focus {
            Some(control) => KeyAction::Activate(control),
            None => KeyAction::Select,
        },
        K_RIGHT | K_L => {
            let count = state.visible().len();
            if count > 0 {
                state.selected = (state.selected + 1).min(count - 1);
                ensure_visible(state, _l);
            }
            KeyAction::Redraw
        }
        K_LEFT | K_H => {
            state.selected = state.selected.saturating_sub(1);
            ensure_visible(state, _l);
            KeyAction::Redraw
        }
        K_DOWN | K_J => {
            let count = state.visible().len();
            let l = _l;
            state.selected = (state.selected + l.tiles_per_row).min(count.saturating_sub(1));
            KeyAction::Redraw
        }
        K_UP | K_K => {
            state.selected = state.selected.saturating_sub(_l.tiles_per_row);
            KeyAction::Redraw
        }
        _ => KeyAction::None,
    }
}

/// Scroll so the selected tile is on screen.
fn ensure_visible(state: &mut State, l: &Layout) {
    let row = state.selected / l.tiles_per_row;
    if row < state.scroll {
        state.scroll = row;
    } else if row >= state.scroll + l.visible_rows {
        state.scroll = row + 1 - l.visible_rows;
    }
}

/// The help text shown by F1, as lines.
pub fn help_lines() -> Vec<&'static str> {
    vec![
        "Tab / Shift+Tab     move between every control",
        "Arrows             choose a picture in the grid",
        "Space              choose the picture or press the focused control",
        "Enter              apply, or press the focused control",
        "Ctrl+D             discard the staged changes",
        "A                  return to the picture grid",
        "F1                 show or hide this help",
        "Esc or q           close the window",
        "A screen name      limits every change to that screen",
        "All screens        changes the picture every screen inherits",
    ]
}

/// The event loop. Returns when the window is closed.
pub fn run(app: &mut App, state: &mut State, _palette: Palette) -> Result<(), String> {
    // The window's own RandR list decides which screens exist, so the two
    // never disagree about a screen that was plugged in a moment ago.
    sync_screens(app, state);
    let mut focus: Option<Hit> = None;
    let mut help = false;
    let mut dirty = true;
    request_visible_previews(app, state);
    loop {
        let mut handled = false;
        while let Some(event) = app.c.poll_for_event().map_err(|e| e.to_string())? {
            handled = true;
            dirty = true;
            let came_from_user = matches!(
                event,
                Event::KeyPress(_) | Event::ButtonPress(_) | Event::MotionNotify(_)
            );
            if came_from_user {
                app.last_activity = Instant::now();
            }
            match event {
                Event::Expose(e) if e.count == 0 => {}
                Event::ConfigureNotify(e) => {
                    if e.width != app.w || e.height != app.h {
                        app.w = e.width.max(MIN_WIDTH);
                        app.h = e.height.max(MIN_HEIGHT);
                        app.resize_backing()?;
                    }
                }
                Event::KeyPress(e) => {
                    let l = layout(app.w, app.h);
                    let key = app.keys.sym(e.detail);
                    // Ctrl+D discards; plain D moves focus back. Neither
                    // shadows the plain letters people already use here.
                    let ctrl = e.state.contains(KeyButMask::CONTROL);
                    if ctrl && key == K_D {
                        state.pending = Pending::default();
                        state.set_message("Discarded staged changes", false);
                        focus = None;
                        help = false;
                        continue;
                    }
                    if help {
                        help = false;
                        focus = None;
                        continue;
                    }
                    if e.state.contains(KeyButMask::SHIFT) && key == K_TAB {
                        focus = move_focus(state, focus, false);
                        continue;
                    }
                    match handle_key(state, key, &l, focus) {
                        KeyAction::Quit => return Ok(()),
                        KeyAction::Apply => {
                            run_apply(app, state, focus)?;
                            focus = None;
                        }
                        KeyAction::Select => select_current(state),
                        KeyAction::Discard => {
                            state.pending = Pending::default();
                            state.set_message("Discarded staged changes", false);
                        }
                        KeyAction::Activate(Hit::Apply) => {
                            run_apply(app, state, focus)?;
                            focus = None;
                        }
                        KeyAction::Activate(control) => activate(state, control),
                        KeyAction::Focus(next) => focus = next,
                        KeyAction::Help => {
                            help = true;
                            focus = None;
                        }
                        KeyAction::Redraw | KeyAction::None => {}
                    }
                }
                Event::ButtonPress(e) => {
                    match hit(app.w, app.h, e.event_x, e.event_y, state) {
                        Some(Hit::Tile(i)) => {
                            state.selected = i;
                            select_current(state);
                            focus = None;
                        }
                        Some(Hit::Apply) => {
                            run_apply(app, state, None)?;
                            focus = None;
                        }
                        Some(Hit::Cancel) => {
                            activate(state, Hit::Cancel);
                            focus = None;
                        }
                        Some(control) => {
                            // A click on a setting changes it once, the same
                            // as pressing Space on it.
                            activate(state, control);
                            focus = None;
                        }
                        None => {}
                    }
                }
                Event::ClientMessage(e) => {
                    if e.type_ == atom_id(&app.atoms, "WM_PROTOCOLS") {
                        return Ok(());
                    }
                }
                Event::RandrScreenChangeNotify(_) => {
                    app.screens_dirty = true;
                }
                _ => dirty = false,
            }
        }
        if app.collect_thumbs() {
            dirty = true;
        }
        if app.screens_dirty || app.reread_screens() {
            app.screens_dirty = false;
            app.last_activity = Instant::now();
            if let Err(err) = refresh(state) {
                state.set_message(err, true);
            }
            dirty = true;
        } else if !handled && app.last_activity.elapsed() >= REFRESH {
            app.last_activity = Instant::now();
            if app.reread_screens() {
                sync_screens(app, state);
            }
            if let Err(err) = refresh(state) {
                state.set_message(err, true);
            }
            dirty = true;
        }
        if dirty {
            app.palette = state.palette;
            if help {
                paint_help(app)?;
            } else {
                app.paint(state, focus)?;
            }
            app.flush()?;
            dirty = false;
        } else {
            app.c.flush().map_err(|e| e.to_string())?;
        }
        request_visible_previews(app, state);
        if !handled {
            wait_for_input(app)?;
        }
    }
}

/// Add or remove the screens the window knows about in the desktop state.
///
/// A screen that disappeared is dropped from the plan and the selection falls
/// back to all screens; its saved settings stay in the file for a reconnect.
pub fn sync_screens(app: &App, state: &mut State) {
    sync_screen_names(app.screens.iter().map(|s| s.name.clone()), state);
}

/// The same, from names alone, so a test needs no X connection.
pub fn sync_screen_names(names: impl IntoIterator<Item = String>, state: &mut State) {
    let names: Vec<String> = names.into_iter().collect();
    let mut plan = Vec::new();
    for name in &names {
        if let Some(existing) = state.status.screens.iter().find(|s| &s.output == name) {
            plan.push(existing.clone());
            continue;
        }
        // A fresh screen inherits the common settings until it is set.
        let overs = state.status.override_for(name).cloned();
        plan.push(backend::ScreenPlan {
            output: name.clone(),
            source: if overs.as_ref().is_some_and(|o| !o.inherits()) {
                "override".into()
            } else {
                "inherit".into()
            },
            kind: state.status.mode.clone(),
            scale: state
                .status
                .override_for(name)
                .map(|o| o.scale.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| state.status.scale.clone()),
            image: String::new(),
        });
    }
    state.status.screens = plan;
    let gone = match &state.target {
        Target::Screen(name) if !names.contains(name) => Some(name.clone()),
        _ => None,
    };
    if let Some(name) = gone {
        state.target = Target::All;
        state.pending = Pending::default();
        state.set_message(
            format!("{name} is no longer connected; settings now apply to all screens"),
            false,
        );
    }
}

/// Block until the server has something to say or the refresh is due.
///
/// The connection is owned by this thread, so the window waits on its socket
/// rather than spinning: a keypress wakes it immediately, and a theme change
/// made elsewhere is noticed on the next refresh.
fn wait_for_input(app: &App) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;
    let fd = app.c.stream().as_raw_fd();
    let remaining = REFRESH
        .checked_sub(app.last_activity.elapsed())
        .unwrap_or(Duration::from_millis(20))
        .max(Duration::from_millis(20));
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = remaining.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: one pollfd describing the live X socket is passed.
    let _ = unsafe { libc::poll(&mut poll, 1, ms) };
    Ok(())
}

/// Show "applying", run the transaction, and leave the result in the footer.
fn run_apply(app: &mut App, state: &mut State, focus: Option<Hit>) -> Result<(), String> {
    if state.busy {
        return Ok(());
    }
    state.busy = true;
    app.paint(state, focus)?;
    app.flush()?;
    let result = apply(state);
    state.busy = false;
    if let Err(err) = result {
        state.set_message(err, true);
    }
    app.paint(state, focus)?;
    app.flush()?;
    Ok(())
}

/// Activating a control changes the staged settings. Apply is handled by the
/// caller, which runs the transaction.
fn activate(state: &mut State, control: Hit) {
    match control {
        Hit::Tab(tab) => {
            state.tab = tab;
            state.selected = 0;
            state.scroll = 0;
        }
        Hit::Screen(i) => {
            let targets = targets(state);
            if let Some(target) = targets.get(i as usize) {
                if state.target != *target {
                    state.target = target.clone();
                    state.pending = Pending::default();
                }
            }
        }
        Hit::Tile(i) => {
            if i < state.visible().len() {
                state.selected = i;
                select_current(state);
            }
        }
        Hit::Scale(i) => {
            state.pending.scale = Some(SCALES[i as usize].to_string());
        }
        Hit::Rotation(i) => {
            if state.status.rotation_possible() {
                state.pending.rotation = Some(ROTATIONS[i as usize]);
            } else {
                state.set_message(
                    "A single picture keeps one image; rotation would not take effect",
                    true,
                );
            }
        }
        Hit::FollowTheme => {
            let now = state
                .pending
                .follow_theme
                .unwrap_or(state.status.follow_theme);
            state.pending.follow_theme = Some(!now);
            if !now {
                state.pending.image = None;
                state.pending.inherit = false;
                state.set_message("Follow the theme wallpaper", false);
            } else {
                state.set_message("Use a chosen picture instead", false);
            }
        }
        Hit::ScreenFollow => {
            if !state.override_target() {
                state.set_message(
                    if state.status.follow_theme {
                        "All screens already follow the theme"
                    } else {
                        "Turn on Follow theme for every screen"
                    },
                    false,
                );
                return;
            }
            state.pending.inherit = true;
            state.pending.image = None;
            state.pending.scale = None;
            state.set_message("Stage: follow the shared settings", false);
        }
        Hit::Apply => {}
        Hit::Cancel => {
            state.pending = Pending::default();
            state.set_message("Discarded staged changes", false);
        }
    }
}

/// Ask the workers for the previews of the tiles that are on screen.
fn request_visible_previews(app: &mut App, state: &State) {
    let l = layout(app.w, app.h);
    let first = state.scroll * l.tiles_per_row;
    let last = first + l.tiles_per_row * l.visible_rows;
    let paths: Vec<PathBuf> = state
        .visible()
        .iter()
        .enumerate()
        .filter(|(i, _)| *i >= first && *i < last)
        .map(|(_, entry)| entry.path.clone())
        .collect();
    for path in paths {
        app.want_thumb(&path);
    }
}

/// The help view: the same window, the controls on screen.
fn paint_help(app: &App) -> Result<(), String> {
    app.fill(
        app.palette.background,
        Rectangle {
            x: 0,
            y: 0,
            width: app.w,
            height: app.h,
        },
    )?;
    app.label(
        PAD,
        PAD - 8,
        "NIRU Wallpaper — keys",
        app.palette.foreground,
        18.0,
    )?;
    for (i, line) in help_lines().iter().enumerate() {
        app.label(PAD, PAD + 40 + i as i16 * 26, line, app.palette.muted, 13.0)?;
    }
    app.label(
        PAD,
        app.h as i16 - 40,
        "Press any key to return",
        app.palette.muted,
        12.0,
    )?;
    app.flush()
}
