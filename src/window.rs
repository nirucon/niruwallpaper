//! The window: layout, keys and painting.
//!
//! Drawing is immediate mode into a backing pixmap. Every event that changes
//! state schedules a full repaint, which is then copied to the window in one
//! request, so nothing flickers and the state machine stays small.

use std::path::{Path, PathBuf};

use x11rb::{
    connection::Connection,
    protocol::{xproto::*, Event},
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    COPY_DEPTH_FROM_PARENT,
};

use crate::backend::{self, Status, Wallpaper};
use crate::theme::{self, Palette};
use crate::thumbs;

pub const APP_ID: &str = "niruwallpaper";
pub const APP_NAME: &str = "NIRU Wallpaper";

const WIDTH: u16 = 960;
const HEIGHT: u16 = 640;
const MIN_WIDTH: u16 = 640;
const MIN_HEIGHT: u16 = 480;

const PAD: i16 = 20;
const ROW: i16 = 26;
const HEADER: i16 = 100;
const THUMB: u16 = 96;
const TILE_W: i16 = 148;
const TILE_H: i16 = 132;
const PANEL_W: i16 = 250;
const FOOTER: i16 = 76;

/// Which list is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Official,
    Mine,
}

/// Changes the user has staged but not applied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    pub image: Option<PathBuf>,
    pub scale: Option<String>,
    pub rotation: Option<u32>,
    pub follow_theme: Option<bool>,
}

impl Pending {
    pub fn is_empty(&self) -> bool {
        self.image.is_none()
            && self.scale.is_none()
            && self.rotation.is_none()
            && self.follow_theme.is_none()
    }
}

pub struct State {
    pub status: Status,
    pub list: Vec<Wallpaper>,
    pub tab: Tab,
    pub selected: usize,
    pub scroll: usize,
    pub pending: Pending,
    pub message: String,
    pub message_is_error: bool,
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
}

/// Load everything the window shows.
pub fn load() -> Result<State, String> {
    let status = backend::read_status()?;
    let list = backend::read_list(&status.theme_dir)?;
    let mut state = State {
        status,
        list,
        tab: Tab::Official,
        selected: 0,
        scroll: 0,
        pending: Pending::default(),
        message: String::new(),
        message_is_error: false,
    };
    if let Some(active) = state.status.active_image().map(Path::new) {
        if let Some(pos) = state.visible().iter().position(|w| w.path.as_path() == active) {
            state.selected = pos;
        } else if state.visible().iter().any(|w| !w.official) {
            state.tab = Tab::Mine;
            if let Some(pos) = state.visible().iter().position(|w| w.path.as_path() == active) {
                state.selected = pos;
            }
        }
    }
    Ok(state)
}

