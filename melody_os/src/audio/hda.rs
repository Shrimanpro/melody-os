//! Intel High Definition Audio controller + codec driver.
//!
//! - Codecs are walked properly: every output pin is classified from its configuration
//!   default (analog line-out/headphone/speaker vs. digital S/PDIF/HDMI) and a path back
//!   to a DAC is found through the connection lists. Only the selected output's paths are
//!   enabled.
//! - One output stream plays from a 512 KiB cyclic DMA ring. The driver tracks the hardware
//!   read position (LPIB) and keeps a known amount of data queued ahead of it; when the
//!   player has nothing to give, silence is queued instead so stale audio never replays.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use crate::memory::DmaRegion;
use crate::pci::{Bar, PciDevice};

const RING_PAGES: usize = 128;
pub const RING_BYTES: usize = RING_PAGES * 4096;
const STREAM_TAG: u32 = 1;

// Global registers
const GCAP: usize = 0x00;
const GCTL: usize = 0x08;
const STATESTS: usize = 0x0E;
const INTCTL: usize = 0x20;
const CORBCTL: usize = 0x4C;
const RIRBCTL: usize = 0x5C;
const ICW: usize = 0x60;
const IRR: usize = 0x64;
const ICS: usize = 0x68;

// Stream descriptor registers (offsets from the descriptor base)
const SD_CTL: usize = 0x00;
const SD_LPIB: usize = 0x04;
const SD_CBL: usize = 0x08;
const SD_LVI: usize = 0x0C;
const SD_FMT: usize = 0x12;
const SD_BDPL: usize = 0x18;
const SD_BDPU: usize = 0x1C;

const SD_CTL_SRST: u32 = 1 << 0;
const SD_CTL_RUN: u32 = 1 << 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Analog,
    Digital,
}

impl OutputKind {
    pub fn name(&self) -> &'static str {
        match self {
            OutputKind::Analog => "Analog (3.5mm)",
            OutputKind::Digital => "Digital (S/PDIF/HDMI)",
        }
    }
}

/// PCM format actually programmed into the stream and converters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    pub rate: u32,
    /// 16, or 24 (sent in a 32-bit container, MSB aligned)
    pub bits: u8,
}

impl PcmFormat {
    pub fn frame_bytes(&self) -> usize {
        if self.bits == 16 { 4 } else { 8 }
    }

    /// Encode as the 16-bit HDA stream format word (always stereo).
    fn to_hda(&self) -> Option<u16> {
        let (base44, mult, div) = match self.rate {
            8000 => (false, 1, 6),
            11025 => (true, 1, 4),
            16000 => (false, 1, 3),
            22050 => (true, 1, 2),
            32000 => (false, 2, 3),
            44100 => (true, 1, 1),
            48000 => (false, 1, 1),
            88200 => (true, 2, 1),
            96000 => (false, 2, 1),
            176400 => (true, 4, 1),
            192000 => (false, 4, 1),
            _ => return None,
        };
        let bits = match self.bits {
            16 => 0b001,
            24 => 0b011,
            _ => return None,
        };
        Some(((base44 as u16) << 14) | ((mult - 1) << 11) | ((div - 1) << 8) | (bits << 4) | 1)
    }
}

#[derive(Clone)]
struct Widget {
    nid: u8,
    wtype: u8,
    caps: u32,
    conns: Vec<u8>,
    pin_cfg: u32,
    pin_caps: u32,
    out_amp: u32,
    in_amp: u32,
}

/// A route from an output pin back to a DAC: nids[0] is the pin, the last is the DAC.
/// `sel[i]` is the connection index nids[i] uses to reach nids[i + 1].
#[derive(Clone)]
pub struct OutputPath {
    pub kind: OutputKind,
    pub device: &'static str,
    nids: Vec<u8>,
    sel: Vec<u8>,
    headphone: bool,
}

impl OutputPath {
    pub fn pin(&self) -> u8 {
        self.nids[0]
    }
    pub fn dac(&self) -> u8 {
        *self.nids.last().unwrap()
    }
}

struct Codec {
    addr: u8,
    vendor_id: u32,
    afg: u8,
    widgets: Vec<Widget>,
    paths: Vec<OutputPath>,
}

impl Codec {
    fn widget(&self, nid: u8) -> Option<&Widget> {
        self.widgets.iter().find(|w| w.nid == nid)
    }
}

