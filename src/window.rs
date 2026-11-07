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
use crate::import;
use crate::keyboard;
use crate::preview;
use crate::screens::{self, Screen};
use crate::theme::{self, Palette};
use crate::thumbs;
use crate::watch;

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
/// Space between the bottom of the grid and the status line.
const GRID_GAP: i16 = 4;
/// The settings panel's height when it sits below the grid.
const PANEL_COMPACT_H: i16 = 520;
/// Width kept free at the grid's right edge for the scrollbar.
const SCROLLBAR_ROOM: i16 = 20;

/// The root window property NIRUWM publishes its rendered state in.
const WALLPAPER_STATE: &str = "_NIRUWM_WALLPAPER_STATE";

/// Previews kept in memory: a few screens' worth of tiles at 320 pixels.
const PREVIEW_CACHE: usize = 96;
/// How long an unreadable file waits before it is tried again.
const PREVIEW_RETRY: Duration = Duration::from_secs(10);

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
    /// Where each connected screen is, from RandR, for the screen map.
    pub geometry: Vec<Screen>,
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
        geometry: Vec::new(),
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
    const NAMES: [&str; 6] = [
        "UTF8_STRING",
        "WM_PROTOCOLS",
        "WM_DELETE_WINDOW",
        "_NET_WM_NAME",
        "_NET_WM_WINDOW_TYPE",
        WALLPAPER_STATE,
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
    /// The picture's own size, for the scale preview.
    pub dims: Option<(u32, u32)>,
    pub stamp: Option<(u64, std::time::SystemTime)>,
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
    /// Decoded previews, so a repaint never decodes an image. A failure is
    /// kept too, so a broken file is not retried on every repaint.
    thumbs: thumbs::Cache,
    /// Previews a worker is making now, so one is never asked for twice.
    in_flight: std::collections::HashSet<PathBuf>,
    /// The screens RandR reports now.
    pub screens: Vec<Screen>,
    /// The screens seen on the last refresh, so a cable change is noticed.
    seen_screens: String,
    /// The screen index of the X connection, kept for re-reading RandR.
    xscreen: usize,
    requests: Sender<(u64, PathBuf)>,
    results: Receiver<(u64, Thumb)>,
    request_serial: u64,
    /// The files whose changes can change `niructl`'s answer.
    watcher: watch::Watcher,
    /// Debounces those changes into one refresh.
    debounce: watch::Debounce,
    /// Workers write here to wake the event loop.
    wake: Arc<watch::Wake>,
    /// The Apply in progress and the channel its worker answers on.
    apply_job: ApplyJob,
    apply_done: Sender<(u64, ApplyResult)>,
    apply_results: Receiver<(u64, ApplyResult)>,
    /// Copies made by "Add pictures" come back on this channel.
    import_done: Sender<Vec<Result<PathBuf, String>>>,
    import_results: Receiver<Vec<Result<PathBuf, String>>>,
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
                        | EventMask::BUTTON_RELEASE
                        // Motion only while a button is held, for dragging
                        // the scrollbar; hovering wakes nothing.
                        | EventMask::BUTTON1_MOTION
                        | EventMask::STRUCTURE_NOTIFY
                        | EventMask::FOCUS_CHANGE,
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
        // A resizable window with a preferred size and the smallest size the
        // layout supports, so it stays usable on a small screen and gains room
        // on a wide one.
        let hints = normal_hints();
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

        // NIRUWM publishes what it rendered as a root window property; its
        // change is the cue to look again, so the root's property changes
        // are selected too.
        c.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|e| e.to_string())?;
        let watcher = watch::Watcher::new()?;
        let wake = Arc::new(watch::Wake::new()?);

        let (requests, work) = mpsc::channel::<(u64, PathBuf)>();
        let (done, results) = mpsc::channel::<(u64, Thumb)>();
        spawn_workers(work, done, wake.clone());
        let (apply_done, apply_results) = mpsc::channel::<(u64, ApplyResult)>();
        let (import_done, import_results) = mpsc::channel();

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
            thumbs: thumbs::Cache::new(PREVIEW_CACHE),
            in_flight: std::collections::HashSet::new(),
            screens,
            seen_screens,
            xscreen: screen,
            requests,
            results,
            request_serial: 0,
            watcher,
            debounce: watch::Debounce::default(),
            wake,
            apply_job: ApplyJob::default(),
            apply_done,
            apply_results,
            import_done,
            import_results,
            screens_dirty: false,
        })
    }

    /// Watch the directories the current report names.
    fn watch(&mut self, state: &State) {
        self.watcher.set(watch::watched_dirs(
            &state.status.dir,
            &state.status.theme_dir,
        ));
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

    /// Limit drawing to one rectangle, or lift the limit.
    fn clip(&self, rect: Option<Rectangle>) -> Result<(), String> {
        match rect {
            Some(r) => self
                .c
                .set_clip_rectangles(ClipOrdering::UNSORTED, self.gc, 0, 0, &[r])
                .map_err(|e| e.to_string())
                .map(drop),
            None => self
                .c
                .change_gc(self.gc, &ChangeGCAux::new().clip_mask(x11rb::NONE))
                .map_err(|e| e.to_string())
                .map(drop),
        }
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

    /// Ask a worker for a preview if it is neither decoded, nor being made,
    /// nor recently found unreadable.
    fn want_thumb(&mut self, path: &Path) {
        if let Some(entry) = self.thumbs.get(path) {
            if entry.image.is_some() || entry.at.elapsed() < PREVIEW_RETRY {
                self.thumbs.touch(path);
                return;
            }
        }
        if !self.in_flight.insert(path.to_path_buf()) {
            return;
        }
        self.request_serial += 1;
        let serial = self.request_serial;
        let _ = self.requests.send((serial, path.to_path_buf()));
    }

    /// Collect finished previews without blocking.
    fn collect_thumbs(&mut self) -> bool {
        let mut changed = false;
        while let Ok((_, thumb)) = self.results.try_recv() {
            self.in_flight.remove(&thumb.path);
            self.thumbs
                .insert(thumb.path, thumb.image, thumb.dims, thumb.stamp);
            changed = true;
        }
        changed
    }

    fn thumb(&self, path: &Path) -> Option<&image::RgbaImage> {
        self.thumbs.get(path).and_then(|t| t.image.as_ref())
    }

    /// True when the file was tried and cannot be shown.
    fn unreadable(&self, path: &Path) -> bool {
        self.thumbs.get(path).is_some_and(|t| t.image.is_none())
    }

    /// Draw the whole window. One function so a click and a repaint can never
    /// disagree about where a control is.
    pub fn paint(&self, state: &State, focus: Option<Hit>, dragging: bool) -> Result<(), String> {
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
            // The screen choice only exists when there is more than one
            // screen: "All screens", then a small map of the screens as
            // they are arranged, each one a button.
            self.paint_screen_map(state, &l)?;
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
        // Tiles are clipped to the grid, so a partly visible row can never
        // be drawn over the status line, the key hints or the header.
        self.clip(Some(l.left))?;
        for (i, entry) in visible.iter().enumerate() {
            let rect = tile_rect(&l, i, state.scroll);
            if !intersects(rect, l.left) {
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
                    let text = if self.unreadable(&entry.path) {
                        "cannot read image"
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
        self.clip(None)?;
        let buttons = mine_buttons(&l, state);
        let label_room = buttons.map_or(l.left.width as i16, |(add, _)| add.x - l.left.x - 16);
        self.label(
            l.left.x + 8,
            l.left.y - 24,
            &truncate(&position_label(&l, state), (label_room / 7).max(0) as usize),
            self.palette.muted,
            12.0,
        )?;
        if let Some((add, open)) = buttons {
            self.button(add, "Add pictures\u{2026}", false, !state.busy)?;
            self.button(open, "Open folder", false, true)?;
        }
        if let Some((track, thumb)) = scrollbar(&l, state) {
            self.fill(self.palette.surface, track)?;
            self.fill(
                if dragging {
                    self.palette.foreground
                } else {
                    self.palette.border
                },
                thumb,
            )?;
        }

        // The panel is clipped above the status line too, for a window
        // that is shorter than the layout wants.
        self.clip(Some(Rectangle {
            height: (l.footer - l.panel.y).clamp(0, l.panel.height as i16) as u16,
            ..l.panel
        }))?;
        self.paint_panel(state, &l, focus)?;
        self.clip(None)?;

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
            &truncate(
                if l.compact {
                    "Tab move  Space choose  Enter apply  F1 help  Esc close"
                } else {
                    "Tab move  arrows select  Space choose  Enter apply  Ctrl+D discard  F1 help  Esc/q close"
                },
                (self.w as usize / 7).saturating_sub(2),
            ),
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

        self.label(x, l.panel.y + 76, "Scale", self.palette.muted, 12.0)?;
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
            l.panel.y + 124,
            &truncate(&format!("In effect: {effective}"), 38),
            self.palette.muted,
            11.0,
        )?;

        self.label(
            x,
            l.panel.y + 144,
            if state.status.rotation_possible() {
                "Rotate (minutes)"
            } else {
                "Rotate (minutes): a single picture ignores it"
            },
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
                l.panel.y + 230 + i as i16 * 18,
                &truncate(line, 42),
                if i == 0 {
                    self.palette.foreground
                } else {
                    self.palette.muted
                },
                11.0,
            )?;
        }

        self.paint_preview(state, l, &scale)?;

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

    fn paint_screen_map(&self, state: &State, l: &Layout) -> Result<(), String> {
        let targets = targets(state);
        let rects = screen_rects(l, state);
        let mut any_own = false;
        for (i, rect) in rects.iter().enumerate() {
            let target = &targets[i];
            let chosen = state.target == *target;
            let Target::Screen(name) = target else {
                self.button(*rect, "All screens", chosen, true)?;
                continue;
            };
            let own = screen_has_own(state, name);
            any_own |= own;
            let (bg, fg) = if chosen {
                (self.palette.accent, self.palette.background)
            } else {
                (self.palette.surface, self.palette.foreground)
            };
            self.fill(self.palette.border, outset(*rect, 1))?;
            self.fill(bg, *rect)?;
            if own {
                // A corner mark: this screen has its own picture.
                self.fill(
                    if chosen {
                        self.palette.background
                    } else {
                        self.palette.accent
                    },
                    Rectangle {
                        x: rect.x + rect.width as i16 - 9,
                        y: rect.y + 3,
                        width: 6,
                        height: 6,
                    },
                )?;
            }
            let room = (rect.width as usize).saturating_sub(14) / 6;
            if room >= 3 && rect.height >= 16 {
                self.label_on(rect.x + 4, rect.y + 2, &truncate(name, room), fg, bg, 10.0)?;
            }
        }
        if any_own {
            if let Some(all) = rects.first() {
                // Above "All screens" when the map is beside it, else beside
                // it; clear of the grid's header row either way.
                let map_above = rects
                    .iter()
                    .skip(1)
                    .all(|r| r.y + (r.height as i16) <= all.y);
                let (x, y) = if map_above {
                    (all.x + all.width as i16 + 8, all.y + 8)
                } else {
                    (all.x, all.y - 18)
                };
                self.fill(
                    self.palette.accent,
                    Rectangle {
                        x,
                        y: y + 4,
                        width: 6,
                        height: 6,
                    },
                )?;
                self.label(x + 10, y, "own picture", self.palette.muted, 10.0)?;
            }
        }
        Ok(())
    }

    /// The staged picture on a small copy of the chosen screen, scaled the
    /// way the selected mode would, so the scale buttons show their effect
    /// before Apply.
    fn paint_preview(&self, state: &State, l: &Layout, scale: &str) -> Result<(), String> {
        let Some(frame) = preview_rect(l) else {
            return Ok(());
        };
        let (name, size) = preview_screen(state, &self.screens);
        let image = preview_image(state);
        self.label(
            frame.x,
            frame.y - 18,
            &truncate(
                &format!("Preview: {scale} on {name} {}\u{d7}{}", size.0, size.1),
                (frame.width as usize / 6).max(8),
            ),
            self.palette.muted,
            11.0,
        )?;
        let area = |r: Rectangle| preview::Area {
            x: i32::from(r.x),
            y: i32::from(r.y),
            w: i32::from(r.width),
            h: i32::from(r.height),
        };
        let on = preview::screen_in_frame(area(frame), size);
        let screen = Rectangle {
            x: on.x as i16,
            y: on.y as i16,
            width: on.w as u16,
            height: on.h as u16,
        };
        // The screen's own background shows wherever the picture does not
        // reach (fit, center), as the theme colour does on the desktop.
        self.fill(self.palette.border, outset(screen, 1))?;
        self.fill(self.palette.background, screen)?;
        let entry = image.as_deref().and_then(|p| self.thumbs.get(p));
        match (image.as_deref(), entry) {
            (None, _) => {
                self.label(
                    screen.x + 6,
                    screen.y + 6,
                    "No picture: the theme colour",
                    self.palette.muted,
                    11.0,
                )?;
            }
            (
                Some(_),
                Some(thumbs::Entry {
                    image: Some(thumb),
                    dims,
                    ..
                }),
            ) => {
                let dims = dims.unwrap_or((thumb.width(), thumb.height()));
                let placed = preview::place(on, size, dims, scale);
                if let Some(visible) = placed.intersect(on) {
                    self.image_part(placed, visible, thumb)?;
                }
            }
            (Some(_), entry) => {
                let text = if entry.is_some() {
                    "cannot read image"
                } else {
                    "Loading preview"
                };
                self.label(screen.x + 6, screen.y + 6, text, self.palette.muted, 11.0)?;
            }
        }
        Ok(())
    }

    /// Draw the part `visible` of a picture placed at `placed`, from its
    /// preview.
    fn image_part(
        &self,
        placed: preview::Area,
        visible: preview::Area,
        thumb: &image::RgbaImage,
    ) -> Result<(), String> {
        let (tw, th) = (i64::from(thumb.width()), i64::from(thumb.height()));
        let sx = |x: i32| (i64::from(x - placed.x) * tw / i64::from(placed.w.max(1))).clamp(0, tw);
        let sy = |y: i32| (i64::from(y - placed.y) * th / i64::from(placed.h.max(1))).clamp(0, th);
        let (x0, y0) = (sx(visible.x), sy(visible.y));
        let (x1, y1) = (
            sx(visible.x + visible.w).max(x0 + 1),
            sy(visible.y + visible.h).max(y0 + 1),
        );
        let crop = image::imageops::crop_imm(
            thumb,
            x0 as u32,
            y0 as u32,
            (x1 - x0).min(tw - x0).max(1) as u32,
            (y1 - y0).min(th - y0).max(1) as u32,
        )
        .to_image();
        let scaled = image::imageops::resize(
            &crop,
            visible.w as u32,
            visible.h as u32,
            image::imageops::FilterType::Triangle,
        );
        self.upload(
            visible.x as i16,
            visible.y as i16,
            visible.w as u16,
            visible.h as u16,
            scaled.as_raw(),
        )
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
fn spawn_workers(
    work: Receiver<(u64, PathBuf)>,
    done: Sender<(u64, Thumb)>,
    wake: Arc<watch::Wake>,
) {
    let work = Arc::new(Mutex::new(work));
    for _ in 0..2 {
        let work = work.clone();
        let done = done.clone();
        let wake = wake.clone();
        thread::spawn(move || {
            let take = |work: &Arc<Mutex<Receiver<(u64, PathBuf)>>>| {
                work.lock().ok().and_then(|rx| rx.recv().ok())
            };
            loop {
                let Ok((serial, path)) = take(&work).ok_or(()) else {
                    return;
                };
                let stamp = thumbs::stamp(&path);
                let image = thumbs::decode_preview(&path);
                let dims = image
                    .as_ref()
                    .and_then(|_| image::image_dimensions(&path).ok());
                let thumb = Thumb {
                    path,
                    image,
                    dims,
                    stamp,
                };
                if done.send((serial, thumb)).is_err() {
                    return;
                }
                wake.notify();
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
    /// The grid's scrollbar track.
    Scrollbar,
    /// "Add pictures…" under My wallpapers.
    AddPictures,
    /// "Open folder" under My wallpapers.
    OpenFolder,
}

/// The layout for one window size.
pub struct Layout {
    pub left: Rectangle,
    pub panel: Rectangle,
    pub footer: i16,
    /// True when the window is too narrow for the panel beside the grid,
    /// and it sits below the grid instead.
    pub compact: bool,
    pub tiles_per_row: usize,
    pub visible_rows: usize,
    pub tabs: (Rectangle, Rectangle),
    /// The y of the header tabs, and of the screen choice beneath them.
    pub screen_row: i16,
}

pub fn layout(w: u16, h: u16) -> Layout {
    let w = i16::try_from(w).unwrap_or(i16::MAX).max(1);
    let h = i16::try_from(h).unwrap_or(i16::MAX).max(1);
    let top = HEADER + ROW;
    let footer = (h - FOOTER).max(top + 1);
    // Below the width the side-by-side layout needs (a narrow tiled column),
    // the settings panel moves below the grid instead of being cut off.
    let compact = w < MIN_WIDTH as i16;
    let (left, panel) = if compact {
        let width = (w - PAD * 2).max(1);
        let panel_h = PANEL_COMPACT_H;
        let grid_h = (footer - top - GRID_GAP - panel_h - GRID_GAP).max(TILE_H + 8);
        (
            Rectangle {
                x: PAD,
                y: top,
                width: width as u16,
                height: grid_h as u16,
            },
            Rectangle {
                x: PAD,
                y: top + grid_h + GRID_GAP,
                width: width as u16,
                height: panel_h as u16,
            },
        )
    } else {
        let list_w = w - PAD * 3 - PANEL_W;
        // The grid ends a little above the status line, and only rows that
        // fit completely count as visible; a partly visible row is clipped.
        let grid_h = (footer - top - GRID_GAP).max(1);
        (
            Rectangle {
                x: PAD,
                y: top,
                width: (list_w + PAD).max(1) as u16,
                height: grid_h as u16,
            },
            Rectangle {
                x: w - PAD - PANEL_W,
                y: HEADER - 20,
                width: PANEL_W as u16,
                height: (footer - HEADER + 20).max(1) as u16,
            },
        )
    };
    // Room for the scrollbar at the grid's right edge.
    let tiles_w = left.width as i16 - SCROLLBAR_ROOM;
    let tiles_per_row = (tiles_w / TILE_W).max(1) as usize;
    let visible_rows = ((left.height as i16 - 8) / TILE_H).max(1) as usize;
    Layout {
        left,
        panel,
        footer,
        compact,
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
        Hit::Screen(i) => *screen_rects(l, state).get(i as usize)?,
        Hit::Tile(i) => tile_rect(l, i, state.scroll),
        Hit::Scale(i) => *scale_rects(l).get(i as usize)?,
        Hit::Rotation(i) => *rotation_rects(l).get(i as usize)?,
        Hit::FollowTheme => follow_rect(l),
        Hit::ScreenFollow => settings_rect(l),
        Hit::Apply => apply_rect(l),
        Hit::Cancel => cancel_rect(l),
        Hit::Scrollbar => scrollbar(l, state)?.0,
        Hit::AddPictures => mine_buttons(l, state)?.0,
        Hit::OpenFolder => mine_buttons(l, state)?.1,
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
        for (i, rect) in screen_rects(&l, state).iter().enumerate() {
            if inside(*rect, x, y) {
                return Some(Hit::Screen(i as u8));
            }
        }
    }
    // Tiles, in the order they are painted.
    if let Some((add, open)) = mine_buttons(&l, state) {
        if inside(add, x, y) {
            return Some(Hit::AddPictures);
        }
        if inside(open, x, y) {
            return Some(Hit::OpenFolder);
        }
    }
    if let Some((track, _)) = scrollbar(&l, state) {
        // A little wider than it is drawn, so it is easy to take.
        let grab = Rectangle {
            x: track.x - 4,
            width: track.width + 8,
            ..track
        };
        if inside(grab, x, y) {
            return Some(Hit::Scrollbar);
        }
    }
    // A tile is only clickable where it is drawn: inside the grid.
    if inside(l.left, x, y) {
        let count = state.visible().len();
        for i in 0..count {
            let rect = tile_rect(&l, i, state.scroll);
            if inside(rect, x, y) {
                return Some(Hit::Tile(i));
            }
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

/// True when a screen has a picture of its own instead of the shared one.
pub fn screen_has_own(state: &State, name: &str) -> bool {
    state
        .status
        .override_for(name)
        .is_some_and(|o| !o.inherits())
        || state.screen_plan(name).is_some_and(|p| p.overrides())
}

/// The screen choice in the header: "All screens" first, then one
/// rectangle per screen, drawn to scale where RandR puts it.
pub fn screen_rects(l: &Layout, state: &State) -> Vec<Rectangle> {
    let x0 = l.tabs.1.x + l.tabs.1.width as i16 + 20;
    let all = Rectangle {
        x: x0,
        y: l.screen_row,
        width: 100,
        height: 28,
    };
    // The panel's right edge is the window's right margin in both layouts.
    let right = l.panel.x + l.panel.width as i16;
    let beside = right - (x0 + 110);
    // Beside "All screens" when there is room, else above it.
    let region = if beside >= 120 {
        Rectangle {
            x: x0 + 110,
            y: 8,
            width: beside as u16,
            height: (l.screen_row + 28 - 8).min(68) as u16,
        }
    } else {
        Rectangle {
            x: x0,
            y: 6,
            width: (right - x0).max(24) as u16,
            height: (l.screen_row - 12).max(16) as u16,
        }
    };
    let names = state.screens();
    let geometry: Vec<Option<&Screen>> = names
        .iter()
        .map(|n| state.geometry.iter().find(|s| &s.name == n))
        .collect();
    let mut out = vec![all];
    out.extend(screen_map(region, &geometry));
    out
}

/// Scale screens into `region`, keeping their arrangement. A screen without
/// geometry gets a 16:9 place to the right of the others; mirrored screens
/// (the same area) share it side by side.
pub fn screen_map(region: Rectangle, screens: &[Option<&Screen>]) -> Vec<Rectangle> {
    // Fill in made-up geometry for screens RandR did not describe.
    let mut boxes: Vec<(i64, i64, i64, i64)> = Vec::new();
    let mut next_x = screens
        .iter()
        .flatten()
        .map(|s| i64::from(s.x) + i64::from(s.width))
        .max()
        .unwrap_or(0);
    for screen in screens {
        match screen {
            Some(s) => boxes.push((
                i64::from(s.x),
                i64::from(s.y),
                i64::from(s.width.max(1)),
                i64::from(s.height.max(1)),
            )),
            None => {
                boxes.push((next_x, 0, 1920, 1080));
                next_x += 1920;
            }
        }
    }
    if boxes.is_empty() {
        return Vec::new();
    }
    let min_x = boxes.iter().map(|b| b.0).min().unwrap_or(0);
    let min_y = boxes.iter().map(|b| b.1).min().unwrap_or(0);
    let max_x = boxes.iter().map(|b| b.0 + b.2).max().unwrap_or(1);
    let max_y = boxes.iter().map(|b| b.1 + b.3).max().unwrap_or(1);
    let (bw, bh) = ((max_x - min_x).max(1) as f64, (max_y - min_y).max(1) as f64);
    let k = (f64::from(region.width) / bw).min(f64::from(region.height) / bh);
    // Left-aligned, next to "All screens"; centred vertically.
    let off_x = f64::from(region.x);
    let off_y = f64::from(region.y) + (f64::from(region.height) - bh * k) / 2.0;
    boxes
        .iter()
        .enumerate()
        .map(|(i, b)| {
            // Mirrored screens split their shared area into equal slices.
            let group: Vec<usize> = (0..boxes.len()).filter(|j| boxes[*j] == *b).collect();
            let slot = group.iter().position(|j| *j == i).unwrap_or(0) as f64;
            let slice = b.2 as f64 / group.len() as f64;
            let x = off_x + ((b.0 - min_x) as f64 + slice * slot) * k;
            let y = off_y + (b.1 - min_y) as f64 * k;
            let w = slice * k;
            let h = b.3 as f64 * k;
            // One pixel apart, so neighbours read as separate screens.
            Rectangle {
                x: x.round() as i16 + 1,
                y: y.round() as i16 + 1,
                width: (w.round() as i16 - 2).max(4) as u16,
                height: (h.round() as i16 - 2).max(4) as u16,
            }
        })
        .collect()
}

/// True when two rectangles overlap by at least one pixel.
fn intersects(a: Rectangle, b: Rectangle) -> bool {
    a.x < b.x + b.width as i16
        && b.x < a.x + a.width as i16
        && a.y < b.y + b.height as i16
        && b.y < a.y + a.height as i16
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

/// `count` buttons side by side across the panel, `gap` pixels apart.
fn panel_row(l: &Layout, y: i16, height: u16, count: usize, gap: i16) -> Vec<Rectangle> {
    let inner = l.panel.width as i16 - 24;
    let width = ((inner - gap * (count as i16 - 1)) / count as i16).max(1);
    (0..count)
        .map(|i| Rectangle {
            x: l.panel.x + 12 + i as i16 * (width + gap),
            y,
            width: width as u16,
            height,
        })
        .collect()
}

fn scale_rects(l: &Layout) -> Vec<Rectangle> {
    panel_row(l, l.panel.y + 94, 26, SCALES.len(), 6)
}

fn rotation_rects(l: &Layout) -> Vec<Rectangle> {
    panel_row(l, l.panel.y + 162, 24, ROTATIONS.len(), 6)
}

fn follow_rect(l: &Layout) -> Rectangle {
    Rectangle {
        x: l.panel.x + 12,
        y: l.panel.y + 194,
        width: l.panel.width.saturating_sub(24),
        height: 28,
    }
}

fn settings_rect(l: &Layout) -> Rectangle {
    Rectangle {
        x: l.panel.x + 12,
        y: l.panel.y + 40,
        width: l.panel.width.saturating_sub(24),
        height: 28,
    }
}

/// Apply and Discard share the panel's last row.
fn apply_rect(l: &Layout) -> Rectangle {
    panel_row(l, l.panel.y + l.panel.height as i16 - 52, 36, 2, 8)[0]
}

fn cancel_rect(l: &Layout) -> Rectangle {
    panel_row(l, l.panel.y + l.panel.height as i16 - 52, 36, 2, 8)[1]
}

/// The space for the staged picture's preview: between the description
/// lines and Apply. `None` when the window is too short for one.
pub fn preview_rect(l: &Layout) -> Option<Rectangle> {
    let top = l.panel.y + 306;
    let bottom = apply_rect(l).y - 10;
    (bottom - top >= 40).then(|| Rectangle {
        x: l.panel.x + 12,
        y: top,
        width: l.panel.width.saturating_sub(24),
        height: (bottom - top) as u16,
    })
}

/// A rectangle grown by `by` pixels on every side.
fn outset(r: Rectangle, by: i16) -> Rectangle {
    Rectangle {
        x: r.x - by,
        y: r.y - by,
        width: r.width + 2 * by as u16,
        height: r.height + 2 * by as u16,
    }
}

/// The picture the chosen screen (or the screens that inherit) would show
/// after Apply: the staged one, or the one shown now.
pub fn preview_image(state: &State) -> Option<PathBuf> {
    if let Some(path) = &state.pending.image {
        return Some(path.clone());
    }
    let status = &state.status;
    let theme = || (!status.theme_image.is_empty()).then(|| PathBuf::from(&status.theme_image));
    if state.pending.follow_theme == Some(true) {
        return theme();
    }
    let inherited = || {
        status
            .screens
            .iter()
            .find(|s| !s.overrides() && !s.image.is_empty())
            .map(|s| PathBuf::from(&s.image))
            .or_else(|| status.active_image().map(PathBuf::from))
    };
    match &state.target {
        Target::All => inherited(),
        Target::Screen(_) if state.pending.inherit => inherited(),
        Target::Screen(name) => state
            .screen_rendered(name)
            .map(|r| r.image.clone())
            .filter(|i| !i.is_empty())
            .or_else(|| state.screen_plan(name).map(|p| p.image.clone()))
            .filter(|i| !i.is_empty())
            .map(PathBuf::from),
    }
}

/// The screen the preview is drawn on: the chosen one, or for all screens
/// the first that inherits the shared settings. Its name and pixel size.
pub fn preview_screen(state: &State, screens: &[Screen]) -> (String, (u32, u32)) {
    let chosen = match &state.target {
        Target::Screen(name) => screens.iter().find(|s| &s.name == name),
        Target::All => screens
            .iter()
            .find(|s| state.screen_plan(&s.name).is_some_and(|p| !p.overrides()))
            .or_else(|| screens.first()),
    };
    match chosen {
        Some(s) => (s.name.clone(), (s.width, s.height)),
        None => ("screen".into(), (1920, 1080)),
    }
}

/// Stage the currently selected image for the current target.
pub fn select_current(state: &mut State) {
    if state.busy {
        // The staged change is being applied; it cannot change meanwhile.
        return;
    }
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

/// What a finished Apply brought back: the desktop's new state, or why it
/// failed.
pub type ApplyResult = Result<(Status, Vec<Wallpaper>), String>;

/// Run one change and read the desktop's answer. This is what the Apply
/// worker thread does, with `backend::apply_change` as `command`.
pub fn apply_and_fetch<F>(change: &Change, command: F) -> ApplyResult
where
    F: FnOnce(&Change) -> Result<(), String>,
{
    command(change)?;
    fetch()
}

/// Take the result of an Apply into the window state.
///
/// A failure keeps the staged changes, so Apply can be tried again.
pub fn finish_apply(state: &mut State, result: ApplyResult) {
    state.busy = false;
    let (status, list) = match result {
        Ok(answer) => answer,
        Err(err) => {
            state.set_message(err, true);
            return;
        }
    };
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
}

/// The Apply in progress, so only its own answer is taken and a late one
/// (after the window gave up waiting) is ignored.
#[derive(Debug, Default)]
pub struct ApplyJob {
    serial: u64,
    started: Option<Instant>,
}

impl ApplyJob {
    /// Start a new Apply and return its serial.
    pub fn start(&mut self, now: Instant) -> u64 {
        self.serial += 1;
        self.started = Some(now);
        self.serial
    }

    /// True when `serial` is the Apply the window is waiting for; it is then
    /// finished.
    pub fn accept(&mut self, serial: u64) -> bool {
        if self.started.is_some() && serial == self.serial {
            self.started = None;
            true
        } else {
            false
        }
    }

    /// How long the window still waits, or `None` when nothing is running.
    /// The worker's own deadline stops niructl first; this is the backstop.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        let limit = backend::APPLY_DEADLINE + backend::READ_DEADLINE * 2 + Duration::from_secs(1);
        self.started
            .map(|at| (at + limit).saturating_duration_since(now))
    }

    /// Give up waiting: whatever answer comes later is ignored.
    pub fn abandon(&mut self) {
        self.started = None;
    }
}

/// Keysyms for the keys the window answers to.
///
/// Named keys live in X's fixed range; printable keys use their Latin-1 code
/// point, so one table serves every layout.
const K_ESCAPE: u32 = 0xff1b;
const K_TAB: u32 = 0xff09;
/// Shift+Tab on most layouts.
const K_ISO_LEFT_TAB: u32 = 0xfe20;
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
const K_HOME: u32 = 0xff50;
const K_PAGE_UP: u32 = 0xff55;
const K_PAGE_DOWN: u32 = 0xff56;
const K_END: u32 = 0xff57;

/// Keycode-to-keysym table for the connected keyboard.
///
/// Every level of each key is kept, so a keypress resolves through the
/// shared Shift/AltGr/Caps Lock helpers to the symbol the layout really
/// produces. The table is read again when the keyboard mapping changes.
pub struct Keys {
    /// The keysyms of each keycode, indexed from `min`.
    table: Vec<Vec<u32>>,
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
        let table = map.keysyms.chunks(per).map(<[u32]>::to_vec).collect();
        Ok(Self {
            table,
            min: setup.min_keycode,
        })
    }

    /// The keysym this keycode produces with these modifiers.
    pub fn sym(&self, code: u8, state: KeyButMask) -> u32 {
        code.checked_sub(self.min)
            .and_then(|i| self.table.get(i as usize))
            .map(|syms| keyboard::keyboard_symbol(syms, state))
            .unwrap_or(0)
    }
}

/// The order Tab moves through, so every control is keyboard reachable.
pub fn focus_order(state: &State) -> Vec<Hit> {
    let mut out = vec![Hit::Tab(Tab::Official), Hit::Tab(Tab::Mine)];
    if state.tab == Tab::Mine {
        out.extend([Hit::AddPictures, Hit::OpenFolder]);
    }
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
            ensure_visible(state, l);
            KeyAction::Redraw
        }
        K_UP | K_K => {
            state.selected = state.selected.saturating_sub(_l.tiles_per_row);
            ensure_visible(state, _l);
            KeyAction::Redraw
        }
        K_PAGE_DOWN | K_PAGE_UP | K_HOME | K_END => {
            let count = state.visible().len();
            let page = _l.tiles_per_row * _l.visible_rows.max(1);
            state.selected = match key {
                K_PAGE_DOWN => state.selected + page,
                K_PAGE_UP => state.selected.saturating_sub(page),
                K_HOME => 0,
                _ => count,
            }
            .min(count.saturating_sub(1));
            ensure_visible(state, _l);
            KeyAction::Redraw
        }
        _ => KeyAction::None,
    }
}

/// The tiles shown now, counting a partly visible row, as "Wallpapers 1–16
/// of 24".
pub fn position_label(l: &Layout, state: &State) -> String {
    let count = state.visible().len();
    if count == 0 {
        return "No wallpapers".into();
    }
    let range = preview_range(l, state.scroll, count);
    // `preview_range` adds a row of look-ahead; the label shows what is in
    // view.
    let in_view = range.start
        ..range
            .end
            .min(range.start + l.tiles_per_row * rows_in_view(l, state.scroll));
    format!(
        "Wallpapers {}\u{2013}{} of {count}",
        in_view.start + 1,
        in_view.end.min(count)
    )
}

/// Rows that intersect the grid, a partly visible one included.
fn rows_in_view(l: &Layout, scroll: usize) -> usize {
    let per_row = l.tiles_per_row.max(1);
    let first = scroll * per_row;
    let mut rows = 0;
    while intersects(tile_rect(l, first + rows * per_row, scroll), l.left) {
        rows += 1;
    }
    rows
}

/// The scrollbar's track and thumb, when the list does not fit.
pub fn scrollbar(l: &Layout, state: &State) -> Option<(Rectangle, Rectangle)> {
    let max = max_scroll(state, l);
    if max == 0 {
        return None;
    }
    let track = Rectangle {
        x: l.left.x + l.left.width as i16 - 12,
        y: l.left.y + 8,
        width: 6,
        height: l.left.height.saturating_sub(16).max(24),
    };
    let total = total_rows(state, l).max(1);
    let thumb_h = (u32::from(track.height) * l.visible_rows as u32 / total as u32)
        .clamp(20, u32::from(track.height)) as u16;
    let travel = i32::from(track.height - thumb_h);
    let offset = travel * state.scroll.min(max) as i32 / max as i32;
    Some((
        track,
        Rectangle {
            y: track.y + offset as i16,
            height: thumb_h,
            ..track
        },
    ))
}

/// The scroll position for a thumb dragged so its top is at `thumb_top`.
pub fn scroll_for_thumb(track: Rectangle, thumb: Rectangle, max: usize, thumb_top: i16) -> usize {
    let travel = i32::from(track.height.saturating_sub(thumb.height)).max(1);
    let offset = i32::from(thumb_top - track.y).clamp(0, travel);
    // Round to the nearest row, so the thumb follows the pointer evenly.
    ((offset * max as i32 * 2 + travel) / (travel * 2)) as usize
}

/// What one pointer button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pointer {
    /// The primary button pressed a control.
    Press(Hit),
    /// The wheel scrolls the grid by this many rows.
    Scroll(i32),
    /// Shift and the wheel scroll by this many pages.
    Page(i32),
    /// Every other button, and a press outside any control.
    Ignore,
}

/// Map a button press to an action. Only button 1 selects or presses
/// anything; buttons 4 and 5 are the wheel and scroll the grid (with Shift,
/// one page); the horizontal wheel (6 and 7) and the other buttons are
/// ignored, so turning the wheel can never stage a wallpaper.
pub fn pointer_action(button: u8, shift: bool, at: Option<Hit>) -> Pointer {
    match button {
        1 => at.map_or(Pointer::Ignore, Pointer::Press),
        4 if shift => Pointer::Page(-1),
        5 if shift => Pointer::Page(1),
        4 => Pointer::Scroll(-1),
        5 => Pointer::Scroll(1),
        _ => Pointer::Ignore,
    }
}

/// The number of tile rows the current list needs.
pub fn total_rows(state: &State, l: &Layout) -> usize {
    state.visible().len().div_ceil(l.tiles_per_row.max(1))
}

/// The largest scroll position that still fills the grid.
pub fn max_scroll(state: &State, l: &Layout) -> usize {
    total_rows(state, l).saturating_sub(l.visible_rows.max(1))
}

/// Scroll the grid by whole rows without touching the selection or the
/// staged changes.
pub fn scroll_by(state: &mut State, l: &Layout, rows: i32) {
    let target = state.scroll as i64 + i64::from(rows);
    state.scroll = target.clamp(0, max_scroll(state, l) as i64) as usize;
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
        "PageUp/PageDown    move a page; Home/End to the first or last",
        "Add pictures       copy pictures from Pictures or Downloads",
        "A screen name      limits every change to that screen",
        "All screens        changes the picture every screen inherits",
    ]
}

/// What the event loop keeps between events besides the desktop state.
#[derive(Default)]
struct Ui {
    focus: Option<Hit>,
    help: bool,
    /// While the scrollbar is dragged: where in the thumb it was taken.
    drag: Option<i16>,
    /// The "Add pictures" list, while it is open.
    import: Option<ImportView>,
    dirty: bool,
}

/// Whether the window stays open after an event.
#[derive(PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

/// The event loop. Returns when the window is closed.
///
/// Nothing here runs on a timer. The loop sleeps in `poll` on the X
/// connection, the inotify watch and the workers' wake-up pipe, with a
/// timeout only while a debounced refresh is pending.
pub fn run(app: &mut App, state: &mut State, _palette: Palette) -> Result<(), String> {
    // The window's own RandR list decides which screens exist, so the two
    // never disagree about a screen that was plugged in a moment ago.
    sync_screens(app, state);
    app.watch(state);
    let mut ui = Ui {
        dirty: true,
        ..Ui::default()
    };
    request_visible_previews(app, state);
    let mut queued: Option<Event> = None;
    loop {
        loop {
            let event = match queued.take() {
                Some(event) => event,
                None => match app.c.poll_for_event().map_err(|e| e.to_string())? {
                    Some(event) => event,
                    None => break,
                },
            };
            if handle_event(app, state, &mut ui, event)? == Flow::Quit {
                return Ok(());
            }
        }
        app.wake.drain();
        if app.collect_thumbs() {
            ui.dirty = true;
        }
        if collect_apply(app, state) {
            ui.dirty = true;
        }
        if collect_import(app, state, &mut ui.import) {
            ui.dirty = true;
        }
        if app.watcher.drain() {
            app.debounce.note(Instant::now());
        }
        if app.screens_dirty {
            // A monitor change is refreshed at once: the screen choice and
            // the per-screen plan depend on it.
            app.screens_dirty = false;
            app.reread_screens();
            refresh_now(app, state);
            ui.dirty = true;
        } else if !state.busy && app.debounce.take(Instant::now()) {
            refresh_now(app, state);
            ui.dirty = true;
        }
        if ui.dirty {
            // A resize or a shorter list can leave the grid scrolled past
            // its end.
            let l = layout(app.w, app.h);
            state.scroll = state.scroll.min(max_scroll(state, &l));
            app.palette = state.palette;
            if ui.help {
                paint_help(app)?;
            } else if let Some(view) = &ui.import {
                paint_import(app, view)?;
            } else {
                app.paint(state, ui.focus, ui.drag.is_some())?;
            }
            app.flush()?;
            ui.dirty = false;
        }
        request_visible_previews(app, state);
        app.c.flush().map_err(|e| e.to_string())?;
        // Painting waits for replies, which can queue events without the
        // socket being readable; handle those before sleeping.
        if let Some(event) = app.c.poll_for_event().map_err(|e| e.to_string())? {
            queued = Some(event);
            continue;
        }
        let timeout = if state.busy {
            // Waiting for the Apply worker, which wakes the loop itself.
            app.apply_job.remaining(Instant::now())
        } else {
            app.debounce.timeout(Instant::now())
        };
        wait_for_input(app, timeout);
    }
}

/// Take a fresh report from the desktop now, and follow it everywhere the
/// window keeps something derived from it.
fn refresh_now(app: &mut App, state: &mut State) {
    if let Err(err) = refresh(state) {
        state.set_message(err, true);
    }
    sync_screens(app, state);
    app.thumbs.forget_changed();
    app.watch(state);
}

/// Handle one X event.
fn handle_event(
    app: &mut App,
    state: &mut State,
    ui: &mut Ui,
    event: Event,
) -> Result<Flow, String> {
    match event {
        Event::Expose(e) if e.count == 0 => ui.dirty = true,
        Event::ConfigureNotify(e) => {
            if e.width != app.w || e.height != app.h {
                // Paint for the real size: a tiling window manager may give
                // less than the size hints ask for.
                app.w = e.width.max(1);
                app.h = e.height.max(1);
                app.resize_backing()?;
                ui.dirty = true;
            }
        }
        // Coming back to the window is when a change made elsewhere is
        // most likely; grabs (a window manager shortcut) are not a return.
        Event::FocusIn(e) if e.mode == NotifyMode::NORMAL && e.detail != NotifyDetail::POINTER => {
            app.debounce.note(Instant::now());
        }
        // NIRUWM published what it rendered.
        Event::PropertyNotify(e)
            if e.window == app.root && e.atom == atom_id(&app.atoms, WALLPAPER_STATE) =>
        {
            app.debounce.note(Instant::now());
        }
        Event::KeyPress(e) => {
            ui.dirty = true;
            let l = layout(app.w, app.h);
            // Shortcuts compare letters in lower case, so Caps Lock and
            // Shift do not change which shortcut a key is.
            let key = keyboard::shortcut(app.keys.sym(e.detail, e.state));
            // Ctrl+D discards; plain D moves focus back. Neither
            // shadows the plain letters people already use here.
            let ctrl = e.state.contains(KeyButMask::CONTROL);
            if ctrl && key == K_D {
                if !state.busy {
                    state.pending = Pending::default();
                    state.set_message("Discarded staged changes", false);
                }
                ui.focus = None;
                ui.help = false;
                return Ok(Flow::Continue);
            }
            if ui.help {
                ui.help = false;
                ui.focus = None;
                return Ok(Flow::Continue);
            }
            if let Some(view) = ui.import.as_mut() {
                match import_key(view, key, &import_layout(app.w, app.h)) {
                    ImportAction::Copy => start_import(app, view),
                    ImportAction::Close => ui.import = None,
                    ImportAction::None => {}
                }
                return Ok(Flow::Continue);
            }
            if key == K_ISO_LEFT_TAB || e.state.contains(KeyButMask::SHIFT) && key == K_TAB {
                ui.focus = move_focus(state, ui.focus, false);
                return Ok(Flow::Continue);
            }
            match handle_key(state, key, &l, ui.focus) {
                KeyAction::Quit => return Ok(Flow::Quit),
                KeyAction::Apply => {
                    run_apply(app, state);
                    ui.focus = None;
                }
                KeyAction::Select => select_current(state),
                KeyAction::Discard => activate(state, Hit::Cancel),
                KeyAction::Activate(control) => press(app, state, ui, control),
                KeyAction::Focus(next) => ui.focus = next,
                KeyAction::Help => {
                    ui.help = true;
                    ui.focus = None;
                }
                KeyAction::Redraw | KeyAction::None => {}
            }
        }
        Event::ButtonPress(e) if ui.import.is_some() => {
            ui.dirty = true;
            let il = import_layout(app.w, app.h);
            let Some(view) = ui.import.as_mut() else {
                return Ok(Flow::Continue);
            };
            let at = import_hit(&il, view, e.event_x, e.event_y);
            match (e.detail, at) {
                (4, _) => import_scroll(view, &il, -3),
                (5, _) => import_scroll(view, &il, 3),
                (1, Some(ImportHit::Row(i))) => {
                    view.cursor = i;
                    view.toggle(i);
                }
                (1, Some(ImportHit::Copy)) => start_import(app, view),
                (1, Some(ImportHit::Cancel)) if !view.copying => ui.import = None,
                _ => {}
            }
        }
        Event::ButtonPress(e) => {
            ui.dirty = true;
            let l = layout(app.w, app.h);
            let shift = e.state.contains(KeyButMask::SHIFT);
            let at = hit(app.w, app.h, e.event_x, e.event_y, state);
            let pressed = match pointer_action(e.detail, shift, at) {
                Pointer::Scroll(rows) => {
                    scroll_by(state, &l, rows);
                    None
                }
                Pointer::Page(pages) => {
                    scroll_by(state, &l, pages * l.visible_rows.max(1) as i32);
                    None
                }
                Pointer::Press(control) => Some(control),
                Pointer::Ignore => None,
            };
            match pressed {
                Some(Hit::Scrollbar) => {
                    if let Some((_, thumb)) = scrollbar(&l, state) {
                        if e.event_y >= thumb.y && e.event_y < thumb.y + thumb.height as i16 {
                            ui.drag = Some(e.event_y - thumb.y);
                        } else {
                            // A click on the track pages towards it.
                            let pages = if e.event_y < thumb.y { -1 } else { 1 };
                            scroll_by(state, &l, pages * l.visible_rows.max(1) as i32);
                        }
                    }
                }
                Some(Hit::Tile(i)) => {
                    state.selected = i;
                    select_current(state);
                    ui.focus = None;
                }
                Some(control) => {
                    // A click on a control presses it once, the same as
                    // Space or Enter on it.
                    press(app, state, ui, control);
                    ui.focus = None;
                }
                None => {}
            }
        }
        Event::MotionNotify(e) => {
            if let Some(grab) = ui.drag {
                let l = layout(app.w, app.h);
                if let Some((track, thumb)) = scrollbar(&l, state) {
                    let max = max_scroll(state, &l);
                    let scroll = scroll_for_thumb(track, thumb, max, e.event_y - grab);
                    if scroll != state.scroll {
                        state.scroll = scroll;
                        ui.dirty = true;
                    }
                }
            }
        }
        Event::ButtonRelease(e) if e.detail == 1 && ui.drag.is_some() => {
            ui.drag = None;
            ui.dirty = true;
        }
        Event::ClientMessage(e) => {
            if e.type_ == atom_id(&app.atoms, "WM_PROTOCOLS") {
                return Ok(Flow::Quit);
            }
        }
        Event::RandrScreenChangeNotify(_) => {
            app.screens_dirty = true;
        }
        // A new layout (setxkbmap, a plugged-in keyboard).
        Event::MappingNotify(e) if e.request == Mapping::KEYBOARD => {
            app.keys = Keys::new(&app.c)?;
        }
        _ => {}
    }
    Ok(Flow::Continue)
}

/// Add or remove the screens the window knows about in the desktop state.
///
/// A screen that disappeared is dropped from the plan and the selection falls
/// back to all screens; its saved settings stay in the file for a reconnect.
pub fn sync_screens(app: &App, state: &mut State) {
    state.geometry = app.screens.clone();
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

/// Block until the X server, a watched file or a worker has something to
/// say, or until `timeout` (a pending debounced refresh) runs out.
///
/// With nothing pending there is no timeout at all: an idle window sleeps
/// until something happens.
fn wait_for_input(app: &App, timeout: Option<Duration>) {
    use std::os::unix::io::AsRawFd;
    let mut fds =
        [app.c.stream().as_raw_fd(), app.watcher.fd(), app.wake.fd()].map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    let ms = match timeout {
        // Round up, so the loop never wakes a millisecond early and spins.
        Some(t) => t.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32,
        None => -1,
    };
    // SAFETY: a live array of pollfd structures and its length.
    let _ = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
}

/// Start the staged change on a worker thread and show "Applying…".
///
/// The window keeps painting and answering keys meanwhile; the controls that
/// change the staged settings are disabled until the answer arrives.
fn run_apply(app: &mut App, state: &mut State) {
    if state.busy {
        return;
    }
    let change = match staged_change(state) {
        Ok(change) => change,
        Err(err) => {
            state.set_message(err, true);
            return;
        }
    };
    if change.is_empty() {
        return;
    }
    state.busy = true;
    let serial = app.apply_job.start(Instant::now());
    let done = app.apply_done.clone();
    let wake = app.wake.clone();
    thread::spawn(move || {
        let result = apply_and_fetch(&change, backend::apply_change);
        let _ = done.send((serial, result));
        wake.notify();
    });
}

/// Take a finished Apply, or give up on one that ran out of time.
fn collect_apply(app: &mut App, state: &mut State) -> bool {
    let mut changed = false;
    while let Ok((serial, result)) = app.apply_results.try_recv() {
        if app.apply_job.accept(serial) {
            finish_apply(state, result);
            changed = true;
        }
    }
    if state.busy && app.apply_job.remaining(Instant::now()) == Some(Duration::ZERO) {
        app.apply_job.abandon();
        finish_apply(
            state,
            Err("Applying took too long; the desktop did not answer. Your changes are still staged.".into()),
        );
        changed = true;
    }
    changed
}

/// Activating a control changes the staged settings. Apply is handled by the
/// caller, which runs the transaction.
fn activate(state: &mut State, control: Hit) {
    if state.busy && !matches!(control, Hit::Tab(_)) {
        // Only looking around is possible while the change is applied.
        return;
    }
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
        // These need the window and are run by `press`; the scrollbar is
        // dragged, not pressed.
        Hit::Apply | Hit::Scrollbar | Hit::AddPictures | Hit::OpenFolder => {}
        Hit::Cancel => {
            state.pending = Pending::default();
            state.set_message("Discarded staged changes", false);
        }
    }
}

/// The tiles whose previews are wanted now: every row that intersects the
/// grid, including a partly visible one, plus one row of look-ahead below.
pub fn preview_range(l: &Layout, scroll: usize, count: usize) -> std::ops::Range<usize> {
    let per_row = l.tiles_per_row.max(1);
    let first = scroll * per_row;
    let mut rows = 0;
    while intersects(tile_rect(l, first + rows * per_row, scroll), l.left) {
        rows += 1;
    }
    let last = first + (rows + 1) * per_row;
    first.min(count)..last.min(count)
}

/// Ask the workers for the previews of the tiles that are on screen.
fn request_visible_previews(app: &mut App, state: &State) {
    let l = layout(app.w, app.h);
    let visible = state.visible();
    let range = preview_range(&l, state.scroll, visible.len());
    // The cache always holds what is on screen, with room to scroll back.
    app.thumbs.set_capacity(PREVIEW_CACHE.max(range.len() * 3));
    let mut paths: Vec<PathBuf> = visible[range].iter().map(|w| w.path.clone()).collect();
    // The staged picture may be scrolled out of the grid; the panel's
    // preview still needs it.
    paths.extend(preview_image(state));
    for path in paths {
        app.want_thumb(&path);
    }
}

/// "Add pictures…" and "Open folder", above the grid at its right edge,
/// shown under My wallpapers only.
pub fn mine_buttons(l: &Layout, state: &State) -> Option<(Rectangle, Rectangle)> {
    if state.tab != Tab::Mine {
        return None;
    }
    // Right-aligned with the last column of tiles.
    let right = l.left.x + l.tiles_per_row as i16 * TILE_W;
    let y = l.left.y - 30;
    let open = Rectangle {
        x: right - 104,
        y,
        width: 104,
        height: 24,
    };
    let add = Rectangle {
        x: open.x - 8 - 124,
        y,
        width: 124,
        height: 24,
    };
    Some((add, open))
}

/// The configured wallpaper directory, where added pictures go.
pub fn wallpaper_dir(state: &State) -> PathBuf {
    let dir = if state.status.dir.trim().is_empty() {
        "~/Pictures/Wallpapers"
    } else {
        state.status.dir.as_str()
    };
    PathBuf::from(backend::expand_home(dir))
}

/// Press one control: the same for a click and for Space or Enter on it.
fn press(app: &mut App, state: &mut State, ui: &mut Ui, control: Hit) {
    match control {
        Hit::Apply => {
            run_apply(app, state);
            ui.focus = None;
        }
        Hit::AddPictures if !state.busy => {
            let dir = wallpaper_dir(state);
            let items = import::candidates(&import::source_dirs(), &dir);
            ui.import = Some(ImportView::new(items, dir));
        }
        Hit::OpenFolder => {
            let dir = wallpaper_dir(state);
            match open_folder(&dir) {
                Ok(()) => state.set_message(format!("Opened {}", dir.display()), false),
                Err(err) => state.set_message(err, true),
            }
        }
        control => activate(state, control),
    }
}

/// Show a folder in the file manager through `xdg-open`, without waiting.
fn open_folder(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut child = std::process::Command::new("xdg-open")
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run xdg-open: {e}"))?;
    // Reap it whenever it ends, so no zombie stays behind.
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The "Add pictures" list: pictures from Pictures and Downloads, any number
/// of which are marked and then copied into the wallpaper directory.
#[derive(Debug)]
pub struct ImportView {
    pub items: Vec<import::Candidate>,
    pub marked: Vec<bool>,
    pub cursor: usize,
    pub scroll: usize,
    /// Where the copies go.
    pub dir: PathBuf,
    /// True while the copies are made.
    pub copying: bool,
    pub message: String,
}

impl ImportView {
    pub fn new(items: Vec<import::Candidate>, dir: PathBuf) -> Self {
        let marked = vec![false; items.len()];
        Self {
            items,
            marked,
            cursor: 0,
            scroll: 0,
            dir,
            copying: false,
            message: String::new(),
        }
    }

    pub fn toggle(&mut self, i: usize) {
        if let Some(mark) = self.marked.get_mut(i) {
            *mark = !*mark;
        }
    }

    /// The pictures to copy: the marked ones, or the one under the cursor
    /// when none is marked.
    pub fn chosen(&self) -> Vec<PathBuf> {
        let marked: Vec<PathBuf> = self
            .items
            .iter()
            .zip(&self.marked)
            .filter(|(_, m)| **m)
            .map(|(c, _)| c.path.clone())
            .collect();
        if marked.is_empty() {
            self.items
                .get(self.cursor)
                .map(|c| vec![c.path.clone()])
                .unwrap_or_default()
        } else {
            marked
        }
    }
}

/// Where the parts of the "Add pictures" view are.
pub struct ImportLayout {
    pub list: Rectangle,
    pub rows: usize,
    pub copy: Rectangle,
    pub cancel: Rectangle,
}

const IMPORT_ROW: i16 = 24;

pub fn import_layout(w: u16, h: u16) -> ImportLayout {
    let w = i16::try_from(w).unwrap_or(i16::MAX).max(1);
    let h = i16::try_from(h).unwrap_or(i16::MAX).max(1);
    let top = 96;
    let bottom = (h - FOOTER - 44).max(top + IMPORT_ROW);
    let list = Rectangle {
        x: PAD,
        y: top,
        width: (w - PAD * 2).max(1) as u16,
        height: (bottom - top) as u16,
    };
    ImportLayout {
        list,
        rows: ((bottom - top) / IMPORT_ROW).max(1) as usize,
        copy: Rectangle {
            x: PAD,
            y: bottom + 8,
            width: 200,
            height: 30,
        },
        cancel: Rectangle {
            x: PAD + 208,
            y: bottom + 8,
            width: 100,
            height: 30,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportHit {
    Row(usize),
    Copy,
    Cancel,
}

pub fn import_hit(l: &ImportLayout, view: &ImportView, x: i16, y: i16) -> Option<ImportHit> {
    if inside(l.copy, x, y) {
        return Some(ImportHit::Copy);
    }
    if inside(l.cancel, x, y) {
        return Some(ImportHit::Cancel);
    }
    if !inside(l.list, x, y) {
        return None;
    }
    let i = view.scroll + ((y - l.list.y) / IMPORT_ROW) as usize;
    (i < view.items.len()).then_some(ImportHit::Row(i))
}

pub fn import_scroll(view: &mut ImportView, l: &ImportLayout, rows: i32) {
    let max = view.items.len().saturating_sub(l.rows) as i64;
    view.scroll = (view.scroll as i64 + i64::from(rows)).clamp(0, max) as usize;
}

pub enum ImportAction {
    None,
    Copy,
    Close,
}

/// Keys in the "Add pictures" view: arrows move, Space marks, Enter copies,
/// Esc closes.
pub fn import_key(view: &mut ImportView, key: u32, l: &ImportLayout) -> ImportAction {
    let count = view.items.len();
    let page = l.rows.max(1);
    let last = count.saturating_sub(1);
    match key {
        K_ESCAPE | K_Q if !view.copying => return ImportAction::Close,
        K_RETURN if !view.copying => return ImportAction::Copy,
        K_SPACE => {
            let i = view.cursor;
            view.toggle(i);
        }
        K_DOWN | K_J => view.cursor = (view.cursor + 1).min(last),
        K_UP | K_K => view.cursor = view.cursor.saturating_sub(1),
        K_PAGE_DOWN => view.cursor = (view.cursor + page).min(last),
        K_PAGE_UP => view.cursor = view.cursor.saturating_sub(page),
        K_HOME => view.cursor = 0,
        K_END => view.cursor = last,
        _ => {}
    }
    if view.cursor < view.scroll {
        view.scroll = view.cursor;
    } else if view.cursor >= view.scroll + page {
        view.scroll = view.cursor + 1 - page;
    }
    ImportAction::None
}

/// Copy the chosen pictures on a worker; the list stays open meanwhile.
fn start_import(app: &App, view: &mut ImportView) {
    let chosen = view.chosen();
    if view.copying || chosen.is_empty() {
        return;
    }
    view.copying = true;
    view.message = format!("Copying {} picture(s)\u{2026}", chosen.len());
    let dir = view.dir.clone();
    let done = app.import_done.clone();
    let wake = app.wake.clone();
    thread::spawn(move || {
        let results = chosen
            .iter()
            .map(|source| import::copy_into(source, &dir))
            .collect();
        let _ = done.send(results);
        wake.notify();
    });
}

/// Take the result of a finished copy: close the list, say what happened,
/// and show My wallpapers, where the new pictures appear once the
/// directory watch has refreshed the list.
fn collect_import(app: &mut App, state: &mut State, view: &mut Option<ImportView>) -> bool {
    let Ok(results) = app.import_results.try_recv() else {
        return false;
    };
    let results: Vec<Result<PathBuf, String>> = results;
    let added = results.iter().filter(|r| r.is_ok()).count();
    let failed: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    *view = None;
    state.tab = Tab::Mine;
    state.selected = 0;
    state.scroll = 0;
    match failed.first() {
        Some(first) => state.set_message(
            format!("Added {added}, {} failed: {first}", failed.len()),
            true,
        ),
        None => state.set_message(
            format!(
                "Added {added} picture(s) to {}. Choose one below.",
                wallpaper_dir(state).display()
            ),
            false,
        ),
    }
    // The watch on the directory refreshes the list; asking now as well
    // covers a directory that was only just created.
    app.debounce.note(Instant::now());
    true
}

/// The "Add pictures" view.
fn paint_import(app: &App, view: &ImportView) -> Result<(), String> {
    let l = import_layout(app.w, app.h);
    app.fill(
        app.palette.background,
        Rectangle {
            x: 0,
            y: 0,
            width: app.w,
            height: app.h,
        },
    )?;
    app.label(PAD, PAD - 8, "Add pictures", app.palette.foreground, 20.0)?;
    app.label(
        PAD,
        PAD + 22,
        &truncate(
            &format!(
                "From Pictures and Downloads, copied to {}. The originals stay where they are.",
                view.dir.display()
            ),
            (app.w as usize / 7).saturating_sub(4),
        ),
        app.palette.muted,
        12.0,
    )?;
    app.label(
        PAD,
        PAD + 44,
        "Space marks a picture, Enter copies the marked ones (or the highlighted one), Esc closes",
        app.palette.muted,
        11.0,
    )?;
    app.fill(app.palette.surface, l.list)?;
    if view.items.is_empty() {
        app.label(
            l.list.x + 8,
            l.list.y + 8,
            "No PNG, JPEG, WebP or BMP pictures in Pictures or Downloads.",
            app.palette.muted,
            12.0,
        )?;
    }
    app.clip(Some(l.list))?;
    for (row, i) in (view.scroll..view.items.len()).take(l.rows).enumerate() {
        let item = &view.items[i];
        let r = Rectangle {
            x: l.list.x,
            y: l.list.y + row as i16 * IMPORT_ROW,
            width: l.list.width,
            height: IMPORT_ROW as u16,
        };
        let (bg, fg) = if i == view.cursor {
            (app.palette.accent, app.palette.background)
        } else {
            (app.palette.surface, app.palette.foreground)
        };
        app.fill(bg, r)?;
        let mark = if view.marked[i] { "[x]" } else { "[ ]" };
        let text = format!(
            "{mark}  {}   {} \u{b7} {}",
            item.name(),
            item.folder,
            import::human_size(item.bytes)
        );
        app.label_on(
            r.x + 8,
            r.y + 4,
            &truncate(&text, (l.list.width as usize / 7).saturating_sub(2)),
            fg,
            bg,
            12.0,
        )?;
    }
    app.clip(None)?;
    let count = view.chosen().len();
    let enabled = count > 0 && !view.copying;
    app.button(
        l.copy,
        &format!("Copy {count} picture{}", if count == 1 { "" } else { "s" }),
        enabled,
        enabled,
    )?;
    app.button(l.cancel, "Cancel", false, !view.copying)?;
    app.label(PAD, l.copy.y + 40, &view.message, app.palette.muted, 12.0)?;
    app.flush()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A window state with `count` official pictures and no desktop behind it.
    fn fixture(count: usize) -> State {
        let list = (0..count)
            .map(|i| Wallpaper {
                name: format!("Picture {i}"),
                path: PathBuf::from(format!("/fixture/w{i:02}.png")),
                official: true,
            })
            .collect();
        State {
            status: Status {
                enabled: true,
                follow_theme: true,
                mode: "random".into(),
                scale: "fill".into(),
                ..Status::default()
            },
            list,
            tab: Tab::Official,
            target: Target::All,
            selected: 0,
            scroll: 0,
            pending: Pending::default(),
            message: String::new(),
            message_is_error: false,
            seen: String::new(),
            busy: false,
            palette: Palette::fallback(),
            geometry: Vec::new(),
        }
    }

    #[test]
    fn the_wheel_scrolls_and_never_stages() {
        let mut state = fixture(30);
        let l = layout(WIDTH, HEIGHT);
        let over_tile = Some(Hit::Tile(5));
        // Fifteen notches over a tile: only scrolling, never a selection.
        for _ in 0..15 {
            match pointer_action(5, false, over_tile) {
                Pointer::Scroll(rows) => scroll_by(&mut state, &l, rows),
                other => panic!("the wheel became {other:?}"),
            }
        }
        assert!(
            state.pending.is_empty(),
            "the wheel staged {:?}",
            state.pending
        );
        assert_eq!(state.selected, 0);
        assert_eq!(state.scroll, max_scroll(&state, &l));
        assert!(state.scroll > 0);
        assert_eq!(pointer_action(4, false, over_tile), Pointer::Scroll(-1));
        assert_eq!(pointer_action(4, true, over_tile), Pointer::Page(-1));
        assert_eq!(pointer_action(5, true, over_tile), Pointer::Page(1));
        // The horizontal wheel and the other buttons do nothing.
        for button in [2, 3, 6, 7, 8, 9] {
            assert_eq!(pointer_action(button, false, over_tile), Pointer::Ignore);
        }
        // Only the primary button presses a control.
        assert_eq!(
            pointer_action(1, false, over_tile),
            Pointer::Press(Hit::Tile(5))
        );
        assert_eq!(pointer_action(1, false, None), Pointer::Ignore);
    }

    #[test]
    fn the_grid_is_clipped_above_the_status_line() {
        let state = fixture(30);
        for (w, h) in [
            (MIN_WIDTH, MIN_HEIGHT),
            (WIDTH, HEIGHT),
            (1600, 1200),
            (420, 1000),
        ] {
            let l = layout(w, h);
            let grid_bottom = l.left.y + l.left.height as i16;
            assert!(
                grid_bottom <= l.footer,
                "{w}x{h}: the grid reaches into the status line"
            );
            // Every row counted as visible fits completely in the grid.
            let last_full = tile_rect(&l, l.visible_rows * l.tiles_per_row - 1, 0);
            assert!(
                last_full.y + last_full.height as i16 <= grid_bottom,
                "{w}x{h}: visible row {} does not fit",
                l.visible_rows
            );
            // The next row is at most partly visible, and is clipped.
            let next = tile_rect(&l, l.visible_rows * l.tiles_per_row, 0);
            assert!(next.y + next.height as i16 > grid_bottom);
            // Nothing in the status line or hint line is a tile.
            for y in [l.footer + 6, l.footer + 30, h as i16 - 2] {
                assert!(
                    !matches!(hit(w, h, l.left.x + 20, y, &state), Some(Hit::Tile(_))),
                    "{w}x{h}: a click at y={y} on the status line hit a tile"
                );
            }
        }
        // The default size shows three full rows, as the window always did.
        assert_eq!(layout(WIDTH, HEIGHT).visible_rows, 3);
    }

    #[test]
    fn a_narrow_window_puts_the_panel_below_the_grid() {
        let state = fixture(30);
        let wide = layout(WIDTH, HEIGHT);
        assert!(!wide.compact);
        assert!(wide.panel.x >= wide.left.x + wide.left.width as i16);
        for (w, h) in [(420u16, 1000u16), (600, 1300)] {
            let l = layout(w, h);
            assert!(l.compact, "{w}x{h}");
            assert!(l.panel.y >= l.left.y + l.left.height as i16, "{w}x{h}");
            assert!(l.panel.y + l.panel.height as i16 <= l.footer, "{w}x{h}");
            assert!(l.left.x + l.left.width as i16 <= w as i16 - PAD);
            assert!(l.panel.x + l.panel.width as i16 <= w as i16 - PAD);
            assert!(l.tiles_per_row >= 2, "{w}x{h}: {}", l.tiles_per_row);
            // Every panel control is inside the panel, and the controls are
            // still found by a click.
            for control in [
                Hit::Scale(0),
                Hit::Scale(3),
                Hit::Rotation(4),
                Hit::FollowTheme,
                Hit::ScreenFollow,
                Hit::Apply,
                Hit::Cancel,
            ] {
                let r = hit_rect(&l, control, &state).unwrap();
                assert!(
                    r.y >= l.panel.y
                        && r.y + r.height as i16 <= l.panel.y + l.panel.height as i16
                        && r.x + r.width as i16 <= l.panel.x + l.panel.width as i16,
                    "{w}x{h}: {control:?} at {r:?} is outside the panel {:?}",
                    l.panel
                );
                assert_eq!(hit(w, h, r.x + 2, r.y + 2, &state), Some(control));
            }
        }
        // Smaller than anything sensible still lays out without panicking.
        for (w, h) in [(1u16, 1u16), (200, 150), (MIN_WIDTH - 1, 300)] {
            let l = layout(w, h);
            assert!(l.tiles_per_row >= 1 && l.visible_rows >= 1);
        }
    }

    #[test]
    fn arrows_scroll_the_selection_into_view() {
        let mut state = fixture(30);
        let l = layout(WIDTH, HEIGHT);
        for _ in 0..5 {
            handle_key(&mut state, K_DOWN, &l, None);
        }
        let row = state.selected / l.tiles_per_row;
        assert!(row >= state.scroll && row < state.scroll + l.visible_rows);
        for _ in 0..5 {
            handle_key(&mut state, K_UP, &l, None);
        }
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn previews_load_for_partly_visible_rows_and_one_more() {
        let l = layout(WIDTH, HEIGHT);
        let per_row = l.tiles_per_row;
        // Three full rows, one partly visible and one row of look-ahead.
        assert_eq!(preview_range(&l, 0, 30), 0..per_row * 5);
        assert_eq!(preview_range(&l, 2, 30), 2 * per_row..(7 * per_row).min(30));
        // Never past the end of the list.
        assert_eq!(preview_range(&l, 0, 3), 0..3);
        assert_eq!(preview_range(&l, 0, 0), 0..0);
        // The partly visible row really intersects the grid.
        let partial = tile_rect(&l, l.visible_rows * per_row, 0);
        assert!(intersects(partial, l.left));
    }

    #[test]
    fn only_the_awaited_apply_is_taken_and_a_late_one_is_ignored() {
        let t0 = Instant::now();
        let mut job = ApplyJob::default();
        assert_eq!(job.remaining(t0), None, "nothing to wait for");
        let first = job.start(t0);
        assert!(job.remaining(t0).unwrap() > backend::APPLY_DEADLINE);
        // The window gives up; the first answer arrives late.
        job.abandon();
        assert!(!job.accept(first), "a late answer is ignored");
        let second = job.start(t0);
        assert!(!job.accept(first), "an older answer is ignored");
        assert!(job.accept(second));
        assert!(!job.accept(second), "an answer is taken once");
        assert_eq!(
            job.start(t0),
            second + 1,
            "serials only grow, so an old answer never matches"
        );
        let far = t0 + Duration::from_secs(600);
        assert_eq!(job.remaining(far), Some(Duration::ZERO));
    }

    #[test]
    fn nothing_is_staged_while_applying_and_a_failure_keeps_the_change() {
        let mut state = fixture(8);
        state.selected = 2;
        select_current(&mut state);
        let staged = state.pending.clone();
        state.busy = true;
        state.selected = 3;
        select_current(&mut state);
        activate(&mut state, Hit::Scale(1));
        activate(&mut state, Hit::Cancel);
        assert_eq!(state.pending, staged, "the change being applied was edited");
        finish_apply(&mut state, Err("niructl gave no answer".into()));
        assert!(!state.busy);
        assert!(state.message_is_error);
        assert_eq!(
            state.pending, staged,
            "a failed Apply keeps the staged change"
        );
        let status = state.status.clone();
        let list = state.list.clone();
        state.busy = true;
        finish_apply(&mut state, Ok((status, list)));
        assert!(state.pending.is_empty());
        assert!(!state.busy);
    }

    #[test]
    fn the_position_and_the_scrollbar_follow_the_scroll() {
        let mut state = fixture(24);
        let l = layout(WIDTH, HEIGHT);
        // Three full rows and a partly visible fourth: tiles 1-16.
        assert_eq!(position_label(&l, &state), "Wallpapers 1\u{2013}16 of 24");
        let (track, top) = scrollbar(&l, &state).expect("24 tiles need a scrollbar");
        assert_eq!(top.y, track.y);
        scroll_by(&mut state, &l, 100);
        assert_eq!(position_label(&l, &state), "Wallpapers 13\u{2013}24 of 24");
        let (track, bottom) = scrollbar(&l, &state).unwrap();
        assert_eq!(
            bottom.y + bottom.height as i16,
            track.y + track.height as i16
        );
        // Dragging the thumb to either end and back maps to the scroll.
        let max = max_scroll(&state, &l);
        assert_eq!(scroll_for_thumb(track, bottom, max, track.y - 50), 0);
        assert_eq!(scroll_for_thumb(track, bottom, max, track.y + 2000), max);
        assert_eq!(scroll_for_thumb(track, bottom, max, bottom.y), max);
        // The scrollbar is a control of its own, never a tile.
        assert_eq!(
            hit(WIDTH, HEIGHT, track.x + 2, track.y + 4, &state),
            Some(Hit::Scrollbar)
        );
        // A list that fits has none.
        assert!(scrollbar(&l, &fixture(4)).is_none());
        assert_eq!(position_label(&l, &fixture(0)), "No wallpapers");
    }

    #[test]
    fn page_keys_home_and_end_move_by_pages() {
        let mut state = fixture(40);
        let l = layout(WIDTH, HEIGHT);
        let page = l.tiles_per_row * l.visible_rows;
        handle_key(&mut state, K_PAGE_DOWN, &l, None);
        assert_eq!(state.selected, page);
        assert!(state.scroll > 0, "the new selection is scrolled into view");
        handle_key(&mut state, K_END, &l, None);
        assert_eq!(state.selected, 39);
        assert_eq!(state.scroll, max_scroll(&state, &l));
        handle_key(&mut state, K_PAGE_UP, &l, None);
        assert_eq!(state.selected, 39 - page);
        handle_key(&mut state, K_HOME, &l, None);
        assert_eq!((state.selected, state.scroll), (0, 0));
        assert!(state.pending.is_empty(), "paging never stages");
    }

    #[test]
    fn the_preview_shows_the_staged_picture_on_the_chosen_screen() {
        let mut state = fixture(6);
        state.status.theme_image = "/fixture/theme.png".into();
        state.status.screens = vec![
            backend::ScreenPlan {
                output: "eDP-1".into(),
                source: "override".into(),
                kind: "image".into(),
                scale: "fit".into(),
                image: "/fixture/own.png".into(),
            },
            backend::ScreenPlan {
                output: "DP-1".into(),
                source: "inherit".into(),
                kind: "theme".into(),
                scale: "fill".into(),
                image: "/fixture/theme.png".into(),
            },
        ];
        let screens = vec![
            Screen {
                name: "eDP-1".into(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1200,
            },
            Screen {
                name: "DP-1".into(),
                x: 1920,
                y: 0,
                width: 1080,
                height: 1920,
            },
        ];
        // All screens: what the inheriting screen shows, on that screen.
        assert_eq!(
            preview_image(&state),
            Some(PathBuf::from("/fixture/theme.png"))
        );
        assert_eq!(
            preview_screen(&state, &screens),
            ("DP-1".to_string(), (1080, 1920))
        );
        // One screen: its own picture, on itself.
        state.target = Target::Screen("eDP-1".into());
        assert_eq!(
            preview_image(&state),
            Some(PathBuf::from("/fixture/own.png"))
        );
        assert_eq!(preview_screen(&state, &screens).1, (1920, 1200));
        // A staged picture wins.
        state.selected = 3;
        select_current(&mut state);
        assert_eq!(
            preview_image(&state),
            Some(PathBuf::from("/fixture/w03.png"))
        );
        // No screens reported: a 1080p stand-in.
        assert_eq!(preview_screen(&state, &[]).1, (1920, 1080));
        // There is room for a real preview at the default size, and it sits
        // between the description and Apply.
        let l = layout(WIDTH, HEIGHT);
        let frame = preview_rect(&l).expect("room for a preview");
        assert!(frame.height >= 140, "{frame:?}");
        assert!(frame.y + frame.height as i16 <= apply_rect(&l).y);
        assert!(frame.y >= follow_rect(&l).y + 28 + 3 * 18);
    }

    #[test]
    fn my_wallpapers_offers_add_pictures_and_open_folder() {
        let mut state = fixture(4);
        let l = layout(WIDTH, HEIGHT);
        assert!(
            mine_buttons(&l, &state).is_none(),
            "only under My wallpapers"
        );
        state.tab = Tab::Mine;
        let (add, open) = mine_buttons(&l, &state).unwrap();
        assert_eq!(
            hit(WIDTH, HEIGHT, add.x + 4, add.y + 4, &state),
            Some(Hit::AddPictures)
        );
        assert_eq!(
            hit(WIDTH, HEIGHT, open.x + 4, open.y + 4, &state),
            Some(Hit::OpenFolder)
        );
        assert!(add.x + add.width as i16 <= open.x);
        assert!(open.x + open.width as i16 <= l.left.x + l.left.width as i16);
        assert!(focus_order(&state).contains(&Hit::AddPictures));
        state.status.dir = "/srv/walls".into();
        assert_eq!(wallpaper_dir(&state), PathBuf::from("/srv/walls"));
    }

    #[test]
    fn the_import_list_marks_and_copies_the_chosen_pictures() {
        let root = std::env::temp_dir().join(format!("nw-import-view-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pictures = root.join("Pictures");
        std::fs::create_dir_all(&pictures).unwrap();
        for name in ["a.png", "b.jpg", "c.webp"] {
            std::fs::write(pictures.join(name), name).unwrap();
        }
        let items = import::candidates(&[(pictures.clone(), "Pictures".into())], &root.join("W"));
        let mut view = ImportView::new(items, root.join("W"));
        let l = import_layout(WIDTH, HEIGHT);
        // Nothing marked: Enter copies the highlighted picture.
        assert_eq!(view.chosen().len(), 1);
        import_key(&mut view, K_SPACE, &l);
        import_key(&mut view, K_DOWN, &l);
        import_key(&mut view, K_DOWN, &l);
        import_key(&mut view, K_SPACE, &l);
        assert_eq!(view.chosen().len(), 2);
        assert!(matches!(
            import_key(&mut view, K_RETURN, &l),
            ImportAction::Copy
        ));
        for source in view.chosen() {
            import::copy_into(&source, &view.dir).unwrap();
        }
        assert_eq!(std::fs::read_dir(&view.dir).unwrap().count(), 2);
        assert_eq!(
            std::fs::read_dir(&pictures).unwrap().count(),
            3,
            "originals stay"
        );
        // A click on a row is that row; the buttons are found too.
        let row = import_hit(&l, &view, l.list.x + 10, l.list.y + IMPORT_ROW + 2);
        assert_eq!(row, Some(ImportHit::Row(1)));
        assert_eq!(
            import_hit(&l, &view, l.copy.x + 2, l.copy.y + 2),
            Some(ImportHit::Copy)
        );
        view.copying = true;
        assert!(
            matches!(import_key(&mut view, K_ESCAPE, &l), ImportAction::None),
            "the list stays open while copying"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn two_screens(state: &mut State) {
        state.status.screens = ["DP-1", "HDMI-1"]
            .iter()
            .map(|name| backend::ScreenPlan {
                output: name.to_string(),
                source: if *name == "HDMI-1" {
                    "override"
                } else {
                    "inherit"
                }
                .into(),
                kind: "image".into(),
                scale: "fill".into(),
                image: String::new(),
            })
            .collect();
        state.geometry = vec![
            Screen {
                name: "DP-1".into(),
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
            },
            Screen {
                name: "HDMI-1".into(),
                x: 2560,
                y: 0,
                width: 1080,
                height: 1920,
            },
        ];
    }

    #[test]
    fn the_screen_choice_is_a_map_of_the_screens() {
        let mut state = fixture(4);
        two_screens(&mut state);
        let l = layout(WIDTH, HEIGHT);
        let rects = screen_rects(&l, &state);
        assert_eq!(rects.len(), 3, "All screens and two screens");
        let (dp, hdmi) = (rects[1], rects[2]);
        // Side by side as RandR has them, the portrait one taller and
        // narrower, both in proportion.
        assert!(hdmi.x > dp.x + dp.width as i16);
        assert!(hdmi.height > dp.height && hdmi.width < dp.width);
        let ratio = f64::from(dp.width + 2) / f64::from(dp.height + 2);
        assert!((ratio - 16.0 / 9.0).abs() < 0.15, "{ratio}");
        // Clear of the settings panel, and each is its own control.
        for (i, r) in rects.iter().enumerate() {
            assert!(!intersects(*r, l.panel), "{i}: {r:?}");
            assert_eq!(
                hit(WIDTH, HEIGHT, r.x + 2, r.y + 2, &state),
                Some(Hit::Screen(i as u8))
            );
        }
        assert!(screen_has_own(&state, "HDMI-1"));
        assert!(!screen_has_own(&state, "DP-1"));
        // Narrow: the map moves above "All screens" and still fits.
        let narrow = layout(560, 1200);
        let rects = screen_rects(&narrow, &state);
        assert!(rects[1].y + (rects[1].height as i16) <= rects[0].y);
        assert!(rects[2].x + rects[2].width as i16 <= 560 - PAD + 1);
    }

    #[test]
    fn mirrored_and_unknown_screens_still_get_a_place() {
        let region = Rectangle {
            x: 0,
            y: 0,
            width: 200,
            height: 60,
        };
        let a = Screen {
            name: "eDP-1".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let mirror = Screen {
            name: "HDMI-1".into(),
            ..a.clone()
        };
        let rects = screen_map(region, &[Some(&a), Some(&mirror)]);
        assert!(
            !intersects(rects[0], rects[1]),
            "mirrors share the area side by side"
        );
        assert_eq!(rects[0].y, rects[1].y);
        let rects = screen_map(region, &[Some(&a), None]);
        assert!(
            rects[1].x > rects[0].x,
            "an unknown screen goes to the right"
        );
        assert!(screen_map(region, &[]).is_empty());
    }

    #[test]
    fn scrolling_stops_at_both_ends() {
        let mut state = fixture(30);
        let l = layout(WIDTH, HEIGHT);
        scroll_by(&mut state, &l, -3);
        assert_eq!(state.scroll, 0);
        scroll_by(&mut state, &l, 1000);
        assert_eq!(state.scroll, max_scroll(&state, &l));
        // A list that fits never scrolls.
        let mut small = fixture(2);
        scroll_by(&mut small, &l, 5);
        assert_eq!(small.scroll, 0);
    }
}

/// WM_NORMAL_HINTS (ICCCM 4.1.2.3): the preferred size (PSize) and the
/// smallest size the layout supports (PMinSize). No maximum: a window manager
/// clamps to it, and a bogus one shrank 0.2.0 to a 1x1 stub.
fn normal_hints() -> [u32; 18] {
    const P_SIZE: u32 = 1 << 3;
    const P_MIN_SIZE: u32 = 1 << 4;
    let mut hints = [0u32; 18];
    hints[0] = P_SIZE | P_MIN_SIZE;
    hints[3] = u32::from(WIDTH);
    hints[4] = u32::from(HEIGHT);
    hints[5] = u32::from(MIN_WIDTH);
    hints[6] = u32::from(MIN_HEIGHT);
    hints
}

#[cfg(test)]
mod hint_tests {
    use super::*;

    #[test]
    fn normal_hints_have_no_maximum() {
        let h = normal_hints();
        assert_eq!(h[0] & (1 << 5), 0, "PMaxSize must not be set");
        assert_eq!(h[0], 1 << 3 | 1 << 4);
        assert_eq!((h[3], h[4]), (1000, 690));
        assert_eq!((h[5], h[6]), (660, 500));
        assert!(h[7..].iter().all(|v| *v == 0));
    }
}
