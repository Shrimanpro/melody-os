//! USB 2.0 EHCI host controller: just enough for high-speed USB mass storage.
//!
//! One asynchronous schedule with three queue heads (control, bulk OUT, bulk IN) and a
//! 16 KiB bounce buffer, all in one physically contiguous DMA region. Transfers are
//! synchronous and polled with hard deadlines so a misbehaving device can never hang boot.

use alloc::string::String;
use alloc::vec::Vec;
use crate::interrupts::{delay_ms, Deadline};
use crate::memory::DmaRegion;
use crate::pci::{Bar, PciDevice};
use crate::println;
use super::msc::{self, UsbControllerRef, UsbStorageDrive};

const PID_OUT: u32 = 0;
const PID_IN: u32 = 1;
const PID_SETUP: u32 = 2;

const QTD_STATUS_ACTIVE: u32 = 1 << 7;
const QTD_STATUS_HALTED: u32 = 1 << 6;
const QTD_CERR_3: u32 = 3 << 10; // error counter, bits 11:10
const QTD_TERMINATE: u32 = 1;

// Layout of the DMA region.
// Queue heads are 68 bytes on 64-bit capable controllers (all Intel ICH ones) and the
// controller writes all 68 back, so they must not be packed 64 bytes apart.
const QH_CTRL: usize = 0;
const QH_BULK_OUT: usize = 128;
const QH_BULK_IN: usize = 256;
const QTD_POOL: usize = 512; // three 64-byte slots
const BOUNCE: usize = 4096; // page aligned
pub const BOUNCE_SIZE: usize = 16384;
const DMA_PAGES: usize = 1 + BOUNCE_SIZE / 4096;

const CONTROL_TIMEOUT_MS: u64 = 500;
const BULK_TIMEOUT_MS: u64 = 3000;

/// EHCI Queue Head (QH) - 32-byte aligned
#[repr(C, align(32))]
#[derive(Clone, Copy)]
pub struct EhciQh {
    pub horizontal_link: u32,
    pub ep_characteristics: u32,
    pub ep_capabilities: u32,
    pub current_qtd: u32,
    // Overlay area:
    pub next_qtd: u32,
    pub alt_next_qtd: u32,
    pub token: u32,
    pub buffer_ptrs: [u32; 5],
    pub ext_buffer_ptrs: [u32; 5],
}

/// EHCI Queue Transfer Descriptor (qTD) - 32-byte aligned
#[repr(C, align(32))]
#[derive(Clone, Copy)]
pub struct EhciQtd {
    pub next_qtd: u32,
    pub alt_next_qtd: u32,
    pub token: u32,
    pub buffer_ptrs: [u32; 5],
    pub ext_buffer_ptrs: [u32; 5],
}

fn set_qtd_buffer_ptrs(qtd: &mut EhciQtd, start_phys: u32) {
    qtd.buffer_ptrs[0] = start_phys;
    let page_base = start_phys & !0xFFF;
    for k in 1..5 {
        qtd.buffer_ptrs[k] = page_base + (k as u32 * 4096);
    }
    qtd.ext_buffer_ptrs = [0; 5];
}

pub struct EhciController {
    pub op_regs: *mut u8,
    pub num_ports: usize,
    pub addr64: bool,
    /// USBSTS and qTD token captured when the last transfer failed, for the boot log.
    pub last_failure: (u32, u32),
    dma: DmaRegion,
}

unsafe impl Send for EhciController {}
unsafe impl Sync for EhciController {}

