//! USB 1.1 UHCI host controller (the companion of EHCI on Intel ICH chipsets).
//! Used for full/low-speed devices such as the odd USB 1.1 flash drive.

use alloc::vec::Vec;
use x86_64::instructions::port::Port;
use crate::interrupts::{delay_ms, Deadline};
use crate::memory::DmaRegion;
use crate::pci::{Bar, PciDevice};
use crate::println;
use super::msc::{self, UsbControllerRef, UsbStorageDrive};

const PID_SETUP: u32 = 0x2D;
const PID_IN: u32 = 0x69;
const PID_OUT: u32 = 0xE1;

const TD_TERMINATE: u32 = 0x01;
const TD_DEPTH_FIRST: u32 = 0x04;
const TD_ACTLEN_MASK: u32 = 0x7FF;
const TD_NAK: u32 = 1 << 19;
const TD_ACTIVE: u32 = 1 << 23;
const TD_LOW_SPEED: u32 = 1 << 26;
const TD_ERR_CNT_3: u32 = 3 << 27;
const TD_SPD: u32 = 1 << 29;
/// Stalled | data buffer error | babble | CRC/timeout | bitstuff
const TD_ERROR_MASK: u32 = (1 << 22) | (1 << 21) | (1 << 20) | (1 << 18) | (1 << 17);

