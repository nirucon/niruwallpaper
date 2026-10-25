//! The connected screens, read from RandR.
//!
//! The caller needs only the list; a real session reads it from the X server,
//! and the automated acceptance run can point `NIRU_RANDR_EMU` at a small
//! layout file so the same window code is exercised with fixed screens.

use x11rb::{
    connection::Connection,
    protocol::{randr::ConnectionExt as _, xproto::ConnectionExt as _},
    rust_connection::RustConnection,
};

/// One connected screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    /// RandR output name, e.g. `eDP-1`.
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// A line of the emulated layout: `NAME WxH+X+Y` per screen.
///
/// Every line is one screen, so a missing or malformed line is skipped rather
/// than shifting the others.
fn emulated() -> Option<Vec<Screen>> {
    let path = std::env::var_os("NIRU_RANDR_EMU").map(std::path::PathBuf::from)?;
    let text = std::fs::read_to_string(path).ok()?;
    Some(parse_layout(&text))
}

pub fn parse_layout(text: &str) -> Vec<Screen> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (name, geometry) = line.split_once(' ')?;
            let (size, rest) = geometry.split_once('+')?;
            let (w, h) = size.split_once('x')?;
            let (x, y) = rest.split_once('+')?;
            Some(Screen {
                name: name.to_string(),
                x: x.parse().ok()?,
                y: y.parse().ok()?,
                width: w.parse().ok()?,
                height: h.parse().ok()?,
            })
        })
        .collect()
}

fn read_randr(c: &RustConnection, screen: usize) -> Result<Vec<Screen>, String> {
    let root = c.setup().roots[screen].root;
    let resources = c
        .randr_get_screen_resources_current(root)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for output in resources.outputs {
        let info = c
            .randr_get_output_info(output, resources.config_timestamp)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        if info.connection != x11rb::protocol::randr::Connection::CONNECTED || info.crtc == 0 {
            continue;
        }
        let crtc = c
            .randr_get_crtc_info(info.crtc, resources.config_timestamp)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        if crtc.width == 0 || crtc.height == 0 {
            continue;
        }
        out.push(Screen {
            name: String::from_utf8_lossy(&info.name).to_string(),
            x: i32::from(crtc.x),
            y: i32::from(crtc.y),
            width: u32::from(crtc.width),
            height: u32::from(crtc.height),
        });
    }
    Ok(out)
}

/// The connected screens, left to right.
///
/// A server without RandR, or one that reports nothing, returns one screen
/// covering the whole display so the window stays usable.
pub fn read(c: &RustConnection, screen: usize) -> Result<Vec<Screen>, String> {
    if let Some(screens) = emulated() {
        return Ok(screens);
    }
    let screens = read_randr(c, screen)?;
    if screens.is_empty() {
        let root = c
            .get_geometry(c.setup().roots[screen].root)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        return Ok(vec![Screen {
            name: "screen".into(),
            x: 0,
            y: 0,
            width: u32::from(root.width),
            height: u32::from(root.height),
        }]);
    }
    Ok(screens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_line_is_one_screen() {
        let screens = parse_layout("eDP-1 1600x900+0+0\nDP-1 1080x1920+1600+0\n");
        assert_eq!(screens.len(), 2);
        assert_eq!(screens[0].name, "eDP-1");
        assert_eq!(screens[1].width, 1080);
        assert_eq!(screens[1].x, 1600);
    }

    #[test]
    fn a_malformed_line_is_skipped() {
        let screens = parse_layout("eDP-1 1600x900+0+0\nnonsense\nDP-1 800x600+-1+0\n");
        assert_eq!(screens.len(), 2);
        assert_eq!(screens[1].x, -1);
    }
}
