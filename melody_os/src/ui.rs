//! 80x25 text-mode user interface.
//!
//! Drawn into an off-screen frame and copied to VGA memory cell by cell, only where
//! something changed. Rendering happens in the main loop (never from an interrupt).

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::audio::player::{PlayState, Player, Status};
use crate::keyboard::Key;
use crate::vga_buffer::{Color, ColorCode, ScreenChar, BUFFER_HEIGHT as H, BUFFER_WIDTH as W, WRITER};

type Frame = [[ScreenChar; W]; H];

const fn cc(fg: Color, bg: Color) -> ColorCode {
    ColorCode::new(fg, bg)
}

const BG: Color = Color::Black;
const TEXT: ColorCode = cc(Color::LightGray, BG);
const DIM: ColorCode = cc(Color::DarkGray, BG);
const BRIGHT: ColorCode = cc(Color::White, BG);
const ACCENT: ColorCode = cc(Color::LightCyan, BG);
const GOOD: ColorCode = cc(Color::LightGreen, BG);
const WARN: ColorCode = cc(Color::Yellow, BG);
const BAD: ColorCode = cc(Color::LightRed, BG);
const BAR: ColorCode = cc(Color::Black, Color::Cyan);
const BAR_DIM: ColorCode = cc(Color::DarkGray, Color::Cyan);
const SELECTED: ColorCode = cc(Color::Black, Color::LightGray);
const PLAYING_ROW: ColorCode = cc(Color::LightGreen, BG);

// CP437 glyphs
const G_PLAY: u8 = 0x10;
const G_BLOCK: u8 = 0xDB;
const G_SHADE: u8 = 0xB0;
const G_HLINE: u8 = 0xC4;
const G_DOT: u8 = 0xFA;

const LIST_TOP: usize = 9;
const LIST_ROWS: usize = 12;

struct Event {
    text: String,
    at_ms: u64,
}

static EVENT: Mutex<Option<Event>> = Mutex::new(None);
static UI: Mutex<Option<Ui>> = Mutex::new(None);

/// Show a one-line message in the status line (also logged to serial).
pub fn notify(text: String) {
    crate::serial_println!("[ui] {}", text);
    *EVENT.lock() = Some(Event { text, at_ms: crate::interrupts::now_ms() });
}

/// Static facts gathered at boot, shown in the library header and diagnostics.
pub struct BootInfo {
    pub storage: String,
    pub cpu_mhz: u64,
    pub usb_drives: Vec<String>,
}

struct Ui {
    frame: Frame,
    shown: Frame,
    cursor: usize,
    scroll: usize,
    last_track: Option<usize>,
    last_draw_ms: u64,
    last_version: u32,
    dirty: bool,
    boot: BootInfo,
}

pub fn start(boot: BootInfo) {
    crate::vga_buffer::set_console_enabled(false);
    crate::vga_buffer::hide_cursor();
    let blank = ScreenChar { ascii_character: b' ', color_code: TEXT };
    // `shown` starts as something impossible so the first draw repaints every cell.
    let poison = ScreenChar { ascii_character: 0, color_code: ColorCode(0xFF) };
    *UI.lock() = Some(Ui {
        frame: [[blank; W]; H],
        shown: [[poison; W]; H],
        cursor: 0,
        scroll: 0,
        last_track: None,
        last_draw_ms: 0,
        last_version: u32::MAX,
        dirty: true,
        boot,
    });
}

pub fn handle_key(key: Key, player: &mut Player) {
    let mut guard = UI.lock();
    let Some(ui) = guard.as_mut() else { return };
    let count = player.track_count();
    match key {
        Key::Up => ui.cursor = ui.cursor.saturating_sub(1),
        Key::Down => ui.cursor = (ui.cursor + 1).min(count.saturating_sub(1)),
        Key::PageUp => ui.cursor = ui.cursor.saturating_sub(LIST_ROWS),
        Key::PageDown => ui.cursor = (ui.cursor + LIST_ROWS).min(count.saturating_sub(1)),
        Key::Enter => {
            if count > 0 {
                player.play(ui.cursor);
            }
        }
        Key::PlayPause => player.toggle_pause(),
        Key::Stop => player.stop(),
        Key::Next => player.next(),
        Key::Prev => player.prev(),
        Key::VolUp => player.change_volume(5),
        Key::VolDown => player.change_volume(-5),
        Key::Mute => player.toggle_mute(),
        Key::Output => player.cycle_output(),
        Key::Diagnostics | Key::BootLog => {}
    }
    ui.dirty = true;
}

