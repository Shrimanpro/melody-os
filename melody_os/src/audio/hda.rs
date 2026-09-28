use volatile::Volatile;
use x86_64::VirtAddr;

use crate::pci::{Bar, PciDevice};
use crate::audio::wav::WavAudio;

pub const HDA_GCTL_CRST: u32 = 1 << 0; // Controller Reset
pub const HDA_SDCTL_SRST: u16 = 1 << 0; // Stream Reset
pub const HDA_SDCTL_SRUN: u16 = 1 << 1; // Stream Run
pub const HDA_SDCTL_IOCE: u16 = 1 << 2; // Interrupt on Completion Enable

pub const HDA_ICS_ICB: u16 = 1 << 0; // Immediate Command Busy
pub const HDA_ICS_IRV: u16 = 1 << 1; // Immediate Result Valid

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct HdaBdle {
    pub addr_low: u32,
    pub addr_high: u32,
    pub length: u32,
    pub flags: u32, // Bit 0: Interrupt on Completion
}

#[repr(C)]
pub struct HdaStreamRegs {
    pub ctl_low: Volatile<u16>,
    pub ctl_high: Volatile<u8>,
    pub sts: Volatile<u8>,
    pub lpib: Volatile<u32>,
    pub cbl: Volatile<u32>,
    pub lvi: Volatile<u16>,
    pub fifow: Volatile<u16>,
    pub fifos: Volatile<u16>,
    pub fmt: Volatile<u16>,
    pub bdlpl: Volatile<u32>,
    pub bdlpu: Volatile<u32>,
}

#[allow(dead_code)]
pub struct HdaController {
    base_virt: *mut u8,
    phys_mem_offset: VirtAddr,
    output_stream_offset: usize,
    bdl_phys: u64,
    bdl_virt: *mut HdaBdle,
    buffer_phys: [u64; 2],
    buffer_virt: [*mut u8; 2],
    pub buffer_size: usize,
    pub is_playing: bool,
}

impl HdaController {
    pub unsafe fn new(pci_dev: &PciDevice, phys_mem_offset: VirtAddr) -> Result<Self, &'static str> {
        // Enable Bus Mastering and Memory Space on the HDA device
        unsafe { pci_dev.enable_bus_mastering() };

        let hdbar_phys = match pci_dev.bars[0] {
            Bar::Memory32 { address, .. } => address as u64,
            Bar::Memory64 { address, .. } => address,
            _ => return Err("HDA BAR0 is not a memory BAR"),
        };

        let base_virt = (phys_mem_offset + hdbar_phys).as_mut_ptr::<u8>();

        // 1. Reset controller
        unsafe {
            let gctl_ptr = base_virt.add(0x08) as *mut u32;
            let mut gctl = core::ptr::read_volatile(gctl_ptr);

            // Clear CRST to reset
            gctl &= !HDA_GCTL_CRST;
            core::ptr::write_volatile(gctl_ptr, gctl);

            let mut timeout = 0;
            while (core::ptr::read_volatile(gctl_ptr) & HDA_GCTL_CRST) != 0 && timeout < 100_000 {
                timeout += 1;
            }

            // Set CRST to exit reset
            gctl |= HDA_GCTL_CRST;
            core::ptr::write_volatile(gctl_ptr, gctl);

            timeout = 0;
            while (core::ptr::read_volatile(gctl_ptr) & HDA_GCTL_CRST) == 0 && timeout < 100_000 {
                timeout += 1;
            }
        }

        // Wait a moment for codecs to self-initialize and report state
        let statests = unsafe { core::ptr::read_volatile(base_virt.add(0x0E) as *const u16) };
        crate::println!("HDA: Codecs detected (STATESTS): 0x{:04x}", statests);

        // 2. Discover stream counts
        let gcap = unsafe { core::ptr::read_volatile(base_virt as *const u16) };
        let iss = ((gcap >> 8) & 0x0F) as usize; // Number of input streams
        let oss = ((gcap >> 12) & 0x0F) as usize; // Number of output streams
        crate::println!("HDA: Streams: {} input, {} output", iss, oss);

