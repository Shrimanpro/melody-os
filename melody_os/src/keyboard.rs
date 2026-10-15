//! Turns raw input (PS/2 scancodes, USB HID usages, serial bytes) into player commands.

/// A user command, independent of where the key press came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    PlayPause,
    Stop,
    Next,
    Prev,
    Up,
    Down,
    PageUp,
    PageDown,
    Enter,
    VolUp,
    VolDown,
    Mute,
    Output,
    Diagnostics,
    BootLog,
}

/// Decoder for PS/2 scancode set 1 (what the 8042 delivers with BIOS translation on).
pub struct Ps2Decoder {
    extended: bool,
}

impl Ps2Decoder {
    pub const fn new() -> Self {
        Ps2Decoder { extended: false }
    }

    pub fn feed(&mut self, code: u8) -> Option<Key> {
        if code == 0xE0 {
            self.extended = true;
            return None;
        }
        let extended = core::mem::replace(&mut self.extended, false);
        if code & 0x80 != 0 {
            return None; // key release
        }
        if extended {
            return match code {
                0x48 => Some(Key::Up),
                0x50 => Some(Key::Down),
                0x4B => Some(Key::Prev),
                0x4D => Some(Key::Next),
                0x49 => Some(Key::PageUp),
                0x51 => Some(Key::PageDown),
                0x1C => Some(Key::Enter),
                _ => None,
            };
        }
        match code {
            0x39 => Some(Key::PlayPause), // Space
            0x1C => Some(Key::Enter),
            0x31 => Some(Key::Next),      // N
            0x19 => Some(Key::Prev),      // P
            0x1F => Some(Key::Stop),      // S
            0x0D | 0x4E => Some(Key::VolUp),   // = / keypad +
            0x0C | 0x4A => Some(Key::VolDown), // - / keypad -
            0x32 => Some(Key::Mute),      // M
            0x18 => Some(Key::Output),    // O
            0x20 => Some(Key::Diagnostics), // D
            0x26 => Some(Key::BootLog),   // L
            0x48 => Some(Key::Up),        // keypad 8 / arrows without numlock
            0x50 => Some(Key::Down),
            _ => None,
        }
    }
}

/// Map a USB HID keyboard usage ID (boot protocol) to a command.
pub fn from_usb_usage(usage: u8) -> Option<Key> {
    match usage {
        0x2C => Some(Key::PlayPause),       // Space
        0x28 | 0x58 => Some(Key::Enter),    // Enter / keypad Enter
        0x11 | 0x4F => Some(Key::Next),     // N / Right
        0x13 | 0x50 => Some(Key::Prev),     // P / Left
        0x52 => Some(Key::Up),
        0x51 => Some(Key::Down),
        0x4B => Some(Key::PageUp),
        0x4E => Some(Key::PageDown),
        0x16 => Some(Key::Stop),            // S
        0x2E | 0x57 => Some(Key::VolUp),    // = / keypad +
        0x2D | 0x56 => Some(Key::VolDown),  // - / keypad -
        0x10 => Some(Key::Mute),            // M
        0x12 => Some(Key::Output),          // O
        0x07 => Some(Key::Diagnostics),     // D
        0x0F => Some(Key::BootLog),         // L
        _ => None,
    }
}

/// Decoder for keys typed on a serial terminal (handy when testing under QEMU).
pub struct SerialDecoder {
    state: u8,
}

impl SerialDecoder {
    pub const fn new() -> Self {
        SerialDecoder { state: 0 }
    }

    pub fn feed(&mut self, byte: u8) -> Option<Key> {
        match self.state {
            0 => match byte {
                0x1B => {
                    self.state = 1;
                    None
                }
                b' ' => Some(Key::PlayPause),
                b'\r' | b'\n' => Some(Key::Enter),
                b'n' | b'N' => Some(Key::Next),
                b'p' | b'P' => Some(Key::Prev),
                b's' | b'S' => Some(Key::Stop),
                b'+' | b'=' => Some(Key::VolUp),
                b'-' | b'_' => Some(Key::VolDown),
                b'm' | b'M' => Some(Key::Mute),
                b'o' | b'O' => Some(Key::Output),
                b'd' | b'D' => Some(Key::Diagnostics),
                b'l' | b'L' => Some(Key::BootLog),
                b'k' => Some(Key::Up),
                b'j' => Some(Key::Down),
                _ => None,
            },
            1 => {
                self.state = if byte == b'[' { 2 } else { 0 };
                None
            }
            _ => {
                self.state = 0;
                match byte {
                    b'A' => Some(Key::Up),
                    b'B' => Some(Key::Down),
                    b'C' => Some(Key::Next),
                    b'D' => Some(Key::Prev),
                    _ => None,
                }
            }
        }
    }
}

#[test_case]
fn test_ps2_set1_decoding() {
    let mut d = Ps2Decoder::new();
    assert_eq!(d.feed(0x39), Some(Key::PlayPause));
    assert_eq!(d.feed(0xB9), None); // release
    assert_eq!(d.feed(0xE0), None);
    assert_eq!(d.feed(0x50), Some(Key::Down));
    assert_eq!(d.feed(0xE0), None);
    assert_eq!(d.feed(0xD0), None); // extended release
    assert_eq!(d.feed(0x31), Some(Key::Next));
}
