//! Realtek RTL8111/8168 (and RTL8169) gigabit Ethernet driver, polled.
//!
//! Written for the 8111B-8111D family found on Core 2 era boards (register layout as
//! used by Linux's r8169 for those chips). Descriptor rings and buffers live in
//! physically contiguous DMA memory; nothing ever blocks.

use crate::memory::DmaRegion;
use crate::pci::{Bar, PciDevice};

const IDR0: usize = 0x00; // MAC address
const MAR0: usize = 0x08; // multicast filter
const TNPDS: usize = 0x20; // TX normal-priority descriptor ring (64-bit)
const CHIP_CMD: usize = 0x37;
const TX_POLL: usize = 0x38;
const INTR_MASK: usize = 0x3C;
const INTR_STATUS: usize = 0x3E;
const TX_CONFIG: usize = 0x40;
const RX_CONFIG: usize = 0x44;
const CFG9346: usize = 0x50;
const PHYAR: usize = 0x60;
const PHY_STATUS: usize = 0x6C;
const PMCH: usize = 0x6F;
const RX_MAX_SIZE: usize = 0xDA;
const CPLUS_CMD: usize = 0xE0;
const RDSAR: usize = 0xE4; // RX descriptor ring (64-bit)
const MAX_TX_PACKET: usize = 0xEC;

const CMD_RESET: u8 = 1 << 4;
const CMD_RX_EN: u8 = 1 << 3;
const CMD_TX_EN: u8 = 1 << 2;
const TX_POLL_NPQ: u8 = 1 << 6;

/// FIFO threshold none, unlimited DMA burst, accept: own MAC, multicast, broadcast.
const RX_CONFIG_VAL: u32 = (7 << 13) | (7 << 8) | (1 << 1) | (1 << 2) | (1 << 3);
/// Standard inter-frame gap, unlimited DMA burst.
const TX_CONFIG_VAL: u32 = (3 << 24) | (7 << 8);

const DESC_OWN: u32 = 1 << 31;
const DESC_EOR: u32 = 1 << 30;
const DESC_FS: u32 = 1 << 29;
const DESC_LS: u32 = 1 << 28;
const RX_RES: u32 = 1 << 21; // receive error summary

const NUM_RX: usize = 32;
const NUM_TX: usize = 32;
const BUF_SIZE: usize = 2048;
// DMA layout: page 0 = both descriptor rings (256-byte aligned), then RX and TX buffers.
const RX_RING: usize = 0;
const TX_RING: usize = 2048;
const RX_BUFS: usize = 4096;
const TX_BUFS: usize = RX_BUFS + NUM_RX * BUF_SIZE;
const DMA_PAGES: usize = 1 + (NUM_RX + NUM_TX) * BUF_SIZE / 4096;

#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    opts1: u32,
    opts2: u32,
    addr: u64,
}

pub struct Rtl8168 {
    mmio: *mut u8,
    dma: DmaRegion,
    pub mac: [u8; 6],
    pub device_id: u16,
    /// Hardware revision bits from TxConfig, for the boot log / bug reports.
    pub xid: u32,
    rx_cur: usize,
    tx_cur: usize,
    pub rx_packets: u64,
    pub tx_packets: u64,
}

unsafe impl Send for Rtl8168 {}

impl Rtl8168 {
    pub fn is_supported(dev: &PciDevice) -> bool {
        dev.vendor_id == 0x10EC && matches!(dev.device_id, 0x8161 | 0x8168 | 0x8169 | 0x8167 | 0x8136)
    }

    fn r8(&self, r: usize) -> u8 { unsafe { core::ptr::read_volatile(self.mmio.add(r)) } }
    fn r16(&self, r: usize) -> u16 { unsafe { core::ptr::read_volatile(self.mmio.add(r) as *const u16) } }
    fn r32(&self, r: usize) -> u32 { unsafe { core::ptr::read_volatile(self.mmio.add(r) as *const u32) } }
    fn w8(&self, r: usize, v: u8) { unsafe { core::ptr::write_volatile(self.mmio.add(r), v) } }
    fn w16(&self, r: usize, v: u16) { unsafe { core::ptr::write_volatile(self.mmio.add(r) as *mut u16, v) } }
    fn w32(&self, r: usize, v: u32) { unsafe { core::ptr::write_volatile(self.mmio.add(r) as *mut u32, v) } }

    fn rx_desc(&self, i: usize) -> *mut Desc {
        self.dma.ptr_at::<Desc>(RX_RING + i * 16)
    }

    fn tx_desc(&self, i: usize) -> *mut Desc {
        self.dma.ptr_at::<Desc>(TX_RING + i * 16)
    }