/// Redraw if anything changed (or at least every 250 ms so the clock moves).
pub fn render(player: &Player) {
    let mut guard = UI.lock();
    let Some(ui) = guard.as_mut() else { return };
    let now = crate::interrupts::now_ms();
    if !ui.dirty && player.version == ui.last_version && now - ui.last_draw_ms < 250 {
        return;
    }
    ui.dirty = false;
    ui.last_version = player.version;
    ui.last_draw_ms = now;

    let status = player.status();

    // Follow the playing track with the cursor when it changes.
    if status.track != ui.last_track {
        if let Some(t) = status.track {
            ui.cursor = t;
        }
        ui.last_track = status.track;
    }

    ui.clear();
    ui.draw_header();
    ui.draw_now_playing(player, &status);
    ui.rule(LIST_TOP + LIST_ROWS + 1, "");
    ui.draw_library(player, &status);
    ui.draw_footer(now);

    let (frame, shown) = (&ui.frame, &mut ui.shown);
    x86_64::instructions::interrupts::without_interrupts(|| WRITER.lock().blit(frame, shown));
    *shown = *frame;
}

/// Small fixed-capacity line builder so drawing code reads naturally.
struct Line(String);

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.push_str(s);
        Ok(())
    }
}

macro_rules! fmt {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let mut l = Line(String::new());
        let _ = write!(l, $($arg)*);
        l.0
    }};
}

fn mmss(ms: u64) -> String {
    let s = ms / 1000;
    fmt!("{}:{:02}", s / 60, s % 60)
}

fn khz(rate: u32) -> String {
    if rate % 1000 == 0 { fmt!("{} kHz", rate / 1000) } else { fmt!("{}.{} kHz", rate / 1000, rate % 1000 / 100) }
}

impl Ui {
    fn clear(&mut self) {
        for row in self.frame.iter_mut() {
            for c in row.iter_mut() {
                *c = ScreenChar { ascii_character: b' ', color_code: TEXT };
            }
        }
    }

    fn put(&mut self, row: usize, col: usize, ch: u8, color: ColorCode) {
        if row < H && col < W {
            self.frame[row][col] = ScreenChar { ascii_character: ch, color_code: color };
        }
    }

    /// Write text, clipped to `max` columns (with a trailing '~' when cut). Returns end column.
    fn text(&mut self, row: usize, col: usize, s: &str, color: ColorCode, max: usize) -> usize {
        let max = max.min(W.saturating_sub(col));
        let bytes = s.as_bytes();
        let cut = bytes.len() > max;
        let n = if cut { max.saturating_sub(1) } else { bytes.len() };
        for (i, &b) in bytes[..n].iter().enumerate() {
            let b = if (0x20..0x7F).contains(&b) { b } else { b'?' };
            self.put(row, col + i, b, color);
        }
        if cut && max > 0 {
            self.put(row, col + n, b'~', color);
            return col + max;
        }
        col + n
    }

    fn fill(&mut self, row: usize, from: usize, to: usize, ch: u8, color: ColorCode) {
        for c in from..to.min(W) {
            self.put(row, c, ch, color);
        }
    }

    fn rule(&mut self, row: usize, title: &str) {
        self.fill(row, 0, W, G_HLINE, DIM);
        if !title.is_empty() {
            self.put(row, 1, b' ', DIM);
            let end = self.text(row, 2, title, ACCENT, W - 4);
            self.put(row, end, b' ', DIM);
        }
    }

    fn draw_header(&mut self) {
        self.fill(0, 0, W, b' ', BAR);
        self.text(0, 1, "MELODY OS", BAR, 20);
    }