pub struct HdaController {
    base: *mut u8,
    sd: usize,
    bdl: DmaRegion,
    ring: DmaRegion,
    codecs: Vec<Codec>,
    pub vendor_device: (u16, u16),
    active: Option<OutputKind>,
    format: Option<PcmFormat>,
    running: bool,
    // Ring bookkeeping
    write_pos: usize,
    last_lpib: usize,
    ahead: isize,
    /// What is queued ahead of the hardware, oldest first: (bytes, is_silence).
    segments: VecDeque<(usize, bool)>,
    pub underruns: u32,
    volume: u8,
    muted: bool,
}

unsafe impl Send for HdaController {}

impl HdaController {
    fn r16(&self, off: usize) -> u16 { unsafe { core::ptr::read_volatile(self.base.add(off) as *const u16) } }
    fn r32(&self, off: usize) -> u32 { unsafe { core::ptr::read_volatile(self.base.add(off) as *const u32) } }
    fn w8(&self, off: usize, v: u8) { unsafe { core::ptr::write_volatile(self.base.add(off), v) } }
    fn w16(&self, off: usize, v: u16) { unsafe { core::ptr::write_volatile(self.base.add(off) as *mut u16, v) } }
    fn w32(&self, off: usize, v: u32) { unsafe { core::ptr::write_volatile(self.base.add(off) as *mut u32, v) } }

    pub unsafe fn new(pci_dev: &PciDevice) -> Result<Self, &'static str> {
        unsafe {
            pci_dev.enable_bus_mastering();
            if pci_dev.vendor_id == 0x8086 {
                // Traffic class 0 and snooped DMA (ICH/PCH specific registers).
                let tcsel = pci_dev.read_u8(0x44);
                pci_dev.write_u8(0x44, tcsel & !0x07);
                let devc = pci_dev.read_u16(0x78);
                if devc != 0xFFFF {
                    pci_dev.write_u16(0x78, devc & !(1 << 11));
                }
            }
        }

        let bar = match pci_dev.bars[0] {
            Bar::Memory32 { address, .. } => address as u64,
            Bar::Memory64 { address, .. } => address,
            _ => return Err("HDA BAR0 is not a memory BAR"),
        };
        let base = crate::memory::ensure_mmio_mapped(bar, 16384).as_mut_ptr::<u8>();

        let bdl = crate::memory::alloc_dma(1)?;
        let ring = crate::memory::alloc_dma(RING_PAGES)?;

        let mut hda = HdaController {
            base,
            sd: 0,
            bdl,
            ring,
            codecs: Vec::new(),
            vendor_device: (pci_dev.vendor_id, pci_dev.device_id),
            active: None,
            format: None,
            running: false,
            write_pos: 0,
            last_lpib: 0,
            ahead: 0,
            segments: VecDeque::new(),
            underruns: 0,
            volume: 80,
            muted: false,
        };

        // Controller reset: CRST 0 -> 1.
        hda.w8(CORBCTL, 0);
        hda.w8(RIRBCTL, 0);
        hda.w32(GCTL, hda.r32(GCTL) & !1);
        if !crate::interrupts::wait_until(100, || hda.r32(GCTL) & 1 == 0) {
            return Err("controller did not enter reset");
        }
        crate::interrupts::delay_us(200);
        hda.w32(GCTL, hda.r32(GCTL) | 1);
        if !crate::interrupts::wait_until(100, || hda.r32(GCTL) & 1 == 1) {
            return Err("controller did not leave reset");
        }
        hda.w32(INTCTL, 0);
        // Codecs need up to 521 us after reset to request a state change.
        crate::interrupts::delay_ms(2);
        let mut statests = 0;
        crate::interrupts::wait_until(50, || {
            statests = hda.r16(STATESTS);
            statests != 0
        });
        if statests == 0 {
            return Err("no codecs responded");
        }

        let gcap = hda.r16(GCAP);
        let iss = ((gcap >> 8) & 0x0F) as usize;
        let oss = ((gcap >> 12) & 0x0F) as usize;
        if oss == 0 {
            return Err("no output streams");
        }
        hda.sd = 0x80 + iss * 0x20;

        // BDL: one entry per 4 KiB page of the ring.
        for i in 0..RING_PAGES {
            let e = hda.bdl.ptr_at::<u32>(i * 16);
            let phys = hda.ring.phys_at(i * 4096);
            unsafe {
                e.write_volatile(phys as u32);
                e.add(1).write_volatile((phys >> 32) as u32);
                e.add(2).write_volatile(4096);
                e.add(3).write_volatile(0);
            }
        }