        if oss == 0 {
            return Err("HDA controller has no output streams");
        }

        // Output streams begin after input streams: offset = 0x80 + iss * 0x20
        let output_stream_offset = 0x80 + iss * 0x20;

        // 3. Allocate DMA buffers:
        // BDL (Buffer Descriptor List): 2 entries of 16 bytes = 32 bytes (128-byte aligned)
        let phys_offset = phys_mem_offset.as_u64();
        let bdl_buf = alloc::vec![0u8; 128].leak();
        let bdl_virt = bdl_buf.as_mut_ptr() as *mut HdaBdle;
        let bdl_phys = if (bdl_virt as u64) >= phys_offset { (bdl_virt as u64) - phys_offset } else { bdl_virt as u64 };

        // Double-buffered DMA ring: 2 x 32 KiB buffers
        let buf_size = 32 * 1024;
        let buf0 = alloc::vec![0u8; buf_size].leak();
        let buf1 = alloc::vec![0u8; buf_size].leak();

        let buf0_virt = buf0.as_mut_ptr();
        let buf1_virt = buf1.as_mut_ptr();

        let buf0_phys = if (buf0_virt as u64) >= phys_offset { (buf0_virt as u64) - phys_offset } else { buf0_virt as u64 };
        let buf1_phys = if (buf1_virt as u64) >= phys_offset { (buf1_virt as u64) - phys_offset } else { buf1_virt as u64 };

        unsafe {
            let bdles = core::slice::from_raw_parts_mut(bdl_virt, 2);
            bdles[0] = HdaBdle {
                addr_low: (buf0_phys & 0xFFFFFFFF) as u32,
                addr_high: ((buf0_phys >> 32) & 0xFFFFFFFF) as u32,
                length: buf_size as u32,
                flags: 1, // Interrupt on completion
            };
            bdles[1] = HdaBdle {
                addr_low: (buf1_phys & 0xFFFFFFFF) as u32,
                addr_high: ((buf1_phys >> 32) & 0xFFFFFFFF) as u32,
                length: buf_size as u32,
                flags: 1, // Interrupt on completion
            };
        }

        let mut controller = HdaController {
            base_virt,
            phys_mem_offset,
            output_stream_offset,
            bdl_phys,
            bdl_virt,
            buffer_phys: [buf0_phys, buf1_phys],
            buffer_virt: [buf0_virt, buf1_virt],
            buffer_size: buf_size,
            is_playing: false,
        };

        // 4. Configure S/PDIF and Line Out codecs
        controller.configure_codecs();