    fn draw_now_playing(&mut self, player: &Player, s: &Status) {
        let tracks = player.tracks();
        let (icon, label, color) = match s.state {
            PlayState::Playing => (G_PLAY, "PLAYING", GOOD),
            PlayState::Paused => (b'|', "PAUSED ", WARN),
            PlayState::Stopped => (0xFE, "STOPPED", DIM),
        };
        self.put(2, 2, icon, color);
        if s.state == PlayState::Paused {
            self.put(2, 3, b'|', color);
        }
        self.text(2, 5, label, color, 7);

        match s.track.and_then(|i| tracks.get(i)) {
            Some(t) => {
                self.text(2, 14, t.title(), BRIGHT, W - 16);
                let dir = t.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("/");
                self.text(3, 14, dir, DIM, W - 16);
            }
            None if tracks.is_empty() => {
                self.text(2, 14, "No music found - see the event line below", WARN, W - 16);
            }
            None => {
                self.text(2, 14, "Select a track and press ENTER", TEXT, W - 16);
            }
        }

        // Progress bar
        let bar_w = 50;
        let filled = if s.duration_ms > 0 { (s.elapsed_ms.min(s.duration_ms) * bar_w as u64 / s.duration_ms) as usize } else { 0 };
        self.fill(4, 14, 14 + filled, G_BLOCK, ACCENT);
        self.fill(4, 14 + filled, 14 + bar_w, G_SHADE, DIM);
        let time = if s.duration_ms > 0 { fmt!("{} / {}", mmss(s.elapsed_ms), mmss(s.duration_ms)) } else { mmss(s.elapsed_ms) };
        self.text(4, 15 + bar_w, &time, TEXT, 15);

        // Source format
        self.text(5, 2, "Source", DIM, 10);
        match s.info {
            Some(i) => {
                let ch = match i.channels {
                    1 => String::from("mono"),
                    2 => String::from("stereo"),
                    n => fmt!("{}ch (first 2 played)", n),
                };
                let line = fmt!("{}  {}  {}-bit  {}  {} kbps", s.codec, khz(i.sample_rate), i.bits, ch, i.kbps);
                self.text(5, 14, &line, TEXT, W - 16);
            }
            None => {
                self.text(5, 14, "-", DIM, 10);
            }
        }

        // Output route + what the DAC is actually being sent
        self.text(6, 2, "Output", DIM, 10);
        match s.output {
            Some(o) => {
                let mut col = self.text(6, 14, o.kind.name(), ACCENT, 24);
                if !s.output_pins.is_empty() {
                    col = self.text(6, col + 1, &fmt!("[{}]", s.output_pins), DIM, W - col - 2);
                }
                let _ = col;
            }
            None => {
                self.text(6, 14, "No sound chip found", BAD, 40);
            }
        }
        let vol = if s.muted { String::from("MUTED") } else { fmt!("{}%", s.volume) };
        let vol_kind = if s.hw_volume { "codec amp" } else { "software" };
        let stream = match s.out_format {
            Some(f) => {
                let dec = if s.decimate > 1 { fmt!(" (downsampled /{})", s.decimate) } else { String::new() };
                fmt!("DAC gets {} {}-bit{}", khz(f.rate), f.bits, dec)
            }
            None => String::from("DAC idle"),
        };
        let line = fmt!("{}   Volume {} ({})", stream, vol, vol_kind);
        self.text(7, 14, &line, if s.muted { WARN } else { TEXT }, W - 16);

        // Buffer health
        self.text(8, 2, "Buffer", DIM, 10);
        if s.state == PlayState::Playing || s.state == PlayState::Paused {
            let w = 10usize;
            let fill = (s.buffer_ms.min(2000) as usize * w / 2000).max(if s.buffer_ms > 0 { 1 } else { 0 });
            let color = if s.buffer_ms < 100 { BAD } else if s.buffer_ms < 300 { WARN } else { GOOD };
            self.fill(8, 14, 14 + fill, G_BLOCK, color);
            self.fill(8, 14 + fill, 14 + w, G_SHADE, DIM);
            let line = fmt!(" {} ms queued   decode CPU {}%   underruns {}", s.buffer_ms, s.decode_load, s.underruns);
            self.text(8, 14 + w, &line, if s.underruns > 0 { WARN } else { DIM }, W - 26);
        } else {
            self.text(8, 14, "-", DIM, 4);
        }
    }

    fn draw_library(&mut self, player: &Player, s: &Status) {
        let tracks = player.tracks();
        let title = fmt!("Library: {} tracks on {}", tracks.len(), self.boot.storage);
        self.rule(LIST_TOP, &title);

        if tracks.is_empty() {
            self.text(LIST_TOP + 2, 4, "Nothing to play.", WARN, 70);
            self.text(LIST_TOP + 4, 4, "Put .wav/.flac/.mp3 files on a FAT32 USB stick, plug it into a", TEXT, 74);
            self.text(LIST_TOP + 5, 4, "USB 2.0 port and reboot. Folders like Artist/Album/Song are fine.", TEXT, 74);
            return;
        }

        let cursor = self.cursor.min(tracks.len() - 1);
        if cursor < self.scroll {
            self.scroll = cursor;
        } else if cursor >= self.scroll + LIST_ROWS {
            self.scroll = cursor + 1 - LIST_ROWS;
        }

        for row in 0..LIST_ROWS {
            let i = self.scroll + row;
            let Some(t) = tracks.get(i) else { break };
            let y = LIST_TOP + 1 + row;
            let is_cur = Some(i) == s.track && s.state != PlayState::Stopped;
            let color = if i == cursor { SELECTED } else if is_cur { PLAYING_ROW } else { TEXT };
            if i == cursor {
                self.fill(y, 1, W - 1, b' ', SELECTED);
            }
            if is_cur {
                self.put(y, 2, G_PLAY, if i == cursor { SELECTED } else { PLAYING_ROW });
            }
            let num = fmt!("{:>4}", i + 1);
            self.text(y, 4, &num, if i == cursor { SELECTED } else { DIM }, 4);
            self.text(y, 10, &t.path, color, W - 18);
            self.text(y, W - 6, t.kind.label(), if i == cursor { SELECTED } else { DIM }, 4);
        }

        // Scroll position indicator
        if tracks.len() > LIST_ROWS {
            let pos = fmt!(" {}-{} of {} ", self.scroll + 1, (self.scroll + LIST_ROWS).min(tracks.len()), tracks.len());
            self.text(LIST_TOP + LIST_ROWS + 1, W - pos.len() - 2, &pos, DIM, pos.len());
        }
    }

    fn draw_footer(&mut self, now: u64) {
        if let Some(e) = EVENT.lock().as_ref() {
            let age = (now.saturating_sub(e.at_ms)) / 1000;
            let color = if age < 5 { WARN } else { TEXT };
            self.put(23, 1, G_DOT, color);
            self.text(23, 3, &e.text, color, W - 4);
        }
        let help = " SPACE play/pause  N/P next/prev  ENTER play  +/- vol  M mute  O out";
        self.text(24, 0, help, DIM, W);
    }
}