    pub unsafe fn new(pci: &PciDevice) -> Result<Self, &'static str> {
        unsafe { pci.enable_bus_mastering() };
        // The register window is the first memory BAR (BAR2 on 8168, BAR1 on 8169);
        // BAR0 is the same registers in I/O space.
        let bar = pci
            .bars
            .iter()
            .find_map(|b| match *b {
                Bar::Memory32 { address, .. } => Some(address as u64),
                Bar::Memory64 { address, .. } => Some(address),
                _ => None,
            })
            .ok_or("no memory BAR")?;
        let mmio = crate::memory::ensure_mmio_mapped(bar, 4096).as_mut_ptr::<u8>();
        let dma = crate::memory::alloc_dma(DMA_PAGES)?;
        let mut nic = Rtl8168 {
            mmio,
            dma,
            mac: [0; 6],
            device_id: pci.device_id,
            xid: 0,
            rx_cur: 0,
            tx_cur: 0,
            rx_packets: 0,
            tx_packets: 0,
        };
        nic.xid = nic.r32(TX_CONFIG) & 0x7CF0_0000;

        // Software reset.
        nic.w16(INTR_MASK, 0);
        nic.w8(CHIP_CMD, CMD_RESET);
        if !crate::interrupts::wait_until(100, || nic.r8(CHIP_CMD) & CMD_RESET == 0) {
            return Err("chip reset timed out");
        }

        // The MAC is loaded from the EEPROM into IDR0-5 at power-up.
        for i in 0..6 {
            nic.mac[i] = nic.r8(IDR0 + i);
        }
        if nic.mac == [0; 6] || nic.mac == [0xFF; 6] {
            return Err("chip reports no MAC address");
        }

