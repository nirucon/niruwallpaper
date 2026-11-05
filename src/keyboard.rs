//! Keysyms with their level: Shift, AltGr and Caps Lock.
//!
//! Ported from NIRU Launcher's `keyboard_level`/`keyboard_symbol`, so a text
//! field added here later (a filter, a path) types the same characters as
//! the launcher on a Swedish or any other layout.
//!
//! One difference from the launcher's copy: X's core keyboard map lists a
//! key's symbols as group 1 level 1-2, group 2 level 1-2, then group 1 level
//! 3-4 (XKB protocol, "core keyboard mapping"). AltGr (level 3) is therefore
//! the fifth symbol when the server reports five or more per key; only a
//! shorter list (a test, or an old server) has it third.

use x11rb::protocol::xproto::KeyButMask;

/// Keyboard level: base, Shift, AltGr (MOD5), then Shift+AltGr.
pub fn keyboard_level(state: KeyButMask) -> usize {
    let shift = state.contains(KeyButMask::SHIFT);
    let altgr = state.contains(KeyButMask::MOD5);
    match (shift, altgr) {
        (false, false) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (true, true) => 3,
    }
}

/// X applies Caps Lock to alphabetic keysyms, including Latin-1 letters.
pub fn apply_caps(sym: u32, lock: bool) -> u32 {
    if !lock {
        return sym;
    }
    if (0x61..=0x7a).contains(&sym) || (0xe0..=0xfe).contains(&sym) && sym != 0xf7 {
        return sym.saturating_sub(0x20);
    }
    if (0x41..=0x5a).contains(&sym) || (0xc0..=0xde).contains(&sym) && sym != 0xd7 {
        return sym + 0x20;
    }
    sym
}

/// The keysym one key produces with the given modifiers.
pub fn keyboard_symbol(syms: &[u32], state: KeyButMask) -> u32 {
    let level = keyboard_level(state);
    // Index of each level in the core map; see the module comment.
    let index = if syms.len() > 4 {
        [0, 1, 4, 5][level]
    } else {
        level
    };
    let sym = syms
        .get(index)
        .filter(|s| **s != 0)
        .or_else(|| syms.get(level % 2).filter(|s| **s != 0))
        .or_else(|| syms.first())
        .copied()
        .unwrap_or(0);
    apply_caps(sym, state.contains(KeyButMask::LOCK))
}

/// The keysym a shortcut compares against: letters in lower case, so Caps
/// Lock and Shift do not change which shortcut a letter is.
pub fn shortcut(sym: u32) -> u32 {
    if (0x41..=0x5a).contains(&sym) || (0xc0..=0xde).contains(&sym) && sym != 0xd7 {
        sym + 0x20
    } else {
        sym
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_follows_shift_and_altgr() {
        assert_eq!(keyboard_level(KeyButMask::default()), 0);
        assert_eq!(keyboard_level(KeyButMask::SHIFT), 1);
        assert_eq!(keyboard_level(KeyButMask::MOD5), 2);
        assert_eq!(keyboard_level(KeyButMask::SHIFT | KeyButMask::MOD5), 3);
    }

    #[test]
    fn altgr_and_caps_produce_the_expected_symbols() {
        // The Swedish "2" key as X's core map lists it:
        // 2 quotedbl 2 quotedbl at rightdoublequotemark.
        let core = [0x32, 0x22, 0x32, 0x22, 0x40, 0xad3];
        assert_eq!(keyboard_symbol(&core, KeyButMask::default()), 0x32);
        assert_eq!(keyboard_symbol(&core, KeyButMask::SHIFT), 0x22);
        assert_eq!(
            keyboard_symbol(&core, KeyButMask::MOD5),
            0x40,
            "AltGr+2 is @"
        );
        assert_eq!(
            keyboard_symbol(&core, KeyButMask::SHIFT | KeyButMask::MOD5),
            0xad3
        );
        // The launcher's four-symbol form still works.
        let short = [0x32, 0x22, 0x40, 0x32];
        assert_eq!(keyboard_symbol(&short, KeyButMask::MOD5), 0x40);
        // A key without an AltGr level falls back to its base symbols.
        let plain = [0x61, 0x41, 0x61, 0x41];
        assert_eq!(keyboard_symbol(&plain, KeyButMask::MOD5), 0x61);
        assert_eq!(keyboard_symbol(&plain, KeyButMask::LOCK), 0x41, "Caps Lock");
        assert_eq!(apply_caps(0xe5, true), 0xc5, "Caps Lock applies to å");
        assert_eq!(apply_caps(0x40, true), 0x40, "Caps Lock leaves @ alone");
    }

    #[test]
    fn shortcuts_ignore_case() {
        assert_eq!(shortcut(0x41), 0x61);
        assert_eq!(shortcut(0x61), 0x61);
        assert_eq!(shortcut(0xc5), 0xe5);
        assert_eq!(shortcut(0xff09), 0xff09, "Tab is not a letter");
    }
}
