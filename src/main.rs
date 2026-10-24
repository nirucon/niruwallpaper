//! NIRU Wallpaper — graphical wallpaper settings for the NIRUX desktop.
//!
//! The window is a front end for NIRUWM's existing wallpaper state: it reads
//! `niructl wallpaper status` and `list`, and every change goes back out
//! through `niructl`. It owns no settings and runs no wallpaper daemon.

mod backend;
mod text;
mod theme;
mod thumbs;
mod window;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("open") => run_window(),
        // A text view of what the window would show, for scripts and for
        // checking an installation without a display.
        Some("status") => print_status(),
        Some("list") => print_list(),
        // Applying one image directly is useful from a script, and it lets a
        // personal wallpaper be chosen without the window.
        Some("set") => match args.get(1) {
            Some(path) => match backend::set_image(path) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("niruwallpaper: {e}");
                    ExitCode::FAILURE
                }
            },
            None => {
                eprintln!("niruwallpaper: set needs an image path");
                ExitCode::from(2)
            }
        },
        Some("--version") => {
            println!("NIRU Wallpaper {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help") => {
            usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("niruwallpaper: unknown command: {other}");
            usage();
            ExitCode::from(2)
        }
    }
}

fn usage() {
    println!("NIRU Wallpaper {}", env!("CARGO_PKG_VERSION"));
    println!("Usage: niruwallpaper [open]");
    println!("       niruwallpaper status   print the current wallpaper settings");
    println!("       niruwallpaper list     print the available wallpapers");
    println!("       niruwallpaper set PATH apply one image");
    println!("       niruwallpaper --version | --help");
}

/// Print the settings the window would show. Needs no X server.
fn print_status() -> ExitCode {
    match backend::read_status() {
        Ok(status) => {
            let shown = if status.follows_theme() {
                format!("theme wallpaper ({})", status.theme)
            } else if status.shows_theme_colour() {
                format!("theme background colour ({})", status.theme)
            } else if status.has_override() {
                format!("explicit image ({})", status.file)
            } else {
                "nothing".to_string()
            };
            println!("theme={}", status.theme);
            println!("enabled={}", status.enabled);
            println!("follow_theme={}", status.follow_theme);
            println!("mode={}", status.mode);
            println!("scale={}", status.scale);
            println!("rotation_minutes={}", status.rotation_minutes);
            println!("image={shown}");
            println!("theme_following={}", status.theme_following);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("niruwallpaper: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_list() -> ExitCode {
    let status = match backend::read_status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("niruwallpaper: {e}");
            return ExitCode::FAILURE;
        }
    };
    match backend::read_list(&status.theme_dir) {
        Ok(entries) => {
            for entry in entries {
                let kind = if entry.official { "official" } else { "mine" };
                println!("{}\t{}\t{kind}", entry.name, entry.path.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("niruwallpaper: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_window() -> ExitCode {
    let mut state = match window::load() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("niruwallpaper: {e}");
            return ExitCode::FAILURE;
        }
    };
    let palette = theme::load();
    let mut win = match window::App::open() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("niruwallpaper: cannot open a window: {e}");
            return ExitCode::FAILURE;
        }
    };
    match window::run(&mut win, &mut state, palette) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("niruwallpaper: {e}");
            ExitCode::FAILURE
        }
    }
}
