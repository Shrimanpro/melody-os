//! Intel PRO/1000 (e1000 / e1000e family) Ethernet driver, polled.
//!
//! Covers the classic 8254x parts (what QEMU emulates) and the ICH8/ICH9/ICH10 integrated
//! LAN found on Core 2 era Dell desktops (82566 / 82567 / 82562V). The ICH parts need a few
//! extra steps: the software/firmware semaphore around reset, PHY power-down bits cleared,
//! and the management engine told not to answer ARP on our behalf.

use crate::memory::DmaRegion;
use crate::pci::{Bar, PciDevice};

const CTRL: usize = 0x0000;
const STATUS: usize = 0x0008;
const EERD: usize = 0x0014;
const ICR: usize = 0x00C0;
const IMC: usize = 0x00D8;
const RCTL: usize = 0x0100;
const TCTL: usize = 0x0400;
const TIPG: usize = 0x0410;
const EXTCNF_CTRL: usize = 0x0F00;
const PHY_CTRL: usize = 0x0F10;
const RDBAL: usize = 0x2800;
const RDBAH: usize = 0x2804;
const RDLEN: usize = 0x2808;
const RDH: usize = 0x2810;
const RDT: usize = 0x2818;
const TDBAL: usize = 0x3800;
const TDBAH: usize = 0x3804;
const TDLEN: usize = 0x3808;
const TDH: usize = 0x3810;
const TDT: usize = 0x3818;
const TXDCTL: usize = 0x3828;
const MTA: usize = 0x5200;
const RAL: usize = 0x5400;
const RAH: usize = 0x5404;
const MANC: usize = 0x5820;

const CTRL_SLU: u32 = 1 << 6;
const CTRL_ASDE: u32 = 1 << 5;
const CTRL_LRST: u32 = 1 << 3;
const CTRL_ILOS: u32 = 1 << 7;
const CTRL_RST: u32 = 1 << 26;
const CTRL_VME: u32 = 1 << 30;
const CTRL_PHY_RST: u32 = 1 << 31;

const RCTL_EN: u32 = 1 << 1;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;

const TXD_CMD_EOP: u8 = 1 << 0;
const TXD_CMD_IFCS: u8 = 1 << 1;
const TXD_CMD_RS: u8 = 1 << 3;
const DESC_DD: u8 = 1 << 0;
const RXD_EOP: u8 = 1 << 1;

const NUM_RX: usize = 32;
const NUM_TX: usize = 32;
const BUF_SIZE: usize = 2048;
// DMA layout: page 0 holds both rings, then RX buffers, then TX buffers.
const RX_RING: usize = 0;
const TX_RING: usize = 2048;
const RX_BUFS: usize = 4096;
const TX_BUFS: usize = RX_BUFS + NUM_RX * BUF_SIZE;
const DMA_PAGES: usize = 1 + (NUM_RX + NUM_TX) * BUF_SIZE / 4096;

/// ICH8/9/10 integrated LAN device IDs (e1000e "ich8lan" family).
const ICH_IDS: &[u16] = &[
    0x1049, 0x104A, 0x104B, 0x104C, 0x104D, // ICH8: 82566MM/DM/DC, 82562V/G
    0x10BD, 0x10BF, 0x10C0, 0x10C2, 0x10C3, 0x10C4, 0x10C5, 0x10CB, 0x294C, // ICH9: 82566DM-2, 82567V/LF, 82562V-2...
    0x10CC, 0x10CD, 0x10CE, 0x10DE, 0x10DF, 0x10E5, 0x10F5, // ICH10: 82567LM-3, 82567LF-3, 82567V-3...
];

#[repr(C)]
#[derive(Clone, Copy)]
struct RxDesc {
    addr: u64,
    length: u16,
    checksum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TxDesc {
    addr: u64,
    length: u16,
    cso: u8,
    cmd: u8,
    status: u8,
    css: u8,
    special: u16,
}

pub struct E1000 {
    mmio: *mut u8,
    dma: DmaRegion,
    pub mac: [u8; 6],
    pub device_id: u16,
    pub ich: bool,
    pub mac_from_nvm: bool,
    rx_cur: usize,
    tx_cur: usize,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub tx_dropped: u64,
}

unsafe impl Send for E1000 {}

impl E1000 {
    fn r(&self, reg: usize) -> u32 {
        unsafe { core::ptr::read_volatile(self.mmio.add(reg) as *const u32) }
    }

    fn w(&self, reg: usize, v: u32) {
        unsafe { core::ptr::write_volatile(self.mmio.add(reg) as *mut u32, v) }
    }