        for addr in 0..15u8 {
            if statests & (1 << addr) != 0 {
                if let Some(codec) = hda.probe_codec(addr) {
                    hda.codecs.push(codec);
                }
            }
        }
        if hda.paths().next().is_none() {
            return Err("no usable output pins on any codec");
        }
        Ok(hda)
    }

    /// Send one verb over the immediate command interface.
    fn verb(&self, codec: u8, nid: u8, verb: u32, payload: u32) -> Option<u32> {
        let cmd = if verb <= 0xF {
            ((codec as u32) << 28) | ((nid as u32) << 20) | (verb << 16) | (payload & 0xFFFF)
        } else {
            ((codec as u32) << 28) | ((nid as u32) << 20) | (verb << 8) | (payload & 0xFF)
        };
        if !crate::interrupts::wait_until(2, || self.r16(ICS) & 1 == 0) {
            return None;
        }
        self.w16(ICS, 0x2); // clear IRV
        self.w32(ICW, cmd);
        self.w16(ICS, 0x1); // send
        let mut sts = 0;
        if !crate::interrupts::wait_until(2, || {
            sts = self.r16(ICS);
            sts & 0x1 == 0 && sts & 0x2 != 0
        }) {
            return None;
        }
        let resp = self.r32(IRR);
        self.w16(ICS, 0x2);
        Some(resp)
    }

    fn param(&self, codec: u8, nid: u8, p: u32) -> u32 {
        self.verb(codec, nid, 0xF00, p).unwrap_or(0)
    }

    fn read_conn_list(&self, codec: u8, nid: u8) -> Vec<u8> {
        let info = self.param(codec, nid, 0x0E);
        let len = (info & 0x7F) as usize;
        let long = info & 0x80 != 0;
        let per = if long { 2 } else { 4 };
        let mut list = Vec::new();
        let mut prev = 0u8;
        let mut i = 0;
        while i < len && list.len() < 32 {
            let resp = self.verb(codec, nid, 0xF02, i as u32).unwrap_or(0);
            for k in 0..per {
                if i + k >= len {
                    break;
                }
                let (range, val) = if long {
                    let e = (resp >> (16 * k)) & 0xFFFF;
                    (e & 0x8000 != 0, (e & 0x7FFF) as u8)
                } else {
                    let e = (resp >> (8 * k)) & 0xFF;
                    (e & 0x80 != 0, (e & 0x7F) as u8)
                };
                if range && prev != 0 && val > prev {
                    for n in (prev + 1)..=val {
                        list.push(n);
                    }
                } else {
                    list.push(val);
                }
                prev = val;
            }
            i += per;
        }
        list
    }

    fn probe_codec(&self, addr: u8) -> Option<Codec> {
        let vendor_id = self.verb(addr, 0, 0xF00, 0x00)?;
        let nodes = self.param(addr, 0, 0x04);
        let (start, count) = (((nodes >> 16) & 0xFF) as u8, (nodes & 0xFF) as u8);
        let afg = (start..start.saturating_add(count)).find(|&n| self.param(addr, n, 0x05) & 0xFF == 0x01)?;

        // Power up the function group and wait for it.
        self.verb(addr, afg, 0x705, 0x00);
        crate::interrupts::delay_ms(10);

        // Many Dell boards gate the jack amplifier with codec GPIOs: enable and drive them high.
        let gpio_count = self.param(addr, afg, 0x11) & 0xFF;
        if gpio_count > 0 {
            let mask = ((1u32 << gpio_count.min(8)) - 1) & 0xFF;
            self.verb(addr, afg, 0x716, mask);
            self.verb(addr, afg, 0x717, mask);
            self.verb(addr, afg, 0x715, mask);
        }

        let wnodes = self.param(addr, afg, 0x04);
        let (wstart, wcount) = (((wnodes >> 16) & 0xFF) as u8, (wnodes & 0xFF) as u8);
        let afg_out_amp = self.param(addr, afg, 0x12);
        let afg_in_amp = self.param(addr, afg, 0x0D);

        let mut widgets = Vec::new();
        for nid in wstart..wstart.saturating_add(wcount) {
            let caps = self.param(addr, nid, 0x09);
            let wtype = ((caps >> 20) & 0xF) as u8;
            let conns = if caps & (1 << 8) != 0 { self.read_conn_list(addr, nid) } else { Vec::new() };
            let (pin_cfg, pin_caps) = if wtype == 4 {
                (self.verb(addr, nid, 0xF1C, 0).unwrap_or(0), self.param(addr, nid, 0x0C))
            } else {
                (0, 0)
            };
            // Bit 3 of caps: amp parameters override the function group defaults.
            let override_amp = caps & (1 << 3) != 0;
            let out_amp = if caps & (1 << 2) != 0 { if override_amp { self.param(addr, nid, 0x12) } else { afg_out_amp } } else { 0 };
            let in_amp = if caps & (1 << 1) != 0 { if override_amp { self.param(addr, nid, 0x0D) } else { afg_in_amp } } else { 0 };
            if caps & (1 << 10) != 0 {
                self.verb(addr, nid, 0x705, 0x00); // widget power D0
            }
            widgets.push(Widget { nid, wtype, caps, conns, pin_cfg, pin_caps, out_amp, in_amp });
        }

        let mut codec = Codec { addr, vendor_id, afg, widgets, paths: Vec::new() };
        codec.paths = find_output_paths(&codec);
        crate::println!(
            "       codec {} [{:08x}]: {} widgets, outputs: {}",
            addr,
            vendor_id,
            codec.widgets.len(),
            codec.paths.iter().map(|p| p.device).collect::<Vec<_>>().join(", ")
        );
        Some(codec)
    }

    fn paths(&self) -> impl Iterator<Item = &OutputPath> {
        self.codecs.iter().flat_map(|c| c.paths.iter())
    }

    /// Output kinds this controller can drive.
    pub fn available_outputs(&self) -> Vec<OutputKind> {
        let mut v = Vec::new();
        for k in [OutputKind::Analog, OutputKind::Digital] {
            if self.paths().any(|p| p.kind == k) {
                v.push(k);
            }
        }
        v
    }

    pub fn active_output(&self) -> Option<OutputKind> {
        self.active
    }

    /// Human-readable list of pins driven by the active output, e.g. "Headphone 0x14".
    pub fn active_pins(&self) -> alloc::string::String {
        let mut s = alloc::string::String::new();
        if let Some(kind) = self.active {
            for p in self.paths().filter(|p| p.kind == kind) {
                if !s.is_empty() {
                    s.push_str(", ");
                }
                s.push_str(&alloc::format!("{} {:#04x}", p.device, p.pin()));
            }
        }
        s
    }

    pub fn codec_ids(&self) -> Vec<u32> {
        self.codecs.iter().map(|c| c.vendor_id).collect()
    }

    /// Route audio to the given output kind and silence the other one.
    pub fn set_output(&mut self, kind: OutputKind) {
        self.active = Some(kind);
        for ci in 0..self.codecs.len() {
            let addr = self.codecs[ci].addr;
            let paths = self.codecs[ci].paths.clone();
            // Disable everything first so DACs shared between paths end up configured once.
            for p in &paths {
                self.verb(addr, p.pin(), 0x707, 0x00);
                self.verb(addr, p.dac(), 0x706, 0x00);
            }
            for p in paths.iter().filter(|p| p.kind == kind) {
                self.enable_path(ci, p);
            }
        }
    }

    fn enable_path(&self, ci: usize, p: &OutputPath) {
        let codec = &self.codecs[ci];
        let addr = codec.addr;
        let pin = codec.widget(p.pin()).unwrap();
        let pinctl = match p.kind {
            OutputKind::Digital => 0x40,
            OutputKind::Analog if p.headphone && pin.pin_caps & (1 << 3) != 0 => 0xC0,
            OutputKind::Analog => 0x40,
        };
        self.verb(addr, pin.nid, 0x707, pinctl);
        if pin.pin_caps & (1 << 16) != 0 {
            self.verb(addr, pin.nid, 0x70C, 0x02); // EAPD on
        }

        for (i, &nid) in p.nids.iter().enumerate() {
            let w = codec.widget(nid).unwrap();
            if i < p.sel.len() && w.conns.len() > 1 && w.wtype != 2 {
                self.verb(addr, nid, 0x701, p.sel[i] as u32);
            }
            // Unmute this node's input amp on the connection we use (mixers mute the rest).
            if w.in_amp != 0 || w.caps & (1 << 1) != 0 {
                if w.wtype == 2 {
                    for idx in 0..w.conns.len() as u32 {
                        let mute = if i < p.sel.len() && idx == p.sel[i] as u32 { 0 } else { 0x80 };
                        let gain = amp_offset(w.in_amp);
                        self.verb(addr, nid, 0x3, 0x7000 | (idx << 8) | mute | gain);
                    }
                } else if i < p.sel.len() {
                    self.verb(addr, nid, 0x3, 0x7000 | ((p.sel[i] as u32) << 8) | amp_offset(w.in_amp));
                }
            }
        }

        let dac = codec.widget(p.dac()).unwrap();
        self.verb(addr, dac.nid, 0x706, STREAM_TAG << 4);
        if dac.caps & (1 << 9) != 0 {
            self.verb(addr, dac.nid, 0x70D, 0x01); // digital converter enable
        }
        if let Some(fmt) = self.format.and_then(|f| f.to_hda()) {
            self.verb(addr, dac.nid, 0x2, fmt as u32);
        }
        if p.device == "HDMI" {
            self.setup_hdmi(addr, pin.nid, dac.nid);
        }
    }

    /// HDMI needs more than a digital pin: the converter has to be told it carries two
    /// channels, the pin's channel slots mapped, and an audio InfoFrame sent so the TV
    /// knows what the audio packets contain (as Linux patch_hdmi does).
    fn setup_hdmi(&self, addr: u8, pin: u8, cvt: u8) {
        self.verb(addr, cvt, 0x72D, 1); // converter channel count: 2 - 1
        // Stereo slot map: slot 0 <- ch 0, slot 1 <- ch 1, other slots unused (0xf).
        for slot in 0..8u32 {
            let ch = if slot < 2 { slot } else { 0xF };
            self.verb(addr, pin, 0x734, (ch << 4) | slot);
        }
        // Audio InfoFrame: type 0x84, version 1, length 10, 2 channels, everything else
        // "refer to stream header".
        let mut frame = [0x84u8, 0x01, 0x0A, 0, 0x01, 0, 0, 0, 0];
        let sum = frame.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        frame[3] = 0u8.wrapping_sub(sum);
        self.verb(addr, pin, 0x730, 0); // DIP index: packet 0 (audio InfoFrame), byte 0
        self.verb(addr, pin, 0x732, 0x00); // stop sending while it's rewritten
        self.verb(addr, pin, 0x730, 0);
        for b in frame {
            self.verb(addr, pin, 0x731, b as u32);
        }
        self.verb(addr, pin, 0x730, 0);
        self.verb(addr, pin, 0x732, 0xC0); // send continuously (best effort)
    }

    fn sd_r32(&self, off: usize) -> u32 { self.r32(self.sd + off) }
    fn sd_w32(&self, off: usize, v: u32) { self.w32(self.sd + off, v) }

    /// Stop the stream engine and reset it.
    fn reset_stream(&mut self) {
        let ctl = self.sd_r32(SD_CTL);
        self.sd_w32(SD_CTL, ctl & !SD_CTL_RUN);
        crate::interrupts::wait_until(10, || self.sd_r32(SD_CTL) & SD_CTL_RUN == 0);
        self.sd_w32(SD_CTL, SD_CTL_SRST);
        crate::interrupts::wait_until(10, || self.sd_r32(SD_CTL) & SD_CTL_SRST != 0);
        self.sd_w32(SD_CTL, 0);
        crate::interrupts::wait_until(10, || self.sd_r32(SD_CTL) & SD_CTL_SRST == 0);
        self.w8(self.sd + 0x03, 0x1C); // clear status bits
        self.running = false;
    }

    /// Program a new PCM format and empty the ring. Playback starts with `start()`.
    pub fn prepare(&mut self, fmt: PcmFormat) -> Result<(), &'static str> {
        let word = fmt.to_hda().ok_or("unsupported PCM format")?;
        self.reset_stream();
        self.format = Some(fmt);

        // Converters get the same format word as the stream descriptor.
        if let Some(kind) = self.active {
            for codec in &self.codecs {
                for p in codec.paths.iter().filter(|p| p.kind == kind) {
                    self.verb(codec.addr, p.dac(), 0x2, word as u32);
                    self.verb(codec.addr, p.dac(), 0x706, STREAM_TAG << 4);
                }
            }
        }

        self.ring.as_slice().fill(0);
        flush_cache(self.ring.virt, RING_BYTES);
        flush_cache(self.bdl.virt, RING_PAGES * 16);

        self.sd_w32(SD_BDPL, self.bdl.phys as u32);
        self.sd_w32(SD_BDPU, (self.bdl.phys >> 32) as u32);
        self.sd_w32(SD_CBL, RING_BYTES as u32);
        self.w16(self.sd + SD_LVI, (RING_PAGES - 1) as u16);
        self.w16(self.sd + SD_FMT, word);
        self.sd_w32(SD_CTL, STREAM_TAG << 20);

        self.write_pos = 0;
        self.last_lpib = 0;
        self.ahead = 0;
        self.segments.clear();
        Ok(())
    }

    pub fn format(&self) -> Option<PcmFormat> {
        self.format
    }

    pub fn start(&mut self) {
        if self.format.is_none() {
            return;
        }
        self.last_lpib = self.sd_r32(SD_LPIB) as usize % RING_BYTES;
        let ctl = self.sd_r32(SD_CTL);
        self.sd_w32(SD_CTL, ctl | SD_CTL_RUN);
        self.running = true;
    }

    pub fn pause(&mut self) {
        let ctl = self.sd_r32(SD_CTL);
        self.sd_w32(SD_CTL, ctl & !SD_CTL_RUN);
        crate::interrupts::wait_until(10, || self.sd_r32(SD_CTL) & SD_CTL_RUN == 0);
        self.running = false;
    }

    pub fn stop(&mut self) {
        self.reset_stream();
        self.format = None;
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn lpib(&self) -> u32 {
        self.sd_r32(SD_LPIB)
    }

    /// Account for what the hardware consumed since the last call.
    pub fn update_position(&mut self) {
        if !self.running {
            return;
        }
        let lpib = self.sd_r32(SD_LPIB) as usize % RING_BYTES;
        let consumed = (lpib + RING_BYTES - self.last_lpib) % RING_BYTES;
        self.last_lpib = lpib;
        self.ahead -= consumed as isize;
        let mut left = consumed;
        while left > 0 {
            let Some(front) = self.segments.front_mut() else { break };
            let take = left.min(front.0);
            front.0 -= take;
            left -= take;
            if front.0 == 0 {
                self.segments.pop_front();
            }
        }
        if self.ahead < 0 {
            // We fell behind the DMA engine: restart writing just ahead of it.
            self.underruns += 1;
            let frame = self.format.map(|f| f.frame_bytes()).unwrap_or(4);
            self.write_pos = (lpib + 4096) / frame * frame % RING_BYTES;
            self.ahead = ((self.write_pos + RING_BYTES - lpib) % RING_BYTES) as isize;
            self.segments.clear();
            self.push_segment(self.ahead as usize, true);
        }
    }

    fn push_segment(&mut self, bytes: usize, silence: bool) {
        match self.segments.back_mut() {
            Some(last) if last.1 == silence => last.0 += bytes,
            _ => self.segments.push_back((bytes, silence)),
        }
    }

    /// Bytes of *real* audio queued ahead of the hardware.
    pub fn queued_audio(&self) -> usize {
        self.segments.iter().filter(|s| !s.1).map(|s| s.0).sum()
    }

    /// How many bytes can be written right now.
    pub fn free_space(&mut self) -> usize {
        self.update_position();
        // Keep one page between the writer and the hardware read pointer.
        (RING_BYTES - 4096).saturating_sub(self.ahead.max(0) as usize)
    }

    /// Copy PCM bytes into the ring; returns how many were taken.
    pub fn write(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.free_space());
        let mut done = 0;
        while done < n {
            let chunk = (n - done).min(RING_BYTES - self.write_pos);
            unsafe {
                let dst = self.ring.virt.add(self.write_pos);
                core::ptr::copy_nonoverlapping(data.as_ptr().add(done), dst, chunk);
                flush_cache(dst, chunk);
            }
            self.write_pos = (self.write_pos + chunk) % RING_BYTES;
            done += chunk;
        }
        self.ahead += n as isize;
        self.push_segment(n, false);
        n
    }

    /// Keep at least `min_bytes` queued by appending silence (used when the decoder or the
    /// USB stick can't keep up, and while draining at the end of a track).
    pub fn pad_silence(&mut self, min_bytes: usize) {
        self.update_position();
        let ahead = self.ahead.max(0) as usize;
        if ahead >= min_bytes {
            return;
        }
        let want = (min_bytes - ahead).min(self.free_space());
        let mut done = 0;
        while done < want {
            let chunk = (want - done).min(RING_BYTES - self.write_pos);
            unsafe {
                let dst = self.ring.virt.add(self.write_pos);
                core::ptr::write_bytes(dst, 0, chunk);
                flush_cache(dst, chunk);
            }
            self.write_pos = (self.write_pos + chunk) % RING_BYTES;
            done += chunk;
        }
        self.ahead += want as isize;
        self.push_segment(want, true);
    }
}

