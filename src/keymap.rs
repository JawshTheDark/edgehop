//! Key codes on the wire are Linux evdev codes (input-event-codes.h).
//! PC set-1 scancodes 0x01..=0x58 map 1:1 onto evdev codes; extended (E0)
//! scancodes and a few special keys need a table.
#![allow(dead_code)]

/// (extended scancode, evdev code)
const EXTENDED: &[(u16, u16)] = &[
    (0x1C, 96),  // KP Enter
    (0x1D, 97),  // Right Ctrl
    (0x35, 98),  // KP /
    (0x37, 99),  // Print Screen
    (0x38, 100), // Right Alt / AltGr
    (0x47, 102), // Home
    (0x48, 103), // Up
    (0x49, 104), // Page Up
    (0x4B, 105), // Left
    (0x4D, 106), // Right
    (0x4F, 107), // End
    (0x50, 108), // Down
    (0x51, 109), // Page Down
    (0x52, 110), // Insert
    (0x53, 111), // Delete
    (0x5B, 125), // Left Meta / Win
    (0x5C, 126), // Right Meta / Win
    (0x5D, 127), // Menu
    (0x5E, 116), // Power
    (0x5F, 142), // Sleep
    (0x20, 113), // Mute
    (0x2E, 114), // Volume Down
    (0x30, 115), // Volume Up
    (0x22, 164), // Play/Pause
    (0x24, 166), // Stop
    (0x10, 165), // Previous track
    (0x19, 163), // Next track
    (0x6A, 158), // Browser back
    (0x69, 159), // Browser forward
    (0x32, 172), // Browser home
    (0x6C, 155), // Mail
    (0x21, 140), // Calculator
];

/// Non-extended scancodes beyond the identity range (international keys).
const INTL: &[(u16, u16)] = &[(0x70, 93), (0x73, 89), (0x79, 92), (0x7B, 94), (0x7D, 124)];

/// (Windows virtual key, evdev code) for keys best handled by VK.
const VK_KEYS: &[(u16, u16)] = &[
    (0x13, 119), // Pause
    (0x90, 69),  // Num Lock
    (0xAD, 113), // Mute
    (0xAE, 114), // Volume Down
    (0xAF, 115), // Volume Up
    (0xB0, 163), // Next
    (0xB1, 165), // Previous
    (0xB2, 166), // Stop
    (0xB3, 164), // Play/Pause
];

pub fn win_to_evdev(scan: u32, extended: bool, vk: u32) -> Option<u16> {
    if let Some(&(_, c)) = VK_KEYS.iter().find(|(v, _)| *v as u32 == vk) {
        return Some(c);
    }
    let scan = scan as u16;
    if extended {
        EXTENDED.iter().find(|(s, _)| *s == scan).map(|&(_, c)| c)
    } else if (1..=88).contains(&scan) {
        Some(scan)
    } else {
        INTL.iter().find(|(s, _)| *s == scan).map(|&(_, c)| c)
    }
}

pub enum WinKey {
    Scan(u16, bool),
    Vk(u16),
}

pub fn evdev_to_win(code: u16) -> Option<WinKey> {
    if let Some(&(vk, _)) = VK_KEYS.iter().find(|(_, c)| *c == code) {
        return Some(WinKey::Vk(vk));
    }
    if (1..=88).contains(&code) {
        return Some(WinKey::Scan(code, false));
    }
    if let Some(&(s, _)) = EXTENDED.iter().find(|(_, c)| *c == code) {
        return Some(WinKey::Scan(s, true));
    }
    INTL.iter().find(|(_, c)| *c == code).map(|&(s, _)| WinKey::Scan(s, false))
}

/// evdev code -> macOS virtual keycode (kVK_*). Win/Super maps to Command.
pub fn evdev_to_mac(code: u16) -> Option<u16> {
    let k = match code {
        1 => 0x35,
        2 => 0x12, 3 => 0x13, 4 => 0x14, 5 => 0x15, 6 => 0x17, 7 => 0x16, 8 => 0x1A, 9 => 0x1C, 10 => 0x19, 11 => 0x1D,
        12 => 0x1B, 13 => 0x18, 14 => 0x33, 15 => 0x30,
        16 => 0x0C, 17 => 0x0D, 18 => 0x0E, 19 => 0x0F, 20 => 0x11, 21 => 0x10, 22 => 0x20, 23 => 0x22, 24 => 0x1F, 25 => 0x23,
        26 => 0x21, 27 => 0x1E, 28 => 0x24, 29 => 0x3B,
        30 => 0x00, 31 => 0x01, 32 => 0x02, 33 => 0x03, 34 => 0x05, 35 => 0x04, 36 => 0x26, 37 => 0x28, 38 => 0x25,
        39 => 0x29, 40 => 0x27, 41 => 0x32, 42 => 0x38, 43 => 0x2A,
        44 => 0x06, 45 => 0x07, 46 => 0x08, 47 => 0x09, 48 => 0x0B, 49 => 0x2D, 50 => 0x2E,
        51 => 0x2B, 52 => 0x2F, 53 => 0x2C, 54 => 0x3C, 55 => 0x43, 56 => 0x3A, 57 => 0x31, 58 => 0x39,
        59 => 0x7A, 60 => 0x78, 61 => 0x63, 62 => 0x76, 63 => 0x60, 64 => 0x61, 65 => 0x62, 66 => 0x64, 67 => 0x65, 68 => 0x6D,
        69 => 0x47, 70 => 0x6B,
        71 => 0x59, 72 => 0x5B, 73 => 0x5C, 74 => 0x4E, 75 => 0x56, 76 => 0x57, 77 => 0x58, 78 => 0x45,
        79 => 0x53, 80 => 0x54, 81 => 0x55, 82 => 0x52, 83 => 0x41,
        86 => 0x0A, 87 => 0x67, 88 => 0x6F,
        96 => 0x4C, 97 => 0x3E, 98 => 0x4B, 99 => 0x69, 100 => 0x3D,
        102 => 0x73, 103 => 0x7E, 104 => 0x74, 105 => 0x7B, 106 => 0x7C, 107 => 0x77, 108 => 0x7D, 109 => 0x79,
        110 => 0x72, 111 => 0x75, 113 => 0x4A, 114 => 0x49, 115 => 0x48, 119 => 0x71,
        125 => 0x37, 126 => 0x36, 127 => 0x6E,
        _ => return None,
    };
    Some(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_round_trip() {
        // 'A' is scancode 0x1E, evdev KEY_A = 30
        assert_eq!(win_to_evdev(0x1E, false, 0x41), Some(30));
        assert!(matches!(evdev_to_win(30), Some(WinKey::Scan(0x1E, false))));
    }

    #[test]
    fn arrows_are_extended() {
        assert_eq!(win_to_evdev(0x48, true, 0x26), Some(103));
        assert!(matches!(evdev_to_win(103), Some(WinKey::Scan(0x48, true))));
        assert_eq!(evdev_to_mac(103), Some(0x7E));
    }

    #[test]
    fn every_extended_round_trips() {
        for &(s, c) in EXTENDED {
            if VK_KEYS.iter().any(|(_, vc)| *vc == c) {
                continue;
            }
            assert_eq!(win_to_evdev(s as u32, true, 0), Some(c));
        }
    }
}