    fn rx_desc(&self, i: usize) -> *mut RxDesc {
        self.dma.ptr_at::<RxDesc>(RX_RING + i * 16)
    }

    fn tx_desc(&self, i: usize) -> *mut TxDesc {
        self.dma.ptr_at::<TxDesc>(TX_RING + i * 16)
    }

    pub fn is_supported(dev: &PciDevice) -> bool {
        dev.vendor_id == 0x8086
    }

    pub unsafe fn new(pci: &PciDevice) -> Result<Self, &'static str> {
        unsafe { pci.enable_bus_mastering() };
        let bar0 = match pci.bars[0] {
            Bar::Memory32 { address, .. } => address as u64,
            Bar::Memory64 { address, .. } => address,
            _ => return Err("BAR0 is not a memory BAR"),
        };
        let mmio = crate::memory::ensure_mmio_mapped(bar0, 128 * 1024).as_mut_ptr::<u8>();
        let dma = crate::memory::alloc_dma(DMA_PAGES)?;
        let ich = ICH_IDS.contains(&pci.device_id);
        let mut nic = E1000 {
            mmio,
            dma,
            mac: [0; 6],
            device_id: pci.device_id,
            ich,
            mac_from_nvm: true,
            rx_cur: 0,
            tx_cur: 0,
            rx_packets: 0,
            tx_packets: 0,
            tx_dropped: 0,
        };
        nic.reset();
        nic.read_mac();
        nic.setup_rings();
        Ok(nic)
    }

    fn reset(&mut self) {
        self.w(IMC, 0xFFFF_FFFF);
        self.w(RCTL, 0);
        self.w(TCTL, TCTL_PSP);
        crate::interrupts::delay_ms(10);

        if self.ich {
            // Take the software flag so the management firmware doesn't touch the PHY
            // while we reset.
            self.w(EXTCNF_CTRL, self.r(EXTCNF_CTRL) | (1 << 5));
            crate::interrupts::wait_until(100, || self.r(EXTCNF_CTRL) & (1 << 5) != 0);
        }

        self.w(CTRL, self.r(CTRL) | CTRL_RST);
        crate::interrupts::delay_ms(25);
        crate::interrupts::wait_until(50, || self.r(CTRL) & CTRL_RST == 0);
        self.w(IMC, 0xFFFF_FFFF);
        let _ = self.r(ICR);

        if self.ich {
            self.w(EXTCNF_CTRL, self.r(EXTCNF_CTRL) & !(1 << 5));
            // Clear "GbE disable" and the low-power link-up modes.
            self.w(PHY_CTRL, self.r(PHY_CTRL) & !(0x40 | 0x08 | 0x04 | 0x02));
            // Datasheet-required bit in TXDCTL for ICH parts.
            self.w(TXDCTL, self.r(TXDCTL) | (1 << 22));
        }

        // Stop the management engine (AMT) from answering ARP itself, and let it forward
        // management traffic to us. Without this, ARP requests never reach the OS on
        // AMT-enabled OptiPlex boards.
        let manc = self.r(MANC);
        self.w(MANC, (manc & !(1 << 13)) | (1 << 21));

        let ctrl = self.r(CTRL);
        self.w(CTRL, (ctrl | CTRL_SLU | CTRL_ASDE) & !(CTRL_LRST | CTRL_PHY_RST | CTRL_ILOS | CTRL_VME));
    }

