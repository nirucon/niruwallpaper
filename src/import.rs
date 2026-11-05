//! The user's own picture folders.

use std::path::PathBuf;

/// A directory from `~/.config/user-dirs.dirs` (`XDG_<NAME>_DIR`), so a
/// localized Pictures or Downloads folder (Bilder, Hämtningar) is found.
pub fn xdg_user_dir(name: &str) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    parse_user_dir(&text, name, &home)
}

fn parse_user_dir(text: &str, name: &str, home: &std::path::Path) -> Option<PathBuf> {
    let key = format!("XDG_{name}_DIR");
    text.lines().find_map(|line| {
        let line = line.trim();
        let value = line.strip_prefix(&key)?.trim_start().strip_prefix('=')?;
        let value = value.trim().trim_matches('"');
        let path = match value.strip_prefix("$HOME") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None if value.starts_with('/') => PathBuf::from(value),
            None => return None,
        };
        // `$HOME/` alone means "disabled" in user-dirs.dirs.
        (path != home).then_some(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dirs_are_read_with_home_expanded() {
        let text = "# comment\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\nXDG_DOWNLOAD_DIR=\"$HOME/\"\n";
        let home = std::path::Path::new("/home/u");
        assert_eq!(
            parse_user_dir(text, "PICTURES", home),
            Some(PathBuf::from("/home/u/Bilder"))
        );
        assert_eq!(parse_user_dir(text, "DOWNLOAD", home), None, "disabled");
        assert_eq!(parse_user_dir(text, "MUSIC", home), None);
    }
}