        nic.power_up_phy();
        nic.setup_rings();
        Ok(nic)
    }

    fn phy_read(&self, reg: u32) -> Option<u16> {
        self.w32(PHYAR, (reg & 0x1F) << 16);
        let mut v = 0;
        if crate::interrupts::wait_until(5, || {
            v = self.r32(PHYAR);
            v & (1 << 31) != 0
        }) {
            Some(v as u16)
        } else {
            None
        }
    }

    fn phy_write(&self, reg: u32, val: u16) {
        self.w32(PHYAR, (1 << 31) | ((reg & 0x1F) << 16) | val as u32);
        crate::interrupts::wait_until(5, || self.r32(PHYAR) & (1 << 31) == 0);
    }

    /// The chip family in Linux r8169 terms, from the TxConfig revision bits.
    pub fn family(&self) -> &'static str {
        match self.xid & 0x7C80_0000 {
            0x3000_0000 | 0x3800_0000 => "8168B",
            0x3C00_0000 | 0x3C80_0000 => "8168C",
            0x2800_0000 => "8168D",
            0x2C00_0000 | 0x2C80_0000 => "8168E",
            0x4800_0000 => "8168F",
            0x4C00_0000 | 0x5000_0000 | 0x5080_0000 | 0x5400_0000 => "8168G/H",
            0x1000_0000 | 0x1800_0000 | 0x9800_0000 => "8169",
            _ => "unknown",
        }
    }

    /// Power up the PLL and PHY and (re)start auto-negotiation, as Linux r8169 does on
    /// open. BIOSes often leave the PHY powered down when Wake-on-LAN / PXE are off.
    fn power_up_phy(&self) {
        if matches!(self.family(), "8168D" | "8168E") {
            self.w8(PMCH, self.r8(PMCH) | 0x80);
        }
        self.phy_write(0x1F, 0x0000); // select PHY register page 0
        if matches!(self.family(), "8168B" | "8168C" | "8168D") {
            self.phy_write(0x0E, 0x0000);
        }
        self.phy_write(0x00, (1 << 12) | (1 << 9)); // BMCR: auto-negotiate + restart, powered up
    }

    /// Raw PHY/MAC state for the hardware info page: "BMCR/BMSR/link-partner PHYstatus".
    pub fn debug_state(&self) -> alloc::string::String {
        let rd = |r| self.phy_read(r).map(|v| alloc::format!("{:04x}", v)).unwrap_or_else(|| "????".into());
        alloc::format!(
            "{} xid {:#x} BMCR {} BMSR {} LPA {} st {:02x}",
            self.family(),
            self.xid >> 20,
            rd(0),
            rd(1),
            rd(5),
            self.r8(PHY_STATUS)
        )
    }

    fn setup_rings(&mut self) {
        for i in 0..NUM_RX {
            let eor = if i == NUM_RX - 1 { DESC_EOR } else { 0 };
            let d = Desc { opts1: DESC_OWN | eor | BUF_SIZE as u32, opts2: 0, addr: self.dma.phys_at(RX_BUFS + i * BUF_SIZE) };
            unsafe { self.rx_desc(i).write_volatile(d) };
        }
        for i in 0..NUM_TX {
            let eor = if i == NUM_TX - 1 { DESC_EOR } else { 0 };
            let d = Desc { opts1: eor, opts2: 0, addr: self.dma.phys_at(TX_BUFS + i * BUF_SIZE) };
            unsafe { self.tx_desc(i).write_volatile(d) };
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        self.w8(CFG9346, 0xC0); // unlock config registers
        // No VLAN tag stripping or checksum offload: smoltcp checks everything itself.
        let cplus = self.r16(CPLUS_CMD);
        self.w16(CPLUS_CMD, cplus & !((1 << 6) | (1 << 5)));
        self.w16(RX_MAX_SIZE, BUF_SIZE as u16);
        self.w8(MAX_TX_PACKET, 0x3F);

        let tx = self.dma.phys_at(TX_RING);
        let rx = self.dma.phys_at(RX_RING);
        self.w32(TNPDS, tx as u32);
        self.w32(TNPDS + 4, (tx >> 32) as u32);
        self.w32(RDSAR, rx as u32);
        self.w32(RDSAR + 4, (rx >> 32) as u32);

        self.w32(MAR0, 0);
        self.w32(MAR0 + 4, 0);
        self.w32(TX_CONFIG, TX_CONFIG_VAL);
        self.w32(RX_CONFIG, RX_CONFIG_VAL);
        self.w8(CHIP_CMD, CMD_RX_EN | CMD_TX_EN);
        // Some revisions only latch the configs once RX/TX are enabled; write them again.
        self.w32(TX_CONFIG, TX_CONFIG_VAL);
        self.w32(RX_CONFIG, RX_CONFIG_VAL);
        self.w8(CFG9346, 0x00);

        self.w16(INTR_MASK, 0); // polled
        self.w16(INTR_STATUS, 0xFFFF);
    }

    pub fn link_up(&self) -> bool {
        self.r8(PHY_STATUS) & 0x02 != 0
    }

    pub fn speed(&self) -> u32 {
        let s = self.r8(PHY_STATUS);
        if s & 0x10 != 0 { 1000 } else if s & 0x08 != 0 { 100 } else { 10 }
    }

    pub fn receive(&mut self, out: &mut [u8]) -> Option<usize> {
        loop {
            let i = self.rx_cur;
            let d = unsafe { self.rx_desc(i).read_volatile() };
            if d.opts1 & DESC_OWN != 0 {
                return None;
            }
            // Length includes the 4-byte CRC.
            let len = ((d.opts1 & 0x3FFF) as usize).saturating_sub(4);
            let whole = d.opts1 & (DESC_FS | DESC_LS) == (DESC_FS | DESC_LS);
            let good = whole && d.opts1 & RX_RES == 0 && len > 0 && len <= out.len();
            if good {
                let src = self.dma.ptr_at::<u8>(RX_BUFS + i * BUF_SIZE);
                unsafe { core::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), len) };
            }
            // Give the descriptor back to the chip.
            let eor = if i == NUM_RX - 1 { DESC_EOR } else { 0 };
            let fresh = Desc { opts1: 0, opts2: 0, addr: self.dma.phys_at(RX_BUFS + i * BUF_SIZE) };
            unsafe { self.rx_desc(i).write_volatile(fresh) };
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            unsafe { (*self.rx_desc(i)).opts1 = DESC_OWN | eor | BUF_SIZE as u32 };
            self.rx_cur = (i + 1) % NUM_RX;
            // Acknowledge RX status bits (RX ok / error / overflow).
            self.w16(INTR_STATUS, 0x0053);
            if good {
                self.rx_packets += 1;
                return Some(len);
            }
        }
    }

    pub fn tx_ready(&self) -> bool {
        unsafe { self.tx_desc(self.tx_cur).read_volatile() }.opts1 & DESC_OWN == 0
    }

    pub fn transmit(&mut self, len: usize, fill: &mut dyn FnMut(&mut [u8])) -> bool {
        if !self.tx_ready() || len > BUF_SIZE {
            return false;
        }
        let i = self.tx_cur;
        // Pad runt frames to the 60-byte Ethernet minimum ourselves; not every 8168
        // revision does it reliably in hardware.
        let wire_len = len.max(60);
        let buf = unsafe { core::slice::from_raw_parts_mut(self.dma.ptr_at::<u8>(TX_BUFS + i * BUF_SIZE), wire_len) };
        buf[len..].fill(0);
        fill(&mut buf[..len]);
        let eor = if i == NUM_TX - 1 { DESC_EOR } else { 0 };
        unsafe {
            let d = self.tx_desc(i);
            (*d).opts2 = 0;
            (*d).addr = self.dma.phys_at(TX_BUFS + i * BUF_SIZE);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            core::ptr::write_volatile(&mut (*d).opts1, DESC_OWN | DESC_FS | DESC_LS | eor | wire_len as u32);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.tx_cur = (i + 1) % NUM_TX;
        self.w8(TX_POLL, TX_POLL_NPQ);
        self.tx_packets += 1;
        true
    }
}