// DMA layout: page 0 = frame list, page 1 = QHs + TDs, pages 2-3 = bounce buffer.
const FRAME_LIST: usize = 0;
const QH_CTRL: usize = 4096;
const QH_BULK: usize = 4096 + 16;
const TD_POOL: usize = 4096 + 64;
const MAX_TDS: usize = (4096 - 64) / 32;
const BOUNCE: usize = 8192;
const BOUNCE_SIZE: usize = 8192;
/// Largest single bulk transfer (64 full-speed packets).
const MAX_BULK: usize = 4096;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct UhciQh {
    head_link_ptr: u32,
    element_link_ptr: u32,
    _reserved: [u32; 2],
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct UhciTd {
    link_ptr: u32,
    ctrl_status: u32,
    token: u32,
    buffer_ptr: u32,
    _reserved: [u32; 4],
}

pub struct UhciController {
    pub io_base: u16,
    dma: DmaRegion,
    bulk_toggle_in: u32,
    bulk_toggle_out: u32,
}

unsafe impl Send for UhciController {}
unsafe impl Sync for UhciController {}

impl UhciController {
    fn td(&self, idx: usize) -> &'static mut UhciTd {
        unsafe { &mut *self.dma.ptr_at::<UhciTd>(TD_POOL + idx * 32) }
    }

    fn td_phys(&self, idx: usize) -> u32 {
        self.dma.phys_at(TD_POOL + idx * 32) as u32
    }

    fn qh(&self, off: usize) -> &'static mut UhciQh {
        unsafe { &mut *self.dma.ptr_at::<UhciQh>(off) }
    }

    fn bounce(&self) -> &'static mut [u8] {
        &mut self.dma.as_slice()[BOUNCE..BOUNCE + BOUNCE_SIZE]
    }

    fn port_reg(&self, port: usize) -> Port<u16> {
        Port::new(self.io_base + 0x10 + port as u16 * 2)
    }

    pub unsafe fn new(pci_dev: &PciDevice) -> Result<Self, &'static str> {
        let io_base = match pci_dev.bars[4] {
            Bar::Io { port } => port,
            _ => return Err("missing I/O BAR4"),
        };

        unsafe {
            pci_dev.enable_bus_mastering();
            let p1 = Port::<u16>::new(io_base + 0x10).read();
            let p2 = Port::<u16>::new(io_base + 0x12).read();
            if (p1 & 0x01) == 0 && (p2 & 0x01) == 0 {
                return Err("no devices attached");
            }
            // Disable BIOS legacy keyboard/mouse emulation (SMM traps), clear trap status.
            pci_dev.write_u16(0xC0, 0x8F00);
        }

        let dma = crate::memory::alloc_dma(4)?;
        let ctrl = UhciController { io_base, dma, bulk_toggle_in: 0, bulk_toggle_out: 0 };

        let mut cmd = Port::<u16>::new(io_base);
        let mut sts = Port::<u16>::new(io_base + 0x02);
        unsafe {
            cmd.write(0x0000);
            delay_ms(5);
            cmd.write(0x0002); // HCRESET
            if !crate::interrupts::wait_until(100, || cmd.read() & 0x0002 == 0) {
                return Err("HCRESET timed out");
            }
            Port::<u16>::new(io_base + 0x04).write(0x0000); // no interrupts
            sts.write(0x003F);
        }

        let qh_ctrl_phys = dma.phys_at(QH_CTRL) as u32;
        let qh_bulk_phys = dma.phys_at(QH_BULK) as u32;
        let qh_ctrl = ctrl.qh(QH_CTRL);
        qh_ctrl.head_link_ptr = qh_bulk_phys | 0x02;
        qh_ctrl.element_link_ptr = TD_TERMINATE;
        let qh_bulk = ctrl.qh(QH_BULK);
        qh_bulk.head_link_ptr = TD_TERMINATE;
        qh_bulk.element_link_ptr = TD_TERMINATE;

        let frame_list = unsafe { core::slice::from_raw_parts_mut(dma.ptr_at::<u32>(FRAME_LIST), 1024) };
        for entry in frame_list.iter_mut() {
            *entry = qh_ctrl_phys | 0x02;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        unsafe {
            Port::<u32>::new(io_base + 0x08).write(dma.phys as u32);
            Port::<u16>::new(io_base + 0x06).write(0);
            Port::<u8>::new(io_base + 0x0C).write(0x40);
            cmd.write(0x00C1); // run, configured, 64-byte max packet
            if !crate::interrupts::wait_until(100, || sts.read() & (1 << 5) == 0) {
                return Err("controller failed to start");
            }
        }
        Ok(ctrl)
    }

    pub fn reset_port(&mut self, port: usize) -> bool {
        let mut reg = self.port_reg(port);
        unsafe {
            let sc = reg.read();
            if (sc & 0x01) == 0 {
                return false;
            }
            reg.write(sc | (1 << 9));
            delay_ms(60);
            let sc = reg.read();
            reg.write(sc & !(1 << 9));
            delay_ms(20);
            for _ in 0..10 {
                let sc = reg.read();
                reg.write((sc | (1 << 2) | (1 << 3) | (1 << 1)) & !(1 << 9));
                delay_ms(10);
                if reg.read() & (1 << 2) != 0 {
                    return true;
                }
            }
            false
        }
    }

    /// Link TDs `0..count` into a queue head and wait for them to finish.
    /// A short IN packet (with SPD set) ends the transfer early. Returns bytes moved by IN TDs.
    fn run_tds(&mut self, qh_off: usize, count: usize, timeout_ms: u64, abort_on_nak: bool) -> Result<usize, &'static str> {
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.qh(qh_off).element_link_ptr = self.td_phys(0);

        let deadline = Deadline::after_ms(timeout_ms);
        let result = 'poll: loop {
            let mut moved = 0usize;
            for i in 0..count {
                let td = self.td(i);
                let cs = unsafe { core::ptr::read_volatile(&td.ctrl_status) };
                // Error bits only count once the controller has retired the TD; while it is
                // still active the hardware is retrying.
                if cs & TD_ACTIVE == 0 && cs & TD_ERROR_MASK != 0 {
                    break 'poll Err("USB 1.1 transfer error");
                }
                if cs & TD_ACTIVE != 0 {
                    if abort_on_nak && cs & TD_NAK != 0 {
                        break 'poll Err("NAK");
                    }
                    break;
                }
                let actual = ((cs & TD_ACTLEN_MASK) + 1) & TD_ACTLEN_MASK;
                let expected = (((td.token >> 21) & TD_ACTLEN_MASK) + 1) & TD_ACTLEN_MASK;
                if td.token & 0xFF == PID_IN {
                    moved += actual as usize;
                }
                if i == count - 1 || (td.token & 0xFF == PID_IN && actual < expected) {
                    break 'poll Ok(moved);
                }
            }
            if deadline.expired() {
                break Err("USB 1.1 transfer timeout");
            }
            core::hint::spin_loop();
        };

        self.qh(qh_off).element_link_ptr = TD_TERMINATE;
        for i in 0..count {
            self.td(i).ctrl_status &= !TD_ACTIVE;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // Let the controller finish the current frame before the TDs are reused.
        delay_ms(1);
        result
    }

    fn fill_td(&self, idx: usize, last: bool, status: u32, pid: u32, dev_addr: u8, ep: u8, toggle: u32, len: usize, buf: u32) {
        let link = if last { TD_TERMINATE } else { self.td_phys(idx + 1) | TD_DEPTH_FIRST };
        let td = self.td(idx);
        td.link_ptr = link;
        td.ctrl_status = TD_ACTIVE | TD_ERR_CNT_3 | status;
        td.token = pid
            | ((dev_addr as u32 & 0x7F) << 8)
            | (((ep as u32) & 0x0F) << 15)
            | (toggle << 19)
            | (((len as u32).wrapping_sub(1) & TD_ACTLEN_MASK) << 21);
        td.buffer_ptr = buf;
    }

    pub fn control_transfer(&mut self, dev_addr: u8, low_speed: bool, ep0_mps: usize, req_type: u8, req: u8, value: u16, index: u16, data: &mut [u8]) -> Result<usize, &'static str> {
        let is_in = (req_type & 0x80) != 0;
        let data_len = data.len();
        let mps = ep0_mps.clamp(8, 64);
        if 8 + data_len > BOUNCE_SIZE || data_len / mps + 3 > MAX_TDS {
            return Err("control transfer too large");
        }

        let setup_phys = self.dma.phys_at(BOUNCE) as u32;
        let bounce = self.bounce();
        bounce[0] = req_type;
        bounce[1] = req;
        bounce[2..4].copy_from_slice(&value.to_le_bytes());
        bounce[4..6].copy_from_slice(&index.to_le_bytes());
        bounce[6..8].copy_from_slice(&(data_len as u16).to_le_bytes());
        if !is_in && data_len > 0 {
            bounce[8..8 + data_len].copy_from_slice(data);
        }

        let ls = if low_speed { TD_LOW_SPEED } else { 0 };
        self.fill_td(0, false, ls, PID_SETUP, dev_addr, 0, 0, 8, setup_phys);
        let mut idx = 1;
        let mut off = 0;
        let mut toggle = 1;
        let pid = if is_in { PID_IN } else { PID_OUT };
        while off < data_len {
            let chunk = (data_len - off).min(mps);
            let spd = if is_in { TD_SPD } else { 0 };
            self.fill_td(idx, false, ls | spd, pid, dev_addr, 0, toggle, chunk, setup_phys + 8 + off as u32);
            off += chunk;
            toggle ^= 1;
            idx += 1;
        }
        let status_pid = if is_in { PID_OUT } else { PID_IN };
        self.fill_td(idx, true, ls, status_pid, dev_addr, 0, 1, 0, 0);

        // Short data-in packets end the data stage early; the status stage is then skipped,
        // which devices tolerate for descriptor reads.
        self.run_tds(QH_CTRL, idx + 1, 500, false)?;

        if is_in && data_len > 0 {
            data.copy_from_slice(&self.bounce()[8..8 + data_len]);
        }
        Ok(data_len)
    }

    pub fn bulk_transfer(&mut self, dev_addr: u8, ep: u8, is_in: bool, max_packet: u16, data: &mut [u8]) -> Result<usize, &'static str> {
        let len = data.len();
        if len == 0 {
            return Ok(0);
        }
        if len > MAX_BULK {
            return Err("USB 1.1 bulk transfer too large");
        }
        let mps = (max_packet as usize).clamp(8, 64);
        let packets = len.div_ceil(mps);
        let bounce_phys = self.dma.phys_at(BOUNCE) as u32;
        if !is_in {
            self.bounce()[..len].copy_from_slice(data);
        }

        let pid = if is_in { PID_IN } else { PID_OUT };
        let start_toggle = if is_in { self.bulk_toggle_in } else { self.bulk_toggle_out };
        let mut toggle = start_toggle;
        for p in 0..packets {
            let off = p * mps;
            let chunk = (len - off).min(mps);
            let spd = if is_in { TD_SPD } else { 0 };
            self.fill_td(p, p + 1 == packets, spd, pid, dev_addr, ep, toggle, chunk, bounce_phys + off as u32);
            toggle ^= 1;
        }

        let moved = self.run_tds(QH_BULK, packets, 3000, false)?;
        let done = if is_in { moved } else { len };
        // Advance the toggle by the number of packets that actually went through.
        let sent_packets = if is_in { done.div_ceil(mps).max(1) } else { packets };
        let new_toggle = start_toggle ^ (sent_packets as u32 & 1);
        if is_in {
            self.bulk_toggle_in = new_toggle;
            data[..done].copy_from_slice(&self.bounce()[..done]);
        } else {
            self.bulk_toggle_out = new_toggle;
        }
        Ok(done)
    }

    pub fn clear_endpoint_halt(&mut self, dev_addr: u8, ep_addr: u8) {
        let _ = self.control_transfer(dev_addr, false, 64, 0x02, 0x01, 0, ep_addr as u16, &mut []);
        if (ep_addr & 0x80) != 0 {
            self.bulk_toggle_in = 0;
        } else {
            self.bulk_toggle_out = 0;
        }
    }

    pub fn read_sectors_msc(&mut self, drive: &UsbStorageDrive, lba: u64, buffer: &mut [u8]) -> Result<(), &'static str> {
        let mut last_err = "USB 1.1 read failed";
        for _attempt in 0..2 {
            let cdb = msc::read10_cdb(lba, (buffer.len() / 512) as u16);
            match msc::bot_command(self, drive.dev_addr, drive.ep_in, drive.ep_out, &cdb, msc::DataDir::In(buffer)) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = e;
                    self.clear_endpoint_halt(drive.dev_addr, drive.ep_in | 0x80);
                    self.clear_endpoint_halt(drive.dev_addr, drive.ep_out);
                }
            }
        }
        Err(last_err)
    }

    /// Enumerate the two root ports and return mass storage drives.
    pub fn scan_devices(&mut self, controller_idx: usize, next_addr: &mut u8, name: &str) -> Vec<UsbStorageDrive> {
        let mut drives = Vec::new();
        for port in 0..2 {
            let sc = unsafe { self.port_reg(port).read() };
            if (sc & 0x01) == 0 {
                continue;
            }
            let low_speed = (sc & (1 << 8)) != 0;
            if !self.reset_port(port) {
                println!("       UHCI {} port {}: reset failed", name, port + 1);
                continue;
            }
            delay_ms(50);

            let mut desc = [0u8; 18];
            let mut ok = false;
            for _ in 0..5 {
                if self.control_transfer(0, low_speed, 8, 0x80, 0x06, 0x0100, 0, &mut desc[..8]).is_ok() {
                    ok = true;
                    break;
                }
                delay_ms(30);
            }
            if !ok {
                println!("       UHCI {} port {}: no device descriptor", name, port + 1);
                continue;
            }
            let mps = (desc[7] as usize).clamp(8, 64);

            let dev_addr = *next_addr;
            *next_addr += 1;
            if self.control_transfer(0, low_speed, mps, 0x00, 0x05, dev_addr as u16, 0, &mut []).is_err() {
                println!("       UHCI {} port {}: SET_ADDRESS failed", name, port + 1);
                continue;
            }
            delay_ms(20);

            let mut conf_head = [0u8; 9];
            if self.control_transfer(dev_addr, low_speed, mps, 0x80, 0x06, 0x0200, 0, &mut conf_head).is_err() {
                continue;
            }
            let total = (u16::from_le_bytes([conf_head[2], conf_head[3]]) as usize).clamp(9, 256);
            let mut conf = alloc::vec![0u8; total];
            if self.control_transfer(dev_addr, low_speed, mps, 0x80, 0x06, 0x0200, 0, &mut conf).is_err() {
                continue;
            }
            let config_value = conf[5] as u16;

            let Some(ep) = msc::parse_msc_endpoints(&conf) else {
                println!("       UHCI {} port {}: unsupported device", name, port + 1);
                continue;
            };
            if self.control_transfer(dev_addr, low_speed, mps, 0x00, 0x09, config_value, 0, &mut []).is_err() {
                continue;
            }
            delay_ms(20);
            self.bulk_toggle_in = 0;
            self.bulk_toggle_out = 0;

            let mut drive = UsbStorageDrive {
                name: alloc::format!("usb1.{}", port + 1),
                model: alloc::string::String::new(),
                capacity_mb: 0,
                sectors: 0,
                sector_size: 512,
                controller_ref: UsbControllerRef::Uhci(controller_idx),
                dev_addr,
                ep_in: ep.ep_in,
                ep_out: ep.ep_out,
                max_packet: ep.max_packet,
                single_sector: false,
            };
            msc::identify(self, &mut drive);
            println!("       UHCI {} port {}: {} ({} MB)", name, port + 1, drive.model, drive.capacity_mb);
            drives.push(drive);
        }
        drives
    }
}

impl msc::BulkTransport for UhciController {
    fn bulk(&mut self, dev_addr: u8, ep: u8, is_in: bool, max_packet: u16, data: &mut [u8]) -> Result<usize, &'static str> {
        self.bulk_transfer(dev_addr, ep, is_in, max_packet, data)
    }

    fn clear_halt(&mut self, dev_addr: u8, ep_addr: u8) {
        self.clear_endpoint_halt(dev_addr, ep_addr);
    }

    fn max_transfer(&self) -> usize {
        MAX_BULK
    }
}