impl EhciController {
    fn reg(&self, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile(self.op_regs.add(off) as *const u32) }
    }

    fn set_reg(&self, off: usize, val: u32) {
        unsafe { core::ptr::write_volatile(self.op_regs.add(off) as *mut u32, val) }
    }

    fn qh(&self, off: usize) -> &'static mut EhciQh {
        unsafe { &mut *self.dma.ptr_at::<EhciQh>(off) }
    }

    fn qtd(&self, slot: usize) -> &'static mut EhciQtd {
        unsafe { &mut *self.dma.ptr_at::<EhciQtd>(QTD_POOL + slot * 64) }
    }

    fn bounce(&self) -> &'static mut [u8] {
        &mut self.dma.as_slice()[BOUNCE..BOUNCE + BOUNCE_SIZE]
    }

    pub unsafe fn new(pci_dev: &PciDevice) -> Result<Self, &'static str> {
        unsafe { pci_dev.enable_bus_mastering() };

        let bar0_phys = match pci_dev.bars[0] {
            Bar::Memory32 { address, .. } => address as u64,
            Bar::Memory64 { address, .. } => address,
            _ => return Err("EHCI BAR0 is not a memory BAR"),
        };
        let cap_regs = crate::memory::ensure_mmio_mapped(bar0_phys, 4096).as_mut_ptr::<u8>();

        // BIOS -> OS handoff (USBLEGSUP): take ownership so SMM stops touching the controller.
        let hccparams = unsafe { core::ptr::read_volatile(cap_regs.add(0x08) as *const u32) };
        let mut eecp = ((hccparams >> 8) & 0xFF) as u8;
        let mut guard = 0;
        while eecp >= 0x40 && guard < 8 {
            guard += 1;
            let legsup = unsafe { pci_dev.read_u32(eecp) };
            if (legsup & 0xFF) == 0x01 {
                unsafe { pci_dev.write_u32(eecp, legsup | (1 << 24)) };
                let released = crate::interrupts::wait_until(1000, || unsafe { pci_dev.read_u32(eecp) } & (1 << 16) == 0);
                if !released {
                    println!("       EHCI: BIOS did not release controller, forcing");
                    let cur = unsafe { pci_dev.read_u32(eecp) };
                    unsafe { pci_dev.write_u32(eecp, (cur & !(1 << 16)) | (1 << 24)) };
                }
                // Disable all SMI sources.
                unsafe { pci_dev.write_u32(eecp + 4, 0) };
                break;
            }
            eecp = ((legsup >> 8) & 0xFF) as u8;
        }

        let caplength = unsafe { core::ptr::read_volatile(cap_regs) } as usize;
        let op_regs = unsafe { cap_regs.add(caplength) };
        let hcsparams = unsafe { core::ptr::read_volatile(cap_regs.add(0x04) as *const u32) };
        let num_ports = (hcsparams & 0x0F) as usize;

        let dma = crate::memory::alloc_dma(DMA_PAGES)?;
        let ctrl = EhciController { op_regs, num_ports, addr64: hccparams & 1 != 0, last_failure: (0, 0), dma };

        // Stop, wait for halt.
        ctrl.set_reg(0x00, ctrl.reg(0x00) & !0x01);
        crate::interrupts::wait_until(20, || ctrl.reg(0x04) & (1 << 12) != 0);

        // Route all ports to EHCI, no interrupts, clear status.
        ctrl.set_reg(0x40, 1);
        ctrl.set_reg(0x10, 0);
        ctrl.set_reg(0x08, 0);
        ctrl.set_reg(0x04, 0x3F);

        let qh_phys = dma.phys as u32;
        for (off, next, chars) in [
            (QH_CTRL, QH_BULK_OUT, (1 << 15) | (2 << 12) | (64 << 16) | (1 << 14) | (8 << 28)),
            (QH_BULK_OUT, QH_BULK_IN, (2 << 12) | (512 << 16) | (8 << 28)),
            (QH_BULK_IN, QH_CTRL, (2 << 12) | (512 << 16) | (8 << 28)),
        ] {
            let qh = ctrl.qh(off);
            qh.horizontal_link = (qh_phys + next as u32) | 0x02;
            qh.ep_characteristics = chars;
            qh.ep_capabilities = 1 << 30;
            qh.current_qtd = 0;
            qh.next_qtd = QTD_TERMINATE;
            qh.alt_next_qtd = QTD_TERMINATE;
            qh.token = 0;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        ctrl.set_reg(0x18, qh_phys);
        ctrl.set_reg(0x00, ctrl.reg(0x00) | (1 << 5) | 1);
        crate::interrupts::wait_until(100, || ctrl.reg(0x04) & (1 << 15) != 0);

        // CONFIGFLAG=1 just routed every port to EHCI. Power them and give devices time to
        // show up. Note: port-owner bits read before CONFIGFLAG mean nothing (all ports read
        // as companion-owned while it is 0, e.g. after the BIOS handoff), so they're ignored.
        for p in 0..num_ports {
            let sc = ctrl.reg(0x44 + p * 4);
            ctrl.set_reg(0x44 + p * 4, (sc & !0x002A & !(1 << 13)) | (1 << 12));
        }
        delay_ms(150);
        // Low-speed devices (keyboards/mice) can be recognised from the line state alone.
        for p in 0..num_ports {
            let sc = ctrl.reg(0x44 + p * 4);
            if sc & 0x01 != 0 && ((sc >> 10) & 0x03) == 0x01 {
                ctrl.release_port(p);
            }
        }

        ctrl.stop_async();
        Ok(ctrl)
    }

    fn stop_async(&self) {
        let cmd = self.reg(0x00);
        if cmd & (1 << 5) != 0 {
            self.set_reg(0x00, cmd & !(1 << 5));
            crate::interrupts::wait_until(20, || self.reg(0x04) & (1 << 15) == 0);
        }
    }

    fn start_async(&self) {
        self.set_reg(0x00, self.reg(0x00) | (1 << 5) | 1);
    }

    fn release_port(&self, port: usize) {
        let sc = self.reg(0x44 + port * 4);
        self.set_reg(0x44 + port * 4, (sc & !0x002A) | (1 << 13) | (1 << 12));
    }

    /// Reset a root port. Returns Ok(true) if a high-speed device is now enabled on it.
    pub fn reset_port(&mut self, port: usize) -> Result<bool, &'static str> {
        if port >= self.num_ports {
            return Err("Port index out of range");
        }
        let off = 0x44 + port * 4;
        let sc = self.reg(off);
        if (sc & (1 << 13)) != 0 || (sc & 0x01) == 0 {
            return Ok(false);
        }
        if ((sc >> 10) & 0x03) == 0x01 {
            self.release_port(port); // low-speed device
            return Ok(false);
        }

        for _attempt in 0..2 {
            let sc = self.reg(off);
            if (sc & 0x01) == 0 {
                return Ok(false);
            }
            self.set_reg(off, (sc & !(1 << 13) & !0x002A & !(1 << 2)) | (1 << 8) | (1 << 12));
            delay_ms(60);
            let sc = self.reg(off);
            self.set_reg(off, (sc & !(1 << 13) & !0x002A & !(1 << 8)) | (1 << 12));
            crate::interrupts::wait_until(100, || self.reg(off) & (1 << 8) == 0);
            delay_ms(20);
            if crate::interrupts::wait_until(100, || self.reg(off) & (1 << 2) != 0) {
                return Ok(true);
            }
            delay_ms(30);
        }

        // Not enabled after reset: a full-speed device, belongs to the companion.
        if self.reg(off) & 0x01 != 0 {
            self.release_port(port);
        }
        Ok(false)
    }

    /// Run the qTD chain hanging off `qh_off` until `last_slot` completes.
    fn run_transfer(&mut self, qh_off: usize, first_slot: usize, last_slot: usize, timeout_ms: u64) -> Result<(), &'static str> {
        let first_phys = self.dma.phys_at(QTD_POOL + first_slot * 64) as u32;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.qh(qh_off).next_qtd = first_phys;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.start_async();

        let deadline = Deadline::after_ms(timeout_ms);
        let result = loop {
            let tok = unsafe { core::ptr::read_volatile(&self.qtd(last_slot).token) };
            let qh_tok = unsafe { core::ptr::read_volatile(&self.qh(qh_off).token) };
            if (tok & QTD_STATUS_HALTED) != 0 || (qh_tok & QTD_STATUS_HALTED) != 0 {
                break Err("transfer halted");
            }
            if (tok & QTD_STATUS_ACTIVE) == 0 {
                break Ok(());
            }
            let sts = self.reg(0x04);
            if sts & (1 << 4) != 0 {
                break Err("host system error");
            }
            if deadline.expired() {
                break Err("transfer timeout");
            }
            core::hint::spin_loop();
        };

        if result.is_err() {
            self.last_failure = (self.reg(0x04), self.qtd(last_slot).token);
        }
        self.stop_async();
        let qh = self.qh(qh_off);
        qh.next_qtd = QTD_TERMINATE;
        // Deactivate whatever the controller left in the overlay, keeping the data toggle.
        qh.token &= 1 << 31;
        for slot in first_slot..=last_slot {
            self.qtd(slot).token &= !QTD_STATUS_ACTIVE;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        result
    }

    /// Execute a control transfer on endpoint 0.
    pub fn control_transfer(&mut self, dev_addr: u8, req_type: u8, req: u8, value: u16, index: u16, data: &mut [u8]) -> Result<usize, &'static str> {
        let is_in = (req_type & 0x80) != 0;
        let data_len = data.len();
        if data_len + 8 > BOUNCE_SIZE {
            return Err("control transfer too large");
        }
        self.stop_async();

        let bounce_phys = self.dma.phys_at(BOUNCE) as u32;
        let bounce = self.bounce();
        bounce[0] = req_type;
        bounce[1] = req;
        bounce[2..4].copy_from_slice(&value.to_le_bytes());
        bounce[4..6].copy_from_slice(&index.to_le_bytes());
        bounce[6..8].copy_from_slice(&(data_len as u16).to_le_bytes());
        if !is_in && data_len > 0 {
            bounce[8..8 + data_len].copy_from_slice(data);
        }

        let slot_phys = |s: usize| self.dma.phys_at(QTD_POOL + s * 64) as u32;
        let status_slot = if data_len > 0 { 2 } else { 1 };

        let q0 = self.qtd(0);
        q0.next_qtd = slot_phys(1);
        q0.alt_next_qtd = QTD_TERMINATE;
        q0.token = QTD_STATUS_ACTIVE | QTD_CERR_3 | (PID_SETUP << 8) | (8 << 16);
        set_qtd_buffer_ptrs(q0, bounce_phys);

        if data_len > 0 {
            let q1 = self.qtd(1);
            q1.next_qtd = slot_phys(2);
            q1.alt_next_qtd = QTD_TERMINATE;
            let pid = if is_in { PID_IN } else { PID_OUT };
            q1.token = QTD_STATUS_ACTIVE | QTD_CERR_3 | (pid << 8) | ((data_len as u32) << 16) | (1 << 31);
            set_qtd_buffer_ptrs(q1, bounce_phys + 8);
        }

        let qs = self.qtd(status_slot);
        qs.next_qtd = QTD_TERMINATE;
        qs.alt_next_qtd = QTD_TERMINATE;
        let status_pid = if is_in && data_len > 0 { PID_OUT } else { PID_IN };
        qs.token = QTD_STATUS_ACTIVE | QTD_CERR_3 | (status_pid << 8) | (1 << 31);
        set_qtd_buffer_ptrs(qs, bounce_phys);

        let qh = self.qh(QH_CTRL);
        qh.ep_characteristics = (dev_addr as u32 & 0x7F)
            | (2 << 12)        // high speed
            | (1 << 14)        // data toggle from qTD
            | (1 << 15)        // head of reclamation list
            | (64 << 16)
            | (8 << 28);
        qh.current_qtd = 0;
        qh.alt_next_qtd = QTD_TERMINATE;
        qh.token = 0;

        self.run_transfer(QH_CTRL, 0, status_slot, CONTROL_TIMEOUT_MS)?;

        if is_in && data_len > 0 {
            data.copy_from_slice(&self.bounce()[8..8 + data_len]);
        }
        Ok(data_len)
    }

    /// Execute one bulk transfer of up to `BOUNCE_SIZE` bytes.
    pub fn bulk_transfer(&mut self, dev_addr: u8, ep: u8, is_in: bool, max_packet_len: u16, data: &mut [u8]) -> Result<usize, &'static str> {
        let data_len = data.len();
        if data_len > BOUNCE_SIZE {
            return Err("bulk transfer exceeds bounce buffer");
        }
        self.stop_async();

        if !is_in {
            self.bounce()[..data_len].copy_from_slice(data);
        }

        let q = self.qtd(0);
        q.next_qtd = QTD_TERMINATE;
        q.alt_next_qtd = QTD_TERMINATE;
        let pid = if is_in { PID_IN } else { PID_OUT };
        q.token = QTD_STATUS_ACTIVE | QTD_CERR_3 | (pid << 8) | ((data_len as u32) << 16);
        set_qtd_buffer_ptrs(q, self.dma.phys_at(BOUNCE) as u32);

        let qh_off = if is_in { QH_BULK_IN } else { QH_BULK_OUT };
        let qh = self.qh(qh_off);
        qh.ep_characteristics = (dev_addr as u32 & 0x7F)
            | (((ep & 0x0F) as u32) << 8)
            | (2 << 12)                 // high speed
            | ((max_packet_len as u32) << 16)
            | (8 << 28);                // data toggle tracked in the QH overlay
        qh.current_qtd = 0;
        qh.alt_next_qtd = QTD_TERMINATE;
        qh.token &= 1 << 31;

        self.run_transfer(qh_off, 0, 0, BULK_TIMEOUT_MS)?;

        let remaining = ((self.qtd(0).token >> 16) & 0x7FFF) as usize;
        let done = data_len.saturating_sub(remaining);
        if is_in {
            data[..done].copy_from_slice(&self.bounce()[..done]);
        }
        Ok(done)
    }

    /// CLEAR_FEATURE(ENDPOINT_HALT) and reset the queue head's data toggle.
    pub fn clear_endpoint_halt(&mut self, dev_addr: u8, ep_addr: u8) {
        let _ = self.control_transfer(dev_addr, 0x02, 0x01, 0, ep_addr as u16, &mut []);
        let qh_off = if (ep_addr & 0x80) != 0 { QH_BULK_IN } else { QH_BULK_OUT };
        self.qh(qh_off).token = 0;
    }

    /// Read `buffer.len() / 512` sectors starting at `lba` with SCSI READ(10).
    pub fn read_sectors_msc(&mut self, drive: &UsbStorageDrive, lba: u64, buffer: &mut [u8]) -> Result<(), &'static str> {
        let mut last_err = "USB read failed";
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

    fn get_descriptor(&mut self, dev_addr: u8, value: u16, buf: &mut [u8]) -> bool {
        for _ in 0..3 {
            if self.control_transfer(dev_addr, 0x80, 0x06, value, 0, buf).is_ok() {
                return true;
            }
            delay_ms(30);
        }
        false
    }

    /// Enumerate high-speed devices on the root ports and return mass storage drives.
    /// Anything else is released to the companion UHCI controller.
    pub fn scan_storage_devices(&mut self, controller_idx: usize) -> Vec<UsbStorageDrive> {
        let mut drives = Vec::new();
        let mut next_addr = 1u8;
        let connected: Vec<usize> = (0..self.num_ports).filter(|&p| self.reg(0x44 + p * 4) & 0x01 != 0).map(|p| p + 1).collect();
        println!("       {} ports{}, devices on {:?}", self.num_ports, if self.addr64 { " (64-bit)" } else { "" }, connected);

        for port in 0..self.num_ports {
            let sc = self.reg(0x44 + port * 4);
            if (sc & 0x01) == 0 {
                continue;
            }
            match self.reset_port(port) {
                Ok(true) => {}
                Ok(false) => {
                    println!("       EHCI port {}: full/low-speed device, handed to USB 1.1 (portsc {:#x})", port + 1, sc);
                    continue;
                }
                Err(e) => {
                    println!("       EHCI port {}: reset failed: {}", port + 1, e);
                    continue;
                }
            }
            delay_ms(100);

            let mut desc = [0u8; 18];
            let mut ok = false;
            for _ in 0..5 {
                if self.control_transfer(0, 0x80, 0x06, 0x0100, 0, &mut desc[..8]).is_ok() {
                    ok = true;
                    break;
                }
                delay_ms(50);
            }
            if !ok {
                let (sts, tok) = self.last_failure;
                println!("       EHCI port {}: no device descriptor (sts {:#x} tok {:#x} portsc {:#x})", port + 1, sts, tok, self.reg(0x44 + port * 4));
                // A controller fault halts it; restart so the remaining ports still work.
                if sts & (1 << 4) != 0 {
                    self.set_reg(0x04, 0x3F);
                    self.set_reg(0x00, self.reg(0x00) | 1);
                }
                continue;
            }

            let dev_addr = next_addr;
            next_addr += 1;
            if self.control_transfer(0, 0x00, 0x05, dev_addr as u16, 0, &mut []).is_err() {
                println!("       EHCI port {}: SET_ADDRESS failed", port + 1);
                continue;
            }
            delay_ms(20);

            if !self.get_descriptor(dev_addr, 0x0100, &mut desc) {
                continue;
            }
            let vendor_id = u16::from_le_bytes([desc[8], desc[9]]);
            let product_id = u16::from_le_bytes([desc[10], desc[11]]);

            let mut conf_head = [0u8; 9];
            if !self.get_descriptor(dev_addr, 0x0200, &mut conf_head) {
                continue;
            }
            let total_len = (u16::from_le_bytes([conf_head[2], conf_head[3]]) as usize).clamp(9, 512);
            let mut conf = alloc::vec![0u8; total_len];
            if !self.get_descriptor(dev_addr, 0x0200, &mut conf) {
                continue;
            }

            let Some(ep) = msc::parse_msc_endpoints(&conf) else {
                println!("       EHCI port {}: {:04x}:{:04x} is not mass storage, handed to USB 1.1", port + 1, vendor_id, product_id);
                self.release_port(port);
                continue;
            };

            if self.control_transfer(dev_addr, 0x00, 0x09, conf[5] as u16, 0, &mut []).is_err() {
                println!("       EHCI port {}: SET_CONFIGURATION failed", port + 1);
                continue;
            }
            delay_ms(20);
            self.qh(QH_BULK_OUT).token = 0;
            self.qh(QH_BULK_IN).token = 0;

            let mut drive = UsbStorageDrive {
                name: alloc::format!("usb{}", port + 1),
                model: String::new(),
                capacity_mb: 0,
                sectors: 0,
                sector_size: 512,
                controller_ref: UsbControllerRef::Ehci(controller_idx),
                dev_addr,
                ep_in: ep.ep_in,
                ep_out: ep.ep_out,
                max_packet: ep.max_packet.max(64),
                single_sector: false,
            };
            msc::identify(self, &mut drive);
            println!(
                "       EHCI port {}: {} ({} MB, {:04x}:{:04x})",
                port + 1, drive.model, drive.capacity_mb, vendor_id, product_id
            );
            drives.push(drive);
        }
        drives
    }
}

impl msc::BulkTransport for EhciController {
    fn bulk(&mut self, dev_addr: u8, ep: u8, is_in: bool, max_packet: u16, data: &mut [u8]) -> Result<usize, &'static str> {
        self.bulk_transfer(dev_addr, ep, is_in, max_packet, data)
    }

    fn clear_halt(&mut self, dev_addr: u8, ep_addr: u8) {
        self.clear_endpoint_halt(dev_addr, ep_addr);
    }

    fn max_transfer(&self) -> usize {
        BOUNCE_SIZE
    }
}
