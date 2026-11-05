//! Where a wallpaper lands on a screen for each scale mode.
//!
//! The settings panel draws the staged picture on a small copy of the chosen
//! screen, so `fill`, `fit`, `stretch` and `center` show their effect before
//! Apply. The geometry is the same NIRUWM uses: `fill` covers the screen and
//! crops, `fit` shows the whole picture with bars, `stretch` ignores the
//! aspect ratio and `center` keeps the picture's own pixel size.

/// A rectangle that may lie partly outside the frame (a cropped picture).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Area {
    /// The part of `self` inside `clip`, if any.
    pub fn intersect(self, clip: Area) -> Option<Area> {
        let x0 = self.x.max(clip.x);
        let y0 = self.y.max(clip.y);
        let x1 = (self.x + self.w).min(clip.x + clip.w);
        let y1 = (self.y + self.h).min(clip.y + clip.h);
        (x1 > x0 && y1 > y0).then_some(Area {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }
}

/// The screen drawn as large as fits in `frame`, keeping its aspect ratio,
/// and centred in it.
pub fn screen_in_frame(frame: Area, screen: (u32, u32)) -> Area {
    let (sw, sh) = (f64::from(screen.0.max(1)), f64::from(screen.1.max(1)));
    let k = (f64::from(frame.w.max(1)) / sw).min(f64::from(frame.h.max(1)) / sh);
    let w = (sw * k).round().max(1.0) as i32;
    let h = (sh * k).round().max(1.0) as i32;
    Area {
        x: frame.x + (frame.w - w) / 2,
        y: frame.y + (frame.h - h) / 2,
        w,
        h,
    }
}

/// Where the picture is drawn for `scale`, in the same coordinates as
/// `on`, the screen drawn by `screen_in_frame` for a screen of
/// `screen` pixels. The result may extend past `on`; the caller clips it.
pub fn place(on: Area, screen: (u32, u32), image: (u32, u32), scale: &str) -> Area {
    let (iw, ih) = (f64::from(image.0.max(1)), f64::from(image.1.max(1)));
    let (ow, oh) = (f64::from(on.w), f64::from(on.h));
    let (w, h) = match scale {
        "stretch" => (ow, oh),
        "fit" => {
            let k = (ow / iw).min(oh / ih);
            (iw * k, ih * k)
        }
        "center" => {
            // The picture keeps its own pixels: scale it like the screen.
            let k = ow / f64::from(screen.0.max(1));
            (iw * k, ih * k)
        }
        // `fill`, and anything unknown, covers the screen.
        _ => {
            let k = (ow / iw).max(oh / ih);
            (iw * k, ih * k)
        }
    };
    let w = w.round().max(1.0) as i32;
    let h = h.round().max(1.0) as i32;
    Area {
        x: on.x + (on.w - w) / 2,
        y: on.y + (on.h - h) / 2,
        w,
        h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Area = Area {
        x: 0,
        y: 0,
        w: 280,
        h: 160,
    };

    #[test]
    fn the_screen_keeps_its_aspect_ratio_in_the_frame() {
        let wide = screen_in_frame(FRAME, (2560, 1440));
        assert_eq!((wide.w, wide.h), (280, 158));
        assert_eq!(wide.y, 1, "centred vertically");
        let portrait = screen_in_frame(FRAME, (1080, 1920));
        assert_eq!((portrait.w, portrait.h), (90, 160));
        assert_eq!(portrait.x, 95, "centred horizontally");
    }

    #[test]
    fn each_scale_places_the_picture_like_the_window_manager() {
        let on = screen_in_frame(FRAME, (1920, 1080));
        assert_eq!((on.w, on.h), (280, 158));
        // A portrait photo on a wide screen.
        let photo = (1200, 1600);
        let fill = place(on, (1920, 1080), photo, "fill");
        assert_eq!(fill.w, on.w, "fill covers the width");
        assert!(fill.h > on.h, "and crops the height");
        assert_eq!(
            fill.intersect(on),
            Some(on),
            "nothing of the screen is bare"
        );
        let fit = place(on, (1920, 1080), photo, "fit");
        assert_eq!(fit.h, on.h, "fit shows the whole height");
        assert!(fit.w < on.w, "with bars at the sides");
        let (left, right) = (fit.x - on.x, on.x + on.w - (fit.x + fit.w));
        assert!((left - right).abs() <= 1, "centred: {left} and {right}");
        assert_eq!(place(on, (1920, 1080), photo, "stretch"), on);
        // Centre keeps pixel size: a 960x540 picture is a quarter screen.
        let small = place(on, (1920, 1080), (960, 540), "center");
        assert_eq!((small.w, small.h), (140, 79));
        assert_eq!(small.x, on.x + 70);
        // An 8K picture centred on a 1080p screen shows its middle only.
        let big = place(on, (1920, 1080), (7680, 4320), "center");
        assert!(big.w > on.w * 3);
        assert_eq!(big.intersect(on), Some(on));
        // The aspect ratio is kept by every mode but stretch.
        for mode in ["fill", "fit", "center"] {
            let p = place(on, (1920, 1080), photo, mode);
            let ratio = f64::from(p.w) / f64::from(p.h);
            assert!((ratio - 0.75).abs() < 0.02, "{mode}: {ratio}");
        }
    }

    #[test]
    fn areas_intersect_only_where_they_overlap() {
        let a = Area {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        let b = Area {
            x: 5,
            y: 8,
            w: 10,
            h: 10,
        };
        assert_eq!(
            a.intersect(b),
            Some(Area {
                x: 5,
                y: 8,
                w: 5,
                h: 2
            })
        );
        assert_eq!(a.intersect(Area { x: 10, ..b }), None);
    }
}
