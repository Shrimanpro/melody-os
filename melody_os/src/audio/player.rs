//! The jukebox: library + decoder + HDA output, driven from the main loop.
//!
//! All access goes through `PLAYER`, which is only ever locked from the main loop (never
//! from an interrupt handler), so it cannot deadlock against the hardware drivers.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use super::hda::{HdaController, OutputKind, PcmFormat};
use super::{BufferedFile, Decoder, Packer, StreamInfo};
use crate::fs::{FatVolume, Track};
use crate::usb::msc::UsbStorageDrive;

pub static PLAYER: Mutex<Option<Player>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Stopped,
    Playing,
    Paused,
}

impl PlayState {
    pub fn mpd_name(&self) -> &'static str {
        match self {
            PlayState::Stopped => "stop",
            PlayState::Playing => "play",
            PlayState::Paused => "pause",
        }
    }
}

pub struct Library {
    pub drive: UsbStorageDrive,
    pub fs: FatVolume,
    pub tracks: Vec<Track>,
}

struct Current {
    idx: usize,
    file: BufferedFile,
    decoder: Box<dyn Decoder>,
    info: StreamInfo,
    packer: Packer,
    fmt: PcmFormat,
    bytes_out: u64,
    /// TSC cycles spent inside the decoder, and how much audio that produced.
    decode_cycles: u64,
    decoded_frames: u64,
    eof: bool,
    pending: Vec<u8>,
    pending_off: usize,
    samples: Vec<i32>,
}

/// One selectable output: (controller index, kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    pub ctrl: usize,
    pub kind: OutputKind,
}

/// Everything the UI and MPD need to show, copied out under the lock.
#[derive(Clone)]
pub struct Status {
    pub state: PlayState,
    pub track: Option<usize>,
    pub elapsed_ms: u64,
    pub duration_ms: u64,
    pub codec: &'static str,
    pub info: Option<StreamInfo>,
    pub out_format: Option<PcmFormat>,
    pub decimate: usize,
    pub volume: u8,
    pub muted: bool,
    pub hw_volume: bool,
    pub output: Option<Output>,
    pub output_pins: String,
    pub outputs: Vec<Output>,
    pub buffer_ms: u64,
    /// Share of real time spent reading + decoding, in percent (100 = can't keep up).
    pub decode_load: u32,
    pub underruns: u32,
    pub lpib: u32,
    pub repeat: bool,
}

pub struct Player {
    pub library: Option<Library>,
    hdas: Vec<HdaController>,
    outputs: Vec<Output>,
    output: Option<usize>,
    state: PlayState,
    current: Option<Current>,
    /// Track to resume from after `stop` (or a failed `play`).
    stopped_at: Option<usize>,
    volume: u8,
    muted: bool,
    pub repeat: bool,
    /// Bumped on every state change; the UI redraws and MPD `idle` wakes up when it moves.
    pub version: u32,
}

/// Bytes of real audio below which silence is queued to avoid replaying stale data.
const LOW_WATER: usize = 16 * 1024;

impl Player {
    pub fn new(hdas: Vec<HdaController>, library: Option<Library>) -> Self {
        let mut outputs = Vec::new();
        for (ctrl, h) in hdas.iter().enumerate() {
            for kind in h.available_outputs() {
                outputs.push(Output { ctrl, kind });
            }
        }
        let mut p = Player {
            library,
            hdas,
            outputs,
            output: None,
            state: PlayState::Stopped,
            current: None,
            stopped_at: None,
            volume: 80,
            muted: false,
            repeat: false,
            version: 0,
        };
        // Default to the analog jack when there is one.
        let first = p.outputs.iter().position(|o| o.kind == OutputKind::Analog).or(if p.outputs.is_empty() { None } else { Some(0) });
        if let Some(i) = first {
            p.select_output(i);
        }
        p
    }

    pub fn tracks(&self) -> &[Track] {
        self.library.as_ref().map(|l| l.tracks.as_slice()).unwrap_or(&[])
    }

    pub fn track_count(&self) -> usize {
        self.tracks().len()
    }

