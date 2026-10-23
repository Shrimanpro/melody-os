//! Just enough Intel G4x (G41/G43/G45/Q45/B43) graphics programming to make HDMI carry
//! audio.
//!
//! In text mode the BIOS drives the HDMI connector as plain DVI, which has no room for
//! sound. To get audio to a TV this switches the port to HDMI mode with audio enabled,
//! sends the AVI InfoFrame HDMI sinks expect, and hands the audio codec an ELD. Register
//! layout and sequence follow Linux i915 (g4x_hdmi.c, intel_hdmi.c, intel_audio.c).

use alloc::string::String;
use spin::Mutex;

use crate::pci::{Bar, PciDevice, PciClass};

// HDMI port control (GEN4_HDMIB/HDMIC, a.k.a. SDVOB/SDVOC).
const HDMI_PORTS: [(usize, u32, char); 2] = [(0x61140, 1, 'B'), (0x61160, 2, 'C')];
const PORT_ENABLE: u32 = 1 << 31;
const ENCODING_MASK: u32 = 3 << 10;
const ENCODING_HDMI: u32 = 2 << 10;
const MODE_SELECT_HDMI: u32 = 1 << 9;
const AUDIO_ENABLE: u32 = 1 << 6;

// Video data island packets (InfoFrames) sent by the graphics side.
const VIDEO_DIP_CTL: usize = 0x61170;
const VIDEO_DIP_DATA: usize = 0x61178;
const DIP_ENABLE: u32 = 1 << 31;
const DIP_PORT_MASK: u32 = 3 << 29;
const DIP_ENABLE_AVI: u32 = 1 << 21;
const DIP_ENABLES: u32 = 0xF << 21;
const DIP_SELECT_MASK: u32 = 3 << 19;
const DIP_FREQ_MASK: u32 = 3 << 16;
const DIP_FREQ_VSYNC: u32 = 1 << 16;
const DIP_DATA_SIZE: usize = 32;

// ELD buffer the HDMI audio codec reads.
const AUD_CNTL_ST: usize = 0x620B4;
const ELD_VALID: u32 = 1 << 14;
const ELD_ADDRESS_MASK: u32 = 0xF << 5;
const ELD_BUFFER_SIZE_SHIFT: u32 = 9;
const HDMIW_HDMIEDID: usize = 0x6210C;

/// Intel G4x graphics device IDs (Eaglelake / Cantiga).
const G4X_IDS: &[u16] = &[0x2E02, 0x2E12, 0x2E22, 0x2E32, 0x2E42, 0x2E92, 0x2A42];

struct Gfx {
    mmio: *mut u8,
    device_id: u16,
}

unsafe impl Send for Gfx {}

static GFX: Mutex<Option<Gfx>> = Mutex::new(None);

impl Gfx {
    fn r(&self, reg: usize) -> u32 {
        unsafe { core::ptr::read_volatile(self.mmio.add(reg) as *const u32) }
    }

    fn w(&self, reg: usize, v: u32) {
        unsafe { core::ptr::write_volatile(self.mmio.add(reg) as *mut u32, v) };
        let _ = self.r(reg); // posting read
    }

    /// The HDMI port currently lighting up a screen, if any.
    fn active_hdmi_port(&self) -> Option<(usize, u32, char)> {
        HDMI_PORTS
            .iter()
            .copied()
            .find(|&(reg, _, _)| {
                let v = self.r(reg);
                v & PORT_ENABLE != 0 && v & ENCODING_MASK == ENCODING_HDMI
            })
    }
}

/// Find Intel G4x integrated graphics. Returns a line for the boot log.
pub fn init(pci_devices: &[PciDevice]) -> Option<String> {
    let dev = pci_devices
        .iter()
        .find(|d| d.class == PciClass::Display && d.vendor_id == 0x8086 && G4X_IDS.contains(&d.device_id))?;
    let bar = match dev.bars[0] {
        Bar::Memory32 { address, .. } => address as u64,
        Bar::Memory64 { address, .. } => address,
        _ => return None,
    };
    let mmio = crate::memory::ensure_mmio_mapped(bar, 512 * 1024).as_mut_ptr::<u8>();
    let gfx = Gfx { mmio, device_id: dev.device_id };
    let line = match gfx.active_hdmi_port() {
        Some((reg, _, name)) => {
            let v = gfx.r(reg);
            let mode = if v & MODE_SELECT_HDMI != 0 { "HDMI" } else { "DVI (no audio yet)" };
            alloc::format!("Intel {:04x}, HDMI port {} in {} mode", gfx.device_id, name, mode)
        }
        None => alloc::format!("Intel {:04x}, no HDMI port active", gfx.device_id),
    };
    *GFX.lock() = Some(gfx);
    Some(line)
}

