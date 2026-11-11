//! The window under a real X server, against a fixture `niructl`.
//!
//! Ignored by default because it needs `Xvfb`, `xdotool` and `xwd`; CI runs
//! it with `cargo test --locked -- --ignored`. The fixture serves 30
//! wallpapers, records every argument vector it is started with, and can be
//! told to take 5 seconds for an Apply. Its `wallpaper style` can act as a
//! niructl from before that command.

use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

const FIXTURE: &str = r#"#!/bin/sh
FX=${NW_FX:?}
printf '%s\n' "$*" >> "$FX/argv.log"
theme_dir="$FX/home/Pictures/Wallpapers/NIRU"
dir="$FX/home/Pictures/Wallpapers"
case "$1 $2" in
"wallpaper list")
  for f in "$theme_dir"/*.png; do n=$(basename "$f" .png); printf '%s\t%s\n' "$n" "$f"; done ;;
"wallpaper status-json")
  img="$theme_dir/nirux-wallpaper-w01.png"
  printf '{"theme":"niru-noir","enabled":true,"mode":"random","rotation_minutes":10,"scale":"fill","follow_theme":true,"file":"","dir":"%s","theme_dir":"%s","theme_image":"%s","outputs":[],"planned":[{"output":"screen","source":"inherit","mode":"theme","scale":"fill","image":"%s"}],"rendered":null}\n' "$dir" "$theme_dir" "$img" "$img" ;;
"wallpaper style")
  if [ -f "$FX/old" ]; then echo "usage: niructl wallpaper list|status" >&2; exit 2; fi
  printf 'theme_style=signature\n' ;;
"wallpaper apply")
  if [ -f "$FX/slow" ]; then sleep "$(cat "$FX/slow")"; fi
  printf 'done\n' >> "$FX/argv.log"
  echo '{"applied":true}' ;;
*) echo "fixture: $*" >&2; exit 2 ;;
esac
"#;

/// Everything the test started, stopped and removed however it ends.
struct Session {
    root: PathBuf,
    xvfb: Child,
    app: Option<Child>,
    display: String,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(app) = self.app.as_mut() {
            let _ = app.kill();
            let _ = app.wait();
        }
        let _ = self.xvfb.kill();
        let _ = self.xvfb.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Session {
    fn log(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("argv.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn xdotool(&self, args: &[&str]) -> String {
        let out = Command::new("xdotool")
            .args(args)
            .env("DISPLAY", &self.display)
            .output()
            .expect("xdotool");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn screenshot(&self, window: &str) -> Vec<u8> {
        Command::new("xwd")
            .args(["-silent", "-id", window])
            .env("DISPLAY", &self.display)
            .output()
            .expect("xwd")
            .stdout
    }

    /// Wait until the fixture log satisfies `test`, or fail after `limit`.
    fn wait_log(&self, limit: Duration, what: &str, test: impl Fn(&[String]) -> bool) {
        let start = Instant::now();
        while !test(&self.log()) {
            assert!(
                start.elapsed() < limit,
                "timed out waiting for {what}: {:?}",
                self.log()
            );
            sleep(Duration::from_millis(50));
        }
    }
}

fn start() -> Session {
    let root = std::env::temp_dir().join(format!("nw-xvfb-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let theme_dir = root.join("home/Pictures/Wallpapers/NIRU");
    fs::create_dir_all(&theme_dir).unwrap();
    fs::create_dir_all(root.join("cfg/niruwm")).unwrap();
    fs::create_dir_all(root.join("cache")).unwrap();
    for i in 1..=30u8 {
        let pixel = image::Rgb([i * 8, 255 - i * 8, 120]);
        image::RgbImage::from_pixel(64, 36, pixel)
            .save(theme_dir.join(format!("nirux-wallpaper-w{i:02}.png")))
            .unwrap();
    }
    fs::write(
        root.join("cfg/niruwm/config.toml"),
        "theme = \"niru-noir\"\n[wallpaper]\nenabled = true\n",
    )
    .unwrap();
    let niructl = root.join("niructl");
    fs::write(&niructl, FIXTURE).unwrap();
    fs::set_permissions(&niructl, fs::Permissions::from_mode(0o755)).unwrap();

    // Xvfb picks a free display and names it on the descriptor given.
    let mut xvfb = Command::new("Xvfb")
        .args([
            "-displayfd",
            "1",
            "-screen",
            "0",
            "1280x800x24",
            "-nolisten",
            "tcp",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Xvfb must be installed for this test");
    let mut line = String::new();
    BufReader::new(xvfb.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let display = format!(":{}", line.trim());
    Session {
        root,
        xvfb,
        app: None,
        display,
    }
}

fn open_window(s: &mut Session) -> String {
    let app = Command::new(env!("CARGO_BIN_EXE_niruwallpaper"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("DISPLAY", &s.display)
        .env("HOME", s.root.join("home"))
        .env("XDG_CONFIG_HOME", s.root.join("cfg"))
        .env("XDG_CACHE_HOME", s.root.join("cache"))
        .env("NIRUWALLPAPER_NIRUCTL", s.root.join("niructl"))
        .env("NW_FX", &s.root)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    s.app = Some(app);
    let start = Instant::now();
    loop {
        let id = s.xdotool(&["search", "--name", "NIRU Wallpaper"]);
        if let Some(id) = id.lines().next().filter(|l| !l.is_empty()) {
            return id.to_string();
        }
        assert!(start.elapsed() < Duration::from_secs(20), "no window");
        sleep(Duration::from_millis(100));
    }
}

fn applies(log: &[String]) -> Vec<&String> {
    log.iter()
        .filter(|l| l.starts_with("wallpaper apply"))
        .collect()
}

#[test]
#[ignore = "needs Xvfb, xdotool and xwd; CI runs it with --ignored"]
fn wheel_idle_and_a_slow_apply_under_xvfb() {
    let mut s = start();
    let window = open_window(&mut s);
    // The window reads the desktop once on start.
    s.wait_log(Duration::from_secs(10), "the first read", |l| {
        l.iter().any(|x| x == "wallpaper list")
    });
    sleep(Duration::from_secs(2));

    // An idle window starts no process.
    let before = s.log().len();
    sleep(Duration::from_secs(10));
    assert_eq!(
        s.log().len(),
        before,
        "spawns while idle: {:?}",
        &s.log()[before..]
    );

    // Fifteen wheel notches over the grid scroll it and stage nothing:
    // Enter afterwards has nothing to apply.
    s.xdotool(&["mousemove", "--window", &window, "200", "300"]);
    for _ in 0..15 {
        s.xdotool(&["click", "5"]);
    }
    sleep(Duration::from_millis(500));
    s.xdotool(&["key", "Return"]);
    sleep(Duration::from_secs(1));
    assert!(applies(&s.log()).is_empty(), "the wheel staged a wallpaper");

    // The grid scrolled to its end: 30 pictures, 4 per row, 3 rows in
    // view, so the first tile shown is the 21st.
    s.xdotool(&["mousemove", "--window", &window, "90", "190", "click", "1"]);
    sleep(Duration::from_millis(300));
    fs::write(s.root.join("slow"), "5").unwrap();
    s.xdotool(&["key", "Return"]);
    s.wait_log(Duration::from_secs(5), "the apply", |l| {
        !applies(l).is_empty()
    });
    let apply = applies(&s.log())[0].clone();
    assert!(
        apply.contains("--image") && apply.contains("nirux-wallpaper-w21.png"),
        "{apply}"
    );

    // During the 5 s Apply the window still repaints: the help view opens.
    sleep(Duration::from_millis(800));
    let applying = s.screenshot(&window);
    s.xdotool(&["key", "F1"]);
    sleep(Duration::from_millis(800));
    let help = s.screenshot(&window);
    assert!(
        !s.log().iter().any(|l| l == "done"),
        "the Apply finished too early to show anything"
    );
    assert!(!applying.is_empty() && !help.is_empty());
    assert_ne!(applying, help, "the window did not repaint while applying");
    s.wait_log(Duration::from_secs(15), "the apply to finish", |l| {
        l.iter().any(|x| x == "done")
    });
    fs::remove_file(s.root.join("slow")).unwrap();

    // A change made elsewhere is read again within a second.
    sleep(Duration::from_secs(1));
    let before = s.log().len();
    let config = s.root.join("cfg/niruwm/config.toml");
    fs::write(config.with_extension("tmp"), "theme = \"niru-nord\"\n").unwrap();
    fs::rename(config.with_extension("tmp"), &config).unwrap();
    s.wait_log(Duration::from_secs(1), "the refresh after a change", |l| {
        l[before..].iter().any(|x| x == "wallpaper status-json")
    });

    // S switches the theme picture through niructl, then reads the desktop.
    // The help view is still open; a key closes it first.
    s.xdotool(&["key", "F1"]);
    sleep(Duration::from_millis(300));
    let before = s.log().len();
    s.xdotool(&["key", "s"]);
    s.wait_log(Duration::from_secs(5), "the style switch", |l| {
        let new = &l[before..];
        new.iter()
            .position(|x| x == "wallpaper style toggle")
            .is_some_and(|i| new[i..].iter().any(|x| x == "wallpaper status-json"))
    });
    // An older niructl refuses the command: the window says so and stays.
    fs::write(s.root.join("old"), "").unwrap();
    sleep(Duration::from_millis(500));
    let before = s.log().len();
    s.xdotool(&["key", "s"]);
    s.wait_log(Duration::from_secs(5), "the refused style switch", |l| {
        l[before..].iter().any(|x| x == "wallpaper style toggle")
    });
    sleep(Duration::from_millis(800));
    assert!(
        s.app.as_mut().unwrap().try_wait().unwrap().is_none(),
        "the window closed after a refused style switch"
    );
    assert!(Path::new(&s.root).exists());
}