    pub fn state(&self) -> PlayState {
        self.state
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current.as_ref().map(|c| c.idx)
    }

    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }

    pub fn active_output(&self) -> Option<Output> {
        self.output.map(|i| self.outputs[i])
    }

    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    fn hda(&mut self) -> Option<&mut HdaController> {
        let o = self.active_output()?;
        self.hdas.get_mut(o.ctrl)
    }

    pub fn select_output(&mut self, idx: usize) {
        let Some(&new) = self.outputs.get(idx) else { return };
        let old = self.active_output();
        if old == Some(new) {
            return;
        }
        let resume = self.current.as_ref().map(|c| c.idx).filter(|_| self.state != PlayState::Stopped);
        if old.map(|o| o.ctrl) != Some(new.ctrl) {
            // Moving to another controller: stop the stream there and restart the track.
            if let Some(o) = old {
                self.hdas[o.ctrl].stop();
            }
            self.current = None;
        }
        self.output = Some(idx);
        if new.kind == OutputKind::Digital {
            // HDMI only carries sound once the graphics side is in HDMI mode.
            match crate::gpu::enable_hdmi_audio() {
                Ok(()) => crate::println!("[gpu] HDMI audio enabled: {}", crate::gpu::status()),
                Err(e) => crate::ui::notify(alloc::format!("HDMI audio not enabled: {}", e)),
            }
        }
        let (vol, muted) = (self.volume, self.muted);
        let h = &mut self.hdas[new.ctrl];
        h.set_output(new.kind);
        h.set_volume(vol, muted);
        self.update_soft_volume();
        crate::ui::notify(alloc::format!("Output: {}", new.kind.name()));
        if self.current.is_none() {
            if let Some(i) = resume {
                self.play(i);
            }
        }
        self.bump();
    }

    pub fn cycle_output(&mut self) {
        if self.outputs.len() > 1 {
            let next = self.output.map(|i| (i + 1) % self.outputs.len()).unwrap_or(0);
            self.select_output(next);
        } else {
            crate::ui::notify("Only one audio output available".into());
        }
    }

    fn update_soft_volume(&mut self) {
        let hw = self.hda().map(|h| h.has_hw_volume()).unwrap_or(true);
        let soft = if hw { None } else if self.muted { Some(0) } else { Some(self.volume) };
        if let Some(c) = self.current.as_mut() {
            c.packer.soft_volume = soft;
        }
    }

    pub fn volume(&self) -> u8 {
        self.volume
    }

    pub fn set_volume(&mut self, v: u8) {
        self.volume = v.min(100);
        let (vol, muted) = (self.volume, self.muted);
        if let Some(h) = self.hda() {
            h.set_volume(vol, muted);
        }
        self.update_soft_volume();
        self.bump();
    }

    pub fn change_volume(&mut self, delta: i32) {
        let v = (self.volume as i32 + delta).clamp(0, 100) as u8;
        self.set_volume(v);
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        let v = self.volume;
        self.set_volume(v);
    }

    /// Start playing track `idx` from the beginning.
    pub fn play(&mut self, idx: usize) {
        if idx >= self.track_count() {
            return;
        }
        self.stopped_at = Some(idx);
        match self.open_track(idx) {
            Ok(()) => {
                self.state = PlayState::Playing;
                let name = String::from(self.tracks()[idx].title());
                crate::ui::notify(alloc::format!("Playing: {}", name));
            }
            Err(e) => {
                let name = String::from(self.tracks()[idx].file_name());
                crate::println!("[player] cannot play {}: {}", name, e);
                crate::ui::notify(alloc::format!("Can't play {}: {}", name, e));
                self.current = None;
                self.state = PlayState::Stopped;
                if let Some(h) = self.hda() {
                    h.stop();
                }
            }
        }
        self.bump();
    }

    fn open_track(&mut self, idx: usize) -> Result<(), &'static str> {
        self.current = None;
        let out = self.active_output().ok_or("no audio output")?;
        let lib = self.library.as_mut().ok_or("no music library")?;
        let track = lib.tracks[idx].clone();

        let mut file = BufferedFile::new(track.first_cluster, track.size);
        let decoder = {
            let mut input = file.input(&mut lib.fs, &mut lib.drive);
            super::open_decoder(track.kind, &mut input, track.size as u64)?
        };
        let info = decoder.info();

        // Find a rate the codec accepts: native, or 2x/4x downsampled for hi-res files.
        let hda = &mut self.hdas[out.ctrl];
        let mut chosen = None;
        for d in [1u32, 2, 4] {
            if info.sample_rate % d != 0 {
                continue;
            }
            if let Ok(f) = hda.negotiate(info.sample_rate / d, info.bits) {
                chosen = Some((f, d as usize));
                break;
            }
        }
        let (fmt, decimate) = chosen.ok_or("sample rate not supported by the sound chip")?;
        hda.prepare(fmt)?;

        let soft_volume = if hda.has_hw_volume() { None } else if self.muted { Some(0) } else { Some(self.volume) };
        self.current = Some(Current {
            idx,
            file,
            decoder,
            info,
            packer: Packer { out_bits: fmt.bits, decimate, soft_volume },
            fmt,
            bytes_out: 0,
            decode_cycles: 0,
            decoded_frames: 0,
            eof: false,
            pending: Vec::with_capacity(64 * 1024),
            pending_off: 0,
            samples: Vec::with_capacity(16 * 1024),
        });

        // Prefill about a quarter second before starting the DMA engine.
        let target = fmt.rate as usize * fmt.frame_bytes() / 4;
        for _ in 0..64 {
            self.fill(2);
            let queued = self.hda().map(|h| h.queued_audio()).unwrap_or(0);
            let eof = self.current.as_ref().map(|c| c.eof).unwrap_or(true);
            if queued >= target || eof {
                break;
            }
        }
        if let Some(h) = self.hda() {
            h.start();
        }
        Ok(())
    }

    /// Decode and queue audio for up to `budget_ms`. Returns false on a decode error.
    fn fill(&mut self, budget_ms: u64) -> bool {
        let Some(out) = self.active_output() else { return true };
        let (Some(cur), Some(lib)) = (self.current.as_mut(), self.library.as_mut()) else { return true };
        let hda = &mut self.hdas[out.ctrl];
        let deadline = crate::interrupts::Deadline::after_ms(budget_ms);

        loop {
            if cur.pending_off < cur.pending.len() {
                let n = hda.write(&cur.pending[cur.pending_off..]);
                cur.pending_off += n;
                cur.bytes_out += n as u64;
                if cur.pending_off < cur.pending.len() {
                    return true; // ring is full
                }
                cur.pending.clear();
                cur.pending_off = 0;
            }
            if cur.eof || deadline.expired() {
                return true;
            }
            cur.samples.clear();
            let t0 = crate::interrupts::rdtsc();
            let more = {
                let mut input = cur.file.input(&mut lib.fs, &mut lib.drive);
                cur.decoder.decode(&mut input, &mut cur.samples)
            };
            cur.decode_cycles += crate::interrupts::rdtsc() - t0;
            cur.decoded_frames += (cur.samples.len() / 2) as u64;
            cur.packer.pack(&cur.samples, &mut cur.pending);
            match more {
                Ok(true) => {}
                Ok(false) => cur.eof = true,
                Err(e) => {
                    crate::println!("[player] decode error: {}", e);
                    crate::ui::notify(alloc::format!("Read/decode error: {}", e));
                    cur.eof = true;
                    return false;
                }
            }
        }
    }

    /// Called from the main loop as often as possible.
    pub fn poll(&mut self) {
        if self.state != PlayState::Playing || self.current.is_none() {
            return;
        }
        self.fill(4);
        let (eof, drained) = {
            let cur = self.current.as_ref().unwrap();
            (cur.eof, cur.pending_off >= cur.pending.len())
        };
        let Some(h) = self.hda() else { return };
        // Always re-read the DMA position: after EOF nothing else does, and without it the
        // end of the track would never be noticed (and the ring would loop stale audio).
        h.update_position();
        let queued = h.queued_audio();
        if queued < LOW_WATER {
            h.pad_silence(LOW_WATER);
        }
        if eof && drained && queued == 0 {
            self.track_finished();
        }
    }

    fn track_finished(&mut self) {
        let load = self.status().decode_load;
        crate::serial_println!("[player] track done, read+decode load {}% of real time", load);
        let idx = self.current.as_ref().map(|c| c.idx).unwrap_or(0);
        let count = self.track_count();
        // Move on to the next track that actually opens, skipping broken/unsupported files.
        for step in 1..=count {
            let next = idx + step;
            if next >= count && !self.repeat {
                break;
            }
            self.play(next % count);
            if self.state == PlayState::Playing {
                return;
            }
        }
        self.stop();
    }

    pub fn toggle_pause(&mut self) {
        match self.state {
            PlayState::Playing => self.pause(),
            PlayState::Paused => self.resume(),
            PlayState::Stopped => {
                let idx = self.current.as_ref().map(|c| c.idx).unwrap_or(0);
                self.play(idx);
            }
        }
    }

    pub fn pause(&mut self) {
        if self.state == PlayState::Playing {
            if let Some(h) = self.hda() {
                h.pause();
            }
            self.state = PlayState::Paused;
            self.bump();
        }
    }

    pub fn resume(&mut self) {
        if self.state == PlayState::Paused {
            if let Some(h) = self.hda() {
                h.start();
            }
            self.state = PlayState::Playing;
            self.bump();
        }
    }

    pub fn stop(&mut self) {
        if let Some(h) = self.hda() {
            h.stop();
        }
        // Keep the index so "play" resumes from the same track.
        if let Some(c) = self.current.as_mut() {
            c.eof = true;
        }
        let idx = self.current.as_ref().map(|c| c.idx);
        self.current = None;
        self.state = PlayState::Stopped;
        self.stopped_at = idx;
        self.bump();
    }

    pub fn next(&mut self) {
        let count = self.track_count();
        if count == 0 {
            return;
        }
        let idx = self.current_or_stopped().map(|i| (i + 1) % count).unwrap_or(0);
        self.play(idx);
    }

    pub fn prev(&mut self) {
        let count = self.track_count();
        if count == 0 {
            return;
        }
        // Like most players: restart the song if we're more than 3 s in.
        if self.status().elapsed_ms > 3000 {
            if let Some(i) = self.current_index() {
                self.play(i);
                return;
            }
        }
        let idx = self.current_or_stopped().map(|i| (i + count - 1) % count).unwrap_or(0);
        self.play(idx);
    }

    fn current_or_stopped(&self) -> Option<usize> {
        self.current_index().or(self.stopped_at)
    }

    pub fn status(&self) -> Status {
        let out = self.active_output();
        let hda = out.and_then(|o| self.hdas.get(o.ctrl));
        let (elapsed_ms, duration_ms, codec, info, out_format, decimate, buffer_ms, decode_load) = match &self.current {
            Some(c) => {
                let fb = c.fmt.frame_bytes() as u64;
                let queued = hda.map(|h| h.queued_audio() as u64).unwrap_or(0);
                let played_frames = c.bytes_out.saturating_sub(queued) / fb;
                let elapsed = played_frames * 1000 / c.fmt.rate as u64;
                let duration = if c.info.sample_rate > 0 { c.info.total_frames * 1000 / c.info.sample_rate as u64 } else { 0 };
                let buffer_ms = queued / fb * 1000 / c.fmt.rate as u64;
                let audio_ms = c.decoded_frames * 1000 / c.info.sample_rate.max(1) as u64;
                let cpu_ms = c.decode_cycles / crate::interrupts::TSC_CYCLES_PER_MS.load(core::sync::atomic::Ordering::Relaxed).max(1);
                let load = if audio_ms > 0 { (cpu_ms * 100 / audio_ms) as u32 } else { 0 };
                (elapsed, duration, c.decoder.codec_name(), Some(c.info), Some(c.fmt), c.packer.decimate, buffer_ms, load)
            }
            None => (0, 0, "", None, None, 1, 0, 0),
        };
        Status {
            state: self.state,
            track: self.current_or_stopped(),
            elapsed_ms,
            duration_ms,
            codec,
            info,
            out_format,
            decimate,
            volume: self.volume,
            muted: self.muted,
            hw_volume: hda.map(|h| h.has_hw_volume()).unwrap_or(false),
            output: out,
            output_pins: hda.map(|h| h.active_pins()).unwrap_or_default(),
            outputs: self.outputs.clone(),
            buffer_ms,
            decode_load,
            underruns: hda.map(|h| h.underruns).unwrap_or(0),
            lpib: hda.map(|h| h.lpib()).unwrap_or(0),
            repeat: self.repeat,
        }
    }

    /// Hardware summary for the diagnostics overlay.
    pub fn hda_summary(&self) -> Vec<((u16, u16), Vec<u32>)> {
        self.hdas.iter().map(|h| (h.vendor_device, h.codec_ids())).collect()
    }
}