fn amp_offset(amp_caps: u32) -> u32 {
    amp_caps & 0x7F
}

fn flush_cache(ptr: *mut u8, len: usize) {
    let start = ptr as usize & !63;
    let end = ptr as usize + len;
    let mut p = start;
    while p < end {
        unsafe { core::arch::x86_64::_mm_clflush(p as *const u8) };
        p += 64;
    }
    unsafe { core::arch::x86_64::_mm_mfence() };
}

/// Map a pin configuration default to (kind, label, is_headphone), or None if the pin
/// is not a connected output.
fn classify_pin(w: &Widget) -> Option<(OutputKind, &'static str, bool)> {
    let connectivity = w.pin_cfg >> 30;
    let hdmi_capable = w.pin_caps & (1 << 7) != 0;
    if w.pin_caps & (1 << 4) == 0 {
        return None; // not output capable
    }
    if connectivity == 1 {
        // "No physical connection". BIOSes sometimes say this about the graphics chip's
        // HDMI audio pin even though the board has an HDMI connector, so keep those.
        if !hdmi_capable {
            return None;
        }
        return Some((OutputKind::Digital, "HDMI", false));
    }
    let device = (w.pin_cfg >> 20) & 0xF;
    match device {
        0x0 => Some((OutputKind::Analog, "Line-out", false)),
        0x1 => Some((OutputKind::Analog, "Speaker", false)),
        0x2 => Some((OutputKind::Analog, "Headphone", true)),
        0x4 => Some((OutputKind::Digital, "S/PDIF", false)),
        0x5 => Some((OutputKind::Digital, if w.pin_caps & (1 << 7) != 0 { "HDMI" } else { "Digital" }, false)),
        _ => None,
    }
}