/// Refuse to run over a screen that cannot take a 24-bit image.
///
/// Returns the error text; the caller decides whether it is fatal.
fn check_screen(c: &RustConnection) -> Result<(), String> {
    let root = c.get_geometry(c.setup().roots[0].root).map_err(|e| e.to_string())?.reply();
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

pub struct App {
    c: RustConnection,
    win: Window,
    gc: Gcontext,
    back: x11rb::protocol::xproto::Pixmap,
    w: u16,
    h: u16,
    atoms: Vec<(&'static str, u32)>,
    palette: Palette,
    font: crate::text::Font,
    keys: Keys,
    /// True when the server wants the least significant byte first, which
    /// decides whether a pixel is uploaded as BGRX or RGBX.
    image_lsb: bool,
    root: Window,
    depth: u8,
}

impl App {
    pub fn open() -> Result<Self, String> {
        let (c, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
        check_screen(&c)?;
        let root = c.setup().roots[screen].root;
        let atoms = intern(&c)?;
        let palette = theme::load();
        let font = crate::text::Font::load(palette);
        let keys = Keys::new(&c)?;
        let image_lsb = c.setup().image_byte_order == x11rb::protocol::xproto::ImageOrder::LSB_FIRST;

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
        c.change_property8(PropMode::REPLACE, win, AtomEnum::WM_NAME, AtomEnum::STRING, name)
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
        // A fixed size: every control has a position computed from the window
        // size, so letting a window manager resize it would only move the
        // panel around. min and max are equal, which is how hints express that.
        let mut hints = vec![0u32; 18];
        hints[0] = 1 << 4 | 1 << 5; // min size, max size
        hints[5] = u32::from(WIDTH);
        hints[6] = u32::from(HEIGHT);
        hints[7] = u32::from(WIDTH);
        hints[8] = u32::from(HEIGHT);
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
        })
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
}

impl App {
    fn label(&self, x: i16, y: i16, text: &str, colour: u32, size: f32) -> Result<(), String> {
        let (w, h, pixels) = self.font.size(size).render(text, colour, self.palette.background);
        if w == 0 || h == 0 {
            return Ok(());
        }
        self.upload(x, y, w as u16, h as u16, &pixels)
    }

    fn label_on(&self, x: i16, y: i16, text: &str, colour: u32, bg: u32, size: f32) -> Result<(), String> {
        let (w, h, pixels) = self.font.size(size).render(text, colour, bg);
        if w == 0 || h == 0 {
            return Ok(());
        }
        self.upload(x, y, w as u16, h as u16, &pixels)
    }

    fn button(&self, r: Rectangle, text: &str, active: bool) -> Result<(), String> {
        let (bg, fg) = if active {
            (self.palette.accent, self.palette.background)
        } else {
            (self.palette.surface, self.palette.foreground)
        };
        self.fill(bg, r)?;
        self.fill(self.palette.border, Rectangle { width: r.width, height: 1, ..r })?;
        let (_, text_h, _) = self.font.size(13.0).render(text, fg, bg);
        let y = r.y + (i16::try_from(text_h).unwrap_or(0) - 8).max(0) / 2;
        self.label_on(r.x + 8, y, text, fg, bg, 13.0)
    }

    /// Draw the whole window. One function so a click and a repaint can never
    /// disagree about where a control is.
    pub fn paint(&self, state: &State) -> Result<(), String> {
        let l = layout(self.w, self.h);
        self.fill(self.palette.background, Rectangle {
            x: 0,
            y: 0,
            width: self.w,
            height: self.h,
        })?;
        self.label(PAD, PAD - 8, APP_NAME, self.palette.foreground, 20.0)?;
        self.label(
            PAD,
            PAD + 22,
            &format!("Theme: {}", theme_label(&state.status.theme)),
            self.palette.muted,
            12.0,
        )?;
        // Tabs.
        for (rect, tab, text) in [
            (l.tabs.0, Tab::Official, "Official themes"),
            (l.tabs.1, Tab::Mine, "My wallpapers"),
        ] {
            self.button(rect, text, state.tab == tab)?;
        }

        // Tiles.
        let visible = state.visible();
        if visible.is_empty() {
            let text = match state.tab {
                Tab::Official => "No theme wallpapers are installed.\nChoose a theme in NIRUX settings, or add an image under My wallpapers.",
                Tab::Mine => "No images of your own. Put PNG files in ~/Pictures/Wallpapers and open this window again.",
            };
            for (i, line) in text.lines().enumerate() {
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
            let border = if chosen || active {
                self.palette.accent
            } else {
                self.palette.border
            };
            self.fill(self.palette.surface, rect)?;
            self.fill(border, Rectangle { width: rect.width, height: 2, ..rect })?;
            // The preview is cached on disk keyed by the file's identity, so
            // this is a file check on every repaint rather than a decode.
            let thumb = thumbs::thumbnail(&entry.path);
            match thumb {
                Some(path) => {
                    if let Ok(img) = image::open(&path) {
                        let rgba = img.to_rgba8();
                        let tw = rect.width.min(rgba.width() as u16);
                        let th = rect.height.min(rgba.height() as u16).min(THUMB);
                        let small = image::imageops::resize(
                            &rgba,
                            u32::from(tw),
                            u32::from(th),
                            image::imageops::FilterType::Triangle,
                        );
                        self.upload(rect.x, rect.y + 2, tw, th, small.as_raw())?;
                    }
                }
                None => {
                    self.label(rect.x + 6, rect.y + 8, "(no preview)", self.palette.muted, 11.0)?;
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

        // Panel.
        self.fill(self.palette.surface, l.panel)?;
        self.label(l.panel.x + 12, l.panel.y + 4, "Scale", self.palette.muted, 12.0)?;
        let scale = state
            .pending
            .scale
            .clone()
            .unwrap_or_else(|| state.status.scale.clone());
        for (i, rect) in scale_rects(&l).iter().enumerate() {
            self.button(*rect, SCALES[i], SCALES[i] == scale)?;
        }
        self.label(l.panel.x + 12, l.panel.y + 148, "Rotate (minutes)", self.palette.muted, 12.0)?;
        let rotation = state.pending.rotation.unwrap_or(state.status.rotation_minutes);
        for (i, rect) in rotation_rects(&l).iter().enumerate() {
            let text = if ROTATIONS[i] == 0 {
                "Off".to_string()
            } else {
                ROTATIONS[i].to_string()
            };
            self.button(*rect, &text, ROTATIONS[i] == rotation)?;
        }
        let follow = state
            .pending
            .follow_theme
            .unwrap_or(state.status.follow_theme);
        self.button(follow_rect(&l), if follow { "Follow theme: on" } else { "Follow theme: off" }, follow)?;
        self.label(
            l.panel.x + 12,
            l.panel.y + 290,
            "A chosen image overrides the theme.",
            self.palette.muted,
            11.0,
        )?;
        self.button(apply_rect(&l), "Apply", !state.pending.is_empty())?;
        self.button(cancel_rect(&l), "Discard", !state.pending.is_empty())?;

        // Footer message.
        let message = if state.message.is_empty() {
            describe(state)
        } else {
            state.message.clone()
        };
        let colour = if state.message_is_error {
            self.palette.accent
        } else {
            self.palette.muted
        };
        self.label(PAD, l.footer + 8, &truncate(&message, 96), colour, 12.0)?;
        self.label(
            PAD,
            l.footer + 30,
            "Tab/arrows select  Enter applies  Esc closes  q quits",
            self.palette.muted,
            11.0,
        )?;
        Ok(())
    }

    pub fn flush(&self) -> Result<(), String> {
        self.c
            .copy_area(
                self.back,
                self.win,
                self.gc,
                0,
                0,
                0,
                0,
                self.w,
                self.h,
            )
            .map_err(|e| e.to_string())?;
        self.c.flush().map_err(|e| e.to_string())
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

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// One line about what the desktop is showing.
fn describe(state: &State) -> String {
    if state.status.follows_theme() {
        format!(
            "Showing the {} theme wallpaper",
            theme_label(&state.status.theme)
        )
    } else if state.status.shows_theme_colour() {
        format!(
            "The {} theme has no wallpaper; its background colour is used",
            theme_label(&state.status.theme)
        )
    } else if state.status.has_override() {
        format!("Showing your image: {}", file_name(&state.status.file))
    } else {
        "No wallpaper is set".to_string()
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Where each clickable region sits, so painting and clicking cannot disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Tab(Tab),
    Tile(usize),
    Scale(u8),
    Rotation(u8),
    FollowTheme,
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
}

pub fn layout(w: u16, h: u16) -> Layout {
    let w = i16::try_from(w).unwrap_or(960);
    let h = i16::try_from(h).unwrap_or(640);
    let list_w = w - PAD * 3 - PANEL_W;
    let top = HEADER + ROW * 2;
    let footer = h - FOOTER;
    let tiles_per_row = ((list_w / TILE_W).max(1)) as usize;
    let visible_rows = ((footer - top) / TILE_H).max(1) as usize;
    Layout {
        left: Rectangle {
            x: PAD,
            y: HEADER,
            width: (list_w + PAD).max(1) as u16,
            height: (footer - HEADER).max(1) as u16,
        },
        panel: Rectangle {
            x: w - PAD - PANEL_W,
            y: HEADER,
            width: PANEL_W as u16,
            height: (footer - HEADER).max(1) as u16,
        },
        footer,
        tiles_per_row,
        visible_rows,
        tabs: (
            Rectangle {
                x: PAD,
                y: HEADER - 46,
                width: 130,
                height: 30,
            },
            Rectangle {
                x: PAD + 140,
                y: HEADER - 46,
                width: 130,
                height: 30,
            },
        ),
    }
}

/// The clickable region at a point, if any.
pub fn hit(w: u16, h: u16, x: i16, y: i16, state: &State) -> Option<Hit> {
    let l = layout(w, h);
    for (rect, hit) in [(l.tabs.0, Hit::Tab(Tab::Official)), (l.tabs.1, Hit::Tab(Tab::Mine))] {
        if inside(rect, x, y) {
            return Some(hit);
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
    if inside(apply_rect(&l), x, y) {
        return Some(Hit::Apply);
    }
    if inside(cancel_rect(&l), x, y) {
        return Some(Hit::Cancel);
    }
    None
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
    let y = l.panel.y + 74;
    for i in 0..SCALES.len() {
        out.push(Rectangle {
            x: l.panel.x + 12 + (i as i16 % 2) * 112,
            y: y + (i as i16 / 2) * 32,
            width: 106,
            height: 26,
        });
    }
    out
}

fn rotation_rects(l: &Layout) -> Vec<Rectangle> {
    let mut out = Vec::new();
    let y = l.panel.y + 178;
    for i in 0..ROTATIONS.len() {
        out.push(Rectangle {
            x: l.panel.x + 12 + (i as i16 % 3) * 76,
            y: y + (i as i16 / 3) * 30,
            width: 70,
            height: 24,
        });
    }
    out
}

fn follow_rect(l: &Layout) -> Rectangle {
    Rectangle {
        x: l.panel.x + 12,
        y: l.panel.y + 248,
        width: PANEL_W as u16 - 24,
        height: 30,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            status: Status::default(),
            list: vec![],
            tab: Tab::Official,
            selected: 0,
            scroll: 0,
            pending: Pending::default(),
            message: String::new(),
            message_is_error: false,
        }
    }

    #[test]
    fn tiles_fit_the_list_width() {
        let l = layout(960, 640);
        assert!(l.tiles_per_row >= 3, "{} tiles per row", l.tiles_per_row);
        let right = l.left.x + l.tiles_per_row as i16 * TILE_W;
        assert!(right <= l.panel.x, "tiles overlap the panel");
    }

    #[test]
    fn the_smallest_window_still_shows_tiles() {
        let l = layout(MIN_WIDTH, MIN_HEIGHT);
        assert!(l.tiles_per_row >= 1, "no column at the minimum width");
        assert!(l.visible_rows >= 1, "no row at the minimum height");
        // The list must not be squeezed under the header or the panel.
        assert!(l.left.height > 0 && l.panel.height > 0);
    }

    #[test]
    fn clicks_on_the_tabs_are_recognised() {
        let s = state();
        let l = layout(960, 640);
        let p = (l.tabs.0.x + 2, l.tabs.0.y + 2);
        assert_eq!(hit(960, 640, p.0, p.1, &s), Some(Hit::Tab(Tab::Official)));
        let p = (l.tabs.1.x + 2, l.tabs.1.y + 2);
        assert_eq!(hit(960, 640, p.0, p.1, &s), Some(Hit::Tab(Tab::Mine)));
    }

    #[test]
    fn the_apply_button_is_inside_the_panel() {
        let l = layout(960, 640);
        let r = apply_rect(&l);
        assert!(r.x >= l.panel.x);
        assert!(r.y + r.height as i16 <= l.panel.y + l.panel.height as i16);
    }

    #[test]
    fn an_empty_area_is_not_a_click() {
        let s = state();
        assert_eq!(hit(960, 640, 5, 5, &s), None);
    }

    #[test]
    fn picking_an_image_stops_theme_following_in_the_same_step() {
        let mut s = state();
        s.status.follow_theme = true;
        s.list = vec![Wallpaper {
            name: "Nord".into(),
            path: PathBuf::from("/w/Nord.png"),
            official: true,
        }];
        select_current(&mut s);
        assert_eq!(s.pending.image, Some(PathBuf::from("/w/Nord.png")));
        assert_eq!(
            s.pending.follow_theme,
            Some(false),
            "choosing an image must not leave theme following on"
        );
    }

    #[test]
    fn applying_both_workflows_at_once_is_refused() {
        let mut s = state();
        s.pending.image = Some(PathBuf::from("/w/own.png"));
        s.pending.follow_theme = Some(true);
        let err = apply(&mut s).expect_err("a contradictory request must not be applied");
        assert!(err.contains("either"), "{err}");
    }

    #[test]
    fn a_pending_set_knows_when_it_is_empty() {
        let mut p = Pending::default();
        assert!(p.is_empty());
        p.scale = Some("fit".into());
        assert!(!p.is_empty());
    }

    #[test]
    fn rotation_options_include_off() {
        assert_eq!(ROTATIONS[0], 0, "the first rotation option turns it off");
    }
}

/// Apply the staged changes, one `niructl` call per setting.
///
/// Kept separate from the event loop so a test can drive it without a window.
pub fn apply(state: &mut State) -> Result<(), String> {
    let pending = state.pending.clone();
    if pending.is_empty() {
        return Ok(());
    }
    // The two workflows never run together: an image that replaces the theme
    // and theme following that replaces the image would contradict each other,
    // and the backend's last command would silently win. Refuse the
    // combination rather than picking one.
    if pending.image.is_some() && pending.follow_theme == Some(true) {
        return Err("choose either your own image or the theme wallpaper, not both".into());
    }
    // Order matters: choosing an image sets mode=single, and turning theme
    // following on resets that override, so the image goes first.
    if let Some(image) = &pending.image {
        backend::set_image(&image.display().to_string())?;
    }
    if let Some(scale) = &pending.scale {
        backend::set_scale(scale)?;
    }
    if let Some(rotation) = pending.rotation {
        backend::set_rotation(rotation)?;
    }
    if let Some(follow) = pending.follow_theme {
        backend::set_follow_theme(follow)?;
        // Turning theme following on while an image is still the single
        // override leaves `single` behind; the backend handles that, and this
        // keeps the reported state honest if it did not.
    }
    let status = backend::read_status()?;
    let list = backend::read_list(&status.theme_dir)?;
    state.status = status;
    state.list = list;
    state.pending = Pending::default();
    if state.selected >= state.visible().len() {
        state.selected = state.visible().len().saturating_sub(1);
    }
    state.set_message("Applied", false);
    Ok(())
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

/// The keyboard controls the window answers to.
pub fn handle_key(state: &mut State, key: u32, l: &Layout) -> KeyAction {
    match key {
        K_ESCAPE => KeyAction::Quit,
        K_Q | K_Q_SHIFTED => KeyAction::Quit,
        K_RETURN => KeyAction::Apply,
        K_TAB => {
            state.tab = match state.tab {
                Tab::Official => Tab::Mine,
                Tab::Mine => Tab::Official,
            };
            state.selected = 0;
            state.scroll = 0;
            KeyAction::Redraw
        }
        K_RIGHT | K_L => {
            let count = state.visible().len();
            if count > 0 {
                state.selected = (state.selected + 1).min(count - 1);
                ensure_visible(state, l);
            }
            KeyAction::Redraw
        }
        K_LEFT | K_H => {
            state.selected = state.selected.saturating_sub(1);
            ensure_visible(state, l);
            KeyAction::Redraw
        }
        K_DOWN | K_J => {
            let count = state.visible().len();
            state.selected = (state.selected + l.tiles_per_row).min(count.saturating_sub(1));
            ensure_visible(state, l);
            KeyAction::Redraw
        }
        K_UP | K_K => {
            state.selected = state.selected.saturating_sub(l.tiles_per_row);
            ensure_visible(state, l);
            KeyAction::Redraw
        }
        K_SPACE => KeyAction::Select,
        _ => KeyAction::None,
    }
}

pub enum KeyAction {
    None,
    Redraw,
    Select,
    Apply,
    Quit,
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

/// Stage the currently selected image.
pub fn select_current(state: &mut State) {
    let picked = state
        .current()
        .map(|entry| (entry.name.clone(), entry.path.clone()));
    if let Some((name, path)) = picked {
        state.pending.image = Some(path);
        // The two workflows are exclusive. Staging an image while theme
        // following is on would leave the user with a choice they cannot
        // predict: `wallpaper set` writes mode=single, which stops the theme
        // wallpaper from showing anyway. Say so instead of mixing states.
        if state.pending.follow_theme.unwrap_or(state.status.follow_theme) {
            state.pending.follow_theme = Some(false);
            state.set_message(
                format!("Stage {name} — this replaces the theme wallpaper"),
                false,
            );
        } else {
            state.set_message(format!("Stage {name}"), false);
        }
    }
}

/// The event loop. Returns when the window is closed.
pub fn run(app: &mut App, state: &mut State, _palette: Palette) -> Result<(), String> {
    app.paint(state)?;
    app.flush()?;
    loop {
        let event = app.c.wait_for_event().map_err(|e| e.to_string())?;
        let mut redraw = true;
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
                match handle_key(state, app.keys.sym(e.detail), &l) {
                    KeyAction::Quit => return Ok(()),
                    KeyAction::Apply => {
                        if let Err(err) = apply(state) {
                            state.set_message(err, true);
                        }
                    }
                    KeyAction::Select => select_current(state),
                    KeyAction::Redraw | KeyAction::None => {}
                }
            }
            Event::ButtonPress(e) => {
                redraw = handle_click(app, state, &e)?;
                if !redraw {
                    return Ok(());
                }
            }
            Event::ClientMessage(e) => {
                if e.type_ == atom_id(&app.atoms, "WM_PROTOCOLS") {
                    return Ok(());
                }
            }
            _ => redraw = false,
        }
        if redraw {
            app.paint(state)?;
            app.flush()?;
        } else {
            app.c.flush().map_err(|e| e.to_string())?;
        }
    }
}

/// Returns whether to quit.
fn handle_click(app: &App, state: &mut State, e: &ButtonPressEvent) -> Result<bool, String> {
    match hit(app.w, app.h, e.event_x, e.event_y, state) {
        Some(Hit::Tab(tab)) => {
            state.tab = tab;
            state.selected = 0;
            state.scroll = 0;
        }
        Some(Hit::Tile(i)) => {
            state.selected = i;
            select_current(state);
        }
        Some(Hit::Scale(i)) => {
            state.pending.scale = Some(SCALES[i as usize].to_string());
        }
        Some(Hit::Rotation(i)) => {
            state.pending.rotation = Some(ROTATIONS[i as usize]);
        }
        Some(Hit::FollowTheme) => {
            let now = state
                .pending
                .follow_theme
                .unwrap_or(state.status.follow_theme);
            state.pending.follow_theme = Some(!now);
            // Turning following on is the "use the theme's own wallpaper"
            // workflow, so a staged image would contradict it.
            if !now {
                state.pending.image = None;
                state.set_message("Follow the theme wallpaper", false);
            } else {
                state.set_message("Use a chosen image instead", false);
            }
        }
        Some(Hit::Apply) => {
            if let Err(err) = apply(state) {
                state.set_message(err, true);
            }
        }
        Some(Hit::Cancel) => {
            state.pending = Pending::default();
            state.set_message("Discarded pending changes", false);
        }
        None => {}
    }
    Ok(true)
}