/// One-line state for the hardware info page.
pub fn status() -> String {
    let guard = GFX.lock();
    let Some(gfx) = guard.as_ref() else { return String::from("no Intel G4x graphics") };
    match gfx.active_hdmi_port() {
        Some((reg, _, name)) => {
            let v = gfx.r(reg);
            let mode = if v & MODE_SELECT_HDMI != 0 { "HDMI" } else { "DVI" };
            let audio = if v & AUDIO_ENABLE != 0 { "audio on" } else { "audio off" };
            let eld = if gfx.r(AUD_CNTL_ST) & ELD_VALID != 0 { "ELD ok" } else { "no ELD" };
            alloc::format!("port {} {} mode, {}, {}", name, mode, audio, eld)
        }
        None => String::from("no HDMI port active"),
    }
}

/// Switch the active HDMI port to HDMI mode with audio. The screen blanks for a moment.
pub fn enable_hdmi_audio() -> Result<(), &'static str> {
    let guard = GFX.lock();
    let gfx = guard.as_ref().ok_or("no Intel G4x graphics")?;
    let (reg, port, _) = gfx.active_hdmi_port().ok_or("no active HDMI port")?;
    let val = gfx.r(reg);
    if val & MODE_SELECT_HDMI != 0 && val & AUDIO_ENABLE != 0 {
        write_eld(gfx);
        return Ok(());
    }

    // The mode bit may only change while the port is off (i915 sets it in "prepare").
    gfx.w(reg, val & !(PORT_ENABLE | AUDIO_ENABLE));
    crate::interrupts::delay_ms(20);
    write_avi_infoframe(gfx, port);
    gfx.w(reg, (val & !(PORT_ENABLE | AUDIO_ENABLE)) | MODE_SELECT_HDMI);
    gfx.w(reg, (val & !AUDIO_ENABLE) | MODE_SELECT_HDMI | PORT_ENABLE | AUDIO_ENABLE);
    crate::interrupts::delay_ms(20);
    write_eld(gfx);
    Ok(())
}

/// Minimal AVI InfoFrame (RGB, no picture information), resent every frame.
fn write_avi_infoframe(gfx: &Gfx, port: u32) {
    let mut ctl = gfx.r(VIDEO_DIP_CTL);
    if ctl & DIP_ENABLE != 0 && ctl & DIP_PORT_MASK != port << 29 {
        return; // the DIP engine belongs to another port
    }
    ctl = (ctl & !(DIP_PORT_MASK | DIP_ENABLES | DIP_SELECT_MASK | DIP_FREQ_MASK | 0xF)) | (port << 29) | DIP_ENABLE | DIP_FREQ_VSYNC;
    gfx.w(VIDEO_DIP_CTL, ctl);

    // Header (type 0x82, version 2, length 13), the hardware's reserved byte, then the
    // checksum and 13 zero data bytes.
    let header = [0x82u8, 0x02, 0x0D];
    let checksum = 0u8.wrapping_sub(header.iter().fold(0u8, |a, &b| a.wrapping_add(b)));
    let mut buf = [0u8; DIP_DATA_SIZE];
    buf[..3].copy_from_slice(&header);
    buf[4] = checksum;
    for chunk in buf.chunks_exact(4) {
        gfx.w(VIDEO_DIP_DATA, u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    gfx.w(VIDEO_DIP_CTL, ctl | DIP_ENABLE_AVI);
}

/// Give the codec a baseline ELD: an HDMI sink taking 2-channel LPCM at 32/44.1/48 kHz,
/// 16/20/24-bit. (A real ELD would come from the screen's EDID.)
fn write_eld(gfx: &Gfx) {
    let mut eld = [0u8; 24];
    eld[0] = 2 << 3; // ELD version 2
    eld[2] = 5; // baseline block length in dwords
    eld[4] = 3 << 5; // CEA-861 version 3, no monitor name
    eld[5] = 1 << 4; // one short audio descriptor, connection type HDMI
    eld[7] = 0x01; // speakers: front left/right
    eld[20] = (1 << 3) | 1; // SAD: LPCM, 2 channels
    eld[21] = 0x07; // 32, 44.1, 48 kHz
    eld[22] = 0x07; // 16, 20, 24 bit

    let ctl = gfx.r(AUD_CNTL_ST);
    gfx.w(AUD_CNTL_ST, ctl & !(ELD_VALID | ELD_ADDRESS_MASK));
    let size = ((gfx.r(AUD_CNTL_ST) >> ELD_BUFFER_SIZE_SHIFT) & 0x1F) as usize;
    for i in 0..size {
        let dw = eld.get(i * 4..i * 4 + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
        gfx.w(HDMIW_HDMIEDID, dw);
    }
    gfx.w(AUD_CNTL_ST, gfx.r(AUD_CNTL_ST) | ELD_VALID);
}