fn find_output_paths(codec: &Codec) -> Vec<OutputPath> {
    let mut paths = Vec::new();
    for pin in codec.widgets.iter().filter(|w| w.wtype == 4) {
        let Some((kind, device, headphone)) = classify_pin(pin) else { continue };
        let mut nids = alloc::vec![pin.nid];
        let mut sel = Vec::new();
        if dfs_to_dac(codec, pin.nid, kind == OutputKind::Digital, &mut nids, &mut sel, 0) {
            paths.push(OutputPath { kind, device, nids, sel, headphone });
        }
    }
    paths
}

fn dfs_to_dac(codec: &Codec, nid: u8, digital: bool, nids: &mut Vec<u8>, sel: &mut Vec<u8>, depth: usize) -> bool {
    let Some(w) = codec.widget(nid) else { return false };
    if depth > 0 && w.wtype == 0 {
        let is_digital = w.caps & (1 << 9) != 0;
        // Analog pins need an analog DAC; digital pins take whatever converter feeds them.
        return digital || !is_digital;
    }
    if depth > 5 {
        return false;
    }
    for (idx, &next) in w.conns.iter().enumerate() {
        if nids.contains(&next) {
            continue;
        }
        let Some(nw) = codec.widget(next) else { continue };
        // Only walk through audio outputs, mixers, selectors and power widgets.
        if !matches!(nw.wtype, 0 | 2 | 3) {
            continue;
        }
        nids.push(next);
        sel.push(idx as u8);
        if dfs_to_dac(codec, next, digital, nids, sel, depth + 1) {
            return true;
        }
        nids.pop();
        sel.pop();
    }
    false
}

#[test_case]
fn test_pcm_format_words() {
    assert_eq!(PcmFormat { rate: 48000, bits: 16 }.to_hda(), Some(0x0011));
    assert_eq!(PcmFormat { rate: 44100, bits: 16 }.to_hda(), Some(0x4011));
    assert_eq!(PcmFormat { rate: 96000, bits: 24 }.to_hda(), Some(0x0831));
    assert_eq!(PcmFormat { rate: 88200, bits: 16 }.to_hda(), Some(0x4811));
    assert_eq!(PcmFormat { rate: 12345, bits: 16 }.to_hda(), None);
}