        Ok(controller)
    }

    /// Send an Immediate Command to the codec and wait for response
    pub fn send_verb(&self, codec: u8, nid: u8, verb: u32, payload: u32) -> u32 {
        let command = ((codec as u32 & 0x0F) << 28)
            | ((nid as u32 & 0xFF) << 20)
            | ((verb & 0xFFF) << 8)
            | (payload & 0xFF);

        unsafe {
            let icw = self.base_virt.add(0x60) as *mut u32;
            let irr = self.base_virt.add(0x64) as *const u32;
            let ics = self.base_virt.add(0x68) as *mut u16;

            // Wait until Immediate Command is not busy
            let mut timeout = 0;
            while (core::ptr::read_volatile(ics) & HDA_ICS_ICB) != 0 && timeout < 100_000 {
                timeout += 1;
            }

            // Write command
            core::ptr::write_volatile(icw, command);

            // Trigger command (set ICB and clear IRV)
            core::ptr::write_volatile(ics, HDA_ICS_ICB | HDA_ICS_IRV);

            // Wait for response (IRV set, ICB clear)
            timeout = 0;
            while ((core::ptr::read_volatile(ics) & HDA_ICS_ICB) != 0
                || (core::ptr::read_volatile(ics) & HDA_ICS_IRV) == 0)
                && timeout < 100_000
            {
                timeout += 1;
            }

            core::ptr::read_volatile(irr)
        }
    }

    /// Configure codec widgets for Line Out and digital S/PDIF optical output
    pub fn configure_codecs(&mut self) {
        crate::println!("HDA: Initializing audio codec and activating S/PDIF & Line Out...");

        for nid in 2..=16 {
            // Unmute amplifiers on audio nodes
            self.send_verb(0, nid, 0x3, 0xB07F); // Output amp: unmute, max volume
            self.send_verb(0, nid, 0x705, 0x00); // Power state: D0 (active)
            self.send_verb(0, nid, 0x707, 0x45); // Pin widget control: enable output + vref
            self.send_verb(0, nid, 0x70D, 0x01); // S/PDIF digital output enable
            self.send_verb(0, nid, 0x706, 0x10); // Assign to Stream ID 1, channel 0
        }
    }

    /// Play a WAV audio file directly through Intel HDA
    pub fn play_wav(&mut self, wav: &WavAudio) -> Result<(), &'static str> {
        let format = wav.hda_format();
        crate::println!(
            "HDA: Playing WAV track (Channels: {}, Rate: {}Hz, Bits: {}, Format: 0x{:04x})",
            wav.header.num_channels,
            wav.header.sample_rate,
            wav.header.bits_per_sample,
            format
        );
        self.play_stream(wav.samples(), format)
    }

    /// Play raw PCM samples by streaming them into the DMA ring buffer
    pub fn play_stream(&mut self, pcm_samples: &[u8], format: u16) -> Result<(), &'static str> {
        let stream = unsafe { &mut *(self.base_virt.add(self.output_stream_offset) as *mut HdaStreamRegs) };

        // 1. Reset and stop stream
        stream.ctl_low.write(0);
        stream.ctl_high.write(0);

        // 2. Configure BDL Base Address
        stream.bdlpl.write((self.bdl_phys & 0xFFFFFFFF) as u32);
        stream.bdlpu.write(((self.bdl_phys >> 32) & 0xFFFFFFFF) as u32);

        // Cyclic Buffer Length: total bytes in all BDL entries (2 * buffer_size)
        stream.cbl.write((self.buffer_size * 2) as u32);

        // Last Valid Index: 1 (2 BDL entries)
        stream.lvi.write(1);

        // 3. Set Stream Format
        stream.fmt.write(format);

        // Set Stream ID to 1 in bits 7:4 of ctl_high
        stream.ctl_high.write(1 << 4);

        // 4. Fill initial buffers with PCM samples
        let chunk0_size = self.buffer_size.min(pcm_samples.len());
        unsafe {
            core::ptr::copy_nonoverlapping(pcm_samples.as_ptr(), self.buffer_virt[0], chunk0_size);
            if chunk0_size < self.buffer_size {
                core::ptr::write_bytes(self.buffer_virt[0].add(chunk0_size), 0, self.buffer_size - chunk0_size);
            }
        }

        let remaining = if pcm_samples.len() > chunk0_size { &pcm_samples[chunk0_size..] } else { &[] };
        let chunk1_size = self.buffer_size.min(remaining.len());
        unsafe {
            if chunk1_size > 0 {
                core::ptr::copy_nonoverlapping(remaining.as_ptr(), self.buffer_virt[1], chunk1_size);
            }
            if chunk1_size < self.buffer_size {
                core::ptr::write_bytes(self.buffer_virt[1].add(chunk1_size), 0, self.buffer_size - chunk1_size);
            }
        }

        // 5. Start DMA Stream Engine (SRUN = bit 1)
        let ctl = stream.ctl_low.read();
        stream.ctl_low.write(ctl | HDA_SDCTL_SRUN);
        self.is_playing = true;

        crate::println!("HDA: DMA stream engine active. Audio is now playing via S/PDIF / Line Out!");
        Ok(())
    }

    /// Stop audio playback
    pub fn stop(&mut self) {
        let stream = unsafe { &mut *(self.base_virt.add(self.output_stream_offset) as *mut HdaStreamRegs) };
        let ctl = stream.ctl_low.read();
        stream.ctl_low.write(ctl & !HDA_SDCTL_SRUN);
        self.is_playing = false;
        crate::println!("HDA: Audio playback stopped.");
    }
}
