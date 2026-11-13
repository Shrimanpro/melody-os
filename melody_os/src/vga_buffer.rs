//! 80x25 VGA text mode: a scrolling boot console plus raw cell access for the TUI.
//!
//! `println!` writes to the console *and* the serial port. Once the TUI takes over the
//! screen the console is switched off, so later log lines only go to serial.

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;
use volatile::Volatile;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct ColorCode(pub u8);

impl ColorCode {
    pub const fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ScreenChar {
    pub ascii_character: u8,
    pub color_code: ColorCode,
}

pub const BUFFER_HEIGHT: usize = 25;
pub const BUFFER_WIDTH: usize = 80;

#[repr(transparent)]
pub struct Buffer {
    pub chars: [[Volatile<ScreenChar>; BUFFER_WIDTH]; BUFFER_HEIGHT],
}

pub struct Writer {
    pub column_position: usize,
    pub color_code: ColorCode,
    pub buffer: &'static mut Buffer,
}

lazy_static! {
    pub static ref WRITER: Mutex<Writer> = Mutex::new(Writer {
        column_position: 0,
        color_code: ColorCode::new(Color::LightGray, Color::Black),
        buffer: unsafe { &mut *(0xb8000 as *mut Buffer) },
    });
}

static CONSOLE_ENABLED: AtomicBool = AtomicBool::new(true);

/// Everything printed while the boot console was on, so the TUI can show it later.
/// A fixed buffer: the first lines are printed before the heap exists.
struct BootLog {
    len: usize,
    data: [u8; 32 * 1024],
}

impl fmt::Write for BootLog {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.data.len() - self.len);
        self.data[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// Stop (or resume) mirroring `println!` output to the screen.
pub fn set_console_enabled(enabled: bool) {
    CONSOLE_ENABLED.store(enabled, Ordering::Relaxed);
}

static BOOT_LOG: Mutex<BootLog> = Mutex::new(BootLog { len: 0, data: [0; 32 * 1024] });

fn record(args: fmt::Arguments) {
    use core::fmt::Write;
    if let Some(mut log) = BOOT_LOG.try_lock() {
        let _ = log.write_fmt(args);
    }
}

/// A copy of the boot log text.
pub fn boot_log() -> alloc::string::String {
    let log = BOOT_LOG.lock();
    alloc::string::String::from_utf8_lossy(&log.data[..log.len]).into_owned()
}

impl Writer {
    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            byte => {
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }
                let row = BUFFER_HEIGHT - 1;
                let col = self.column_position;
                self.buffer.chars[row][col].write(ScreenChar {
                    ascii_character: byte,
                    color_code: self.color_code,
                });
                self.column_position += 1;
            }
        }
    }

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                _ => self.write_byte(0xfe),
            }
        }
    }

    pub fn set_color(&mut self, color: ColorCode) {
        self.color_code = color;
    }

    /// Copy a full frame of cells to the screen, touching only cells that differ from `prev`.
    pub fn blit(&mut self, frame: &[[ScreenChar; BUFFER_WIDTH]; BUFFER_HEIGHT], prev: &[[ScreenChar; BUFFER_WIDTH]; BUFFER_HEIGHT]) {
        for row in 0..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                if frame[row][col] != prev[row][col] {
                    self.buffer.chars[row][col].write(frame[row][col]);
                }
            }
        }
    }

    fn new_line(&mut self) {
        for row in 1..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                let character = self.buffer.chars[row][col].read();
                self.buffer.chars[row - 1][col].write(character);
            }
        }
        let blank = ScreenChar { ascii_character: b' ', color_code: self.color_code };
        for col in 0..BUFFER_WIDTH {
            self.buffer.chars[BUFFER_HEIGHT - 1][col].write(blank);
        }
        self.column_position = 0;
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga_buffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// Turn off the blinking hardware text cursor (the TUI draws its own highlights).
pub fn hide_cursor() {
    use x86_64::instructions::port::Port;
    unsafe {
        Port::<u8>::new(0x3D4).write(0x0A);
        Port::<u8>::new(0x3D5).write(0x20);
    }
}

/// Print a boot status line in a given color, e.g. `[ OK ]` in green.
pub fn print_colored(color: Color, args: fmt::Arguments) {
    use core::fmt::Write;
    x86_64::instructions::interrupts::without_interrupts(|| {
        if CONSOLE_ENABLED.load(Ordering::Relaxed) {
            record(args);
            let mut w = WRITER.lock();
            let old = w.color_code;
            w.set_color(ColorCode::new(color, Color::Black));
            let _ = w.write_fmt(args);
            w.set_color(old);
        }
    });
    crate::serial::_print(args);
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    x86_64::instructions::interrupts::without_interrupts(|| {
        if CONSOLE_ENABLED.load(Ordering::Relaxed) {
            record(args);
            let _ = WRITER.lock().write_fmt(args);
        }
    });
    crate::serial::_print(args);
}

/// Print even if the writer lock is held (used by fault handlers that never return).
pub fn force_print(args: fmt::Arguments) {
    use core::fmt::Write;
    unsafe { WRITER.force_unlock() };
    let mut w = WRITER.lock();
    w.set_color(ColorCode::new(Color::White, Color::Red));
    let _ = w.write_fmt(args);
}

#[test_case]
fn test_println_many() {
    for _ in 0..200 {
        println!("test_println_many output");
    }
}

#[test_case]
fn test_println_output() {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    let s = "Some test string that fits on a single line";
    interrupts::without_interrupts(|| {
        let mut writer = WRITER.lock();
        writeln!(writer, "\n{}", s).expect("writeln failed");
        for (i, c) in s.chars().enumerate() {
            let screen_char = writer.buffer.chars[BUFFER_HEIGHT - 2][i].read();
            assert_eq!(char::from(screen_char.ascii_character), c);
        }
    });
}