    fn read_mac(&mut self) {
        let ral = self.r(RAL);
        let rah = self.r(RAH);
        let mut mac = [ral as u8, (ral >> 8) as u8, (ral >> 16) as u8, (ral >> 24) as u8, rah as u8, (rah >> 8) as u8];

        if mac == [0; 6] || mac == [0xFF; 6] {
            // Older parts don't preload RAL/RAH; read the EEPROM (not present on ICH,
            // which always preloads from flash).
            if !self.ich {
                for i in 0..3u32 {
                    self.w(EERD, 1 | (i << 8));
                    let mut v = 0;
                    if crate::interrupts::wait_until(10, || {
                        v = self.r(EERD);
                        v & (1 << 4) != 0
                    }) {
                        mac[i as usize * 2] = (v >> 16) as u8;
                        mac[i as usize * 2 + 1] = (v >> 24) as u8;
                    }
                }
            }
        }
        if mac == [0; 6] || mac == [0xFF; 6] {
            mac = [0x02, 0x4D, 0x45, 0x4C, 0x4F, 0x01]; // locally administered fallback
            self.mac_from_nvm = false;
        }
        self.mac = mac;
        self.w(RAL, u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]));
        self.w(RAH, u16::from_le_bytes([mac[4], mac[5]]) as u32 | (1 << 31));
    }

    fn setup_rings(&mut self) {
        for i in 0..128 {
            self.w(MTA + i * 4, 0);
        }

        for i in 0..NUM_RX {
            let d = RxDesc { addr: self.dma.phys_at(RX_BUFS + i * BUF_SIZE), length: 0, checksum: 0, status: 0, errors: 0, special: 0 };
            unsafe { self.rx_desc(i).write_volatile(d) };
        }
        for i in 0..NUM_TX {
            // Mark every TX slot as done so the first pass through the ring is free.
            let d = TxDesc { addr: self.dma.phys_at(TX_BUFS + i * BUF_SIZE), length: 0, cso: 0, cmd: 0, status: DESC_DD, css: 0, special: 0 };
            unsafe { self.tx_desc(i).write_volatile(d) };
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        let rx_phys = self.dma.phys_at(RX_RING);
        self.w(RDBAL, rx_phys as u32);
        self.w(RDBAH, (rx_phys >> 32) as u32);
        self.w(RDLEN, (NUM_RX * 16) as u32);
        self.w(RDH, 0);
        self.w(RDT, (NUM_RX - 1) as u32);
        // Unicast to our MAC + broadcast, 2 KiB buffers, strip CRC.
        self.w(RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);

        let tx_phys = self.dma.phys_at(TX_RING);
        self.w(TDBAL, tx_phys as u32);
        self.w(TDBAH, (tx_phys >> 32) as u32);
        self.w(TDLEN, (NUM_TX * 16) as u32);
        self.w(TDH, 0);
        self.w(TDT, 0);
        self.w(TIPG, 0x0060_200A);
        self.w(TCTL, TCTL_EN | TCTL_PSP | (0x0F << 4) | (0x40 << 12) | (1 << 24));
    }

    pub fn link_up(&self) -> bool {
        self.r(STATUS) & (1 << 1) != 0
    }

    /// Link speed in Mbit/s (only meaningful when the link is up).
    pub fn speed(&self) -> u32 {
        match (self.r(STATUS) >> 6) & 0x3 {
            0 => 10,
            1 => 100,
            _ => 1000,
        }
    }

    /// Copy the next received frame into `out`; returns its length.
    pub fn receive(&mut self, out: &mut [u8]) -> Option<usize> {
        loop {
            let d = unsafe { self.rx_desc(self.rx_cur).read_volatile() };
            if d.status & DESC_DD == 0 {
                return None;
            }
            let i = self.rx_cur;
            let len = d.length as usize;
            let good = d.status & RXD_EOP != 0 && d.errors == 0 && len <= out.len();
            if good {
                let src = self.dma.ptr_at::<u8>(RX_BUFS + i * BUF_SIZE);
                unsafe { core::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), len) };
            }
            // Hand the descriptor back to the NIC.
            let fresh = RxDesc { addr: self.dma.phys_at(RX_BUFS + i * BUF_SIZE), length: 0, checksum: 0, status: 0, errors: 0, special: 0 };
            unsafe { self.rx_desc(i).write_volatile(fresh) };
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            self.w(RDT, i as u32);
            self.rx_cur = (i + 1) % NUM_RX;
            if good {
                self.rx_packets += 1;
                return Some(len);
            }
        }
    }

    pub fn tx_ready(&self) -> bool {
        unsafe { self.tx_desc(self.tx_cur).read_volatile() }.status & DESC_DD != 0
    }

    /// Queue one frame for transmission. Never blocks: returns false if the ring is full.
    pub fn transmit(&mut self, len: usize, fill: impl FnOnce(&mut [u8])) -> bool {
        if !self.tx_ready() || len > BUF_SIZE {
            self.tx_dropped += 1;
            return false;
        }
        let i = self.tx_cur;
        let buf = unsafe { core::slice::from_raw_parts_mut(self.dma.ptr_at::<u8>(TX_BUFS + i * BUF_SIZE), len) };
        fill(buf);
        let d = TxDesc {
            addr: self.dma.phys_at(TX_BUFS + i * BUF_SIZE),
            length: len as u16,
            cso: 0,
            cmd: TXD_CMD_EOP | TXD_CMD_IFCS | TXD_CMD_RS,
            status: 0,
            css: 0,
            special: 0,
        };
        unsafe { self.tx_desc(i).write_volatile(d) };
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.tx_cur = (i + 1) % NUM_TX;
        self.w(TDT, self.tx_cur as u32);
        self.tx_packets += 1;
        true
    }
}
