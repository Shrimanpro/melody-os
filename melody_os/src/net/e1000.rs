use alloc::vec::Vec;
use x86_64::VirtAddr;

use crate::pci::{Bar, PciDevice};

pub const E1000_REG_CTRL: usize = 0x0000;
pub const E1000_REG_STATUS: usize = 0x0008;
pub const E1000_REG_EERD: usize = 0x0014;
pub const E1000_REG_ICR: usize = 0x00C0;
pub const E1000_REG_IMS: usize = 0x00D0;
pub const E1000_REG_RCTL: usize = 0x0100;
pub const E1000_REG_TCTL: usize = 0x0400;

pub const E1000_REG_RDBAL: usize = 0x2800;
pub const E1000_REG_RDBAH: usize = 0x2804;
pub const E1000_REG_RDLEN: usize = 0x2808;
pub const E1000_REG_RDH: usize = 0x2810;
pub const E1000_REG_RDT: usize = 0x2818;

pub const E1000_REG_TDBAL: usize = 0x3800;
pub const E1000_REG_TDBAH: usize = 0x3804;
pub const E1000_REG_TDLEN: usize = 0x3808;
pub const E1000_REG_TDH: usize = 0x3810;
pub const E1000_REG_TDT: usize = 0x3818;

pub const E1000_REG_RAL: usize = 0x5400;
pub const E1000_REG_RAH: usize = 0x5404;

pub const E1000_CTRL_SLU: u32 = 1 << 6; // Set Link Up
pub const E1000_CTRL_RST: u32 = 1 << 26; // Reset

pub const E1000_RCTL_EN: u32 = 1 << 1; // Receiver Enable
pub const E1000_RCTL_SBP: u32 = 1 << 2; // Store Bad Packets
pub const E1000_RCTL_UPE: u32 = 1 << 3; // Unicast Promiscuous
pub const E1000_RCTL_MPE: u32 = 1 << 4; // Multicast Promiscuous
pub const E1000_RCTL_BAM: u32 = 1 << 15; // Broadcast Accept Mode
pub const E1000_RCTL_BSIZE_2048: u32 = 0 << 16; // Buffer size 2048 bytes
pub const E1000_RCTL_SECRC: u32 = 1 << 26; // Strip Ethernet CRC

pub const E1000_TCTL_EN: u32 = 1 << 1; // Transmit Enable
pub const E1000_TCTL_PSP: u32 = 1 << 3; // Pad Short Packets

pub const E1000_TXD_CMD_EOP: u8 = 1 << 0; // End of Packet
pub const E1000_TXD_CMD_IFCS: u8 = 1 << 1; // Insert FCS
pub const E1000_TXD_CMD_RS: u8 = 1 << 3; // Report Status

pub const E1000_TXD_STAT_DD: u8 = 1 << 0; // Descriptor Done
pub const E1000_RXD_STAT_DD: u8 = 1 << 0; // Descriptor Done
pub const E1000_RXD_STAT_EOP: u8 = 1 << 1; // End of Packet

pub const NUM_RX_DESCS: usize = 32;
pub const NUM_TX_DESCS: usize = 8;
pub const PKT_BUF_SIZE: usize = 2048;

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct E1000RxDesc {
    pub addr: u64,
    pub length: u16,
    pub checksum: u16,
    pub status: u8,
    pub errors: u8,
    pub special: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct E1000TxDesc {
    pub addr: u64,
    pub length: u16,
    pub cso: u8,
    pub cmd: u8,
    pub status: u8,
    pub css: u8,
    pub special: u16,
}

pub struct E1000Driver {
    pub mmio_base: *mut u8,
    pub mac_addr: [u8; 6],
    pub rx_descs: *mut E1000RxDesc,
    pub rx_buffers: [*mut u8; NUM_RX_DESCS],
    pub rx_cur: usize,
    pub tx_descs: *mut E1000TxDesc,
    pub tx_buffers: [*mut u8; NUM_TX_DESCS],
    pub tx_cur: usize,
}

impl E1000Driver {
    pub unsafe fn new(pci_dev: &PciDevice, phys_mem_offset: VirtAddr) -> Result<Self, &'static str> {
        unsafe { pci_dev.enable_bus_mastering() };

        let bar0_phys = match pci_dev.bars[0] {
            Bar::Memory32 { address, .. } => address as u64,
            Bar::Memory64 { address, .. } => address,
            _ => return Err("E1000 BAR0 is not a memory BAR"),
        };

        let mmio_base = (phys_mem_offset + bar0_phys).as_mut_ptr::<u8>();
        let phys_offset = phys_mem_offset.as_u64();

        // 1. Reset controller
        unsafe {
            let ctrl = core::ptr::read_volatile(mmio_base.add(E1000_REG_CTRL) as *const u32);
            core::ptr::write_volatile(mmio_base.add(E1000_REG_CTRL) as *mut u32, ctrl | E1000_CTRL_RST);

            for _ in 0..10_000 {
                core::hint::spin_loop();
            }

            // Set Link Up
            let ctrl = core::ptr::read_volatile(mmio_base.add(E1000_REG_CTRL) as *const u32);
            core::ptr::write_volatile(mmio_base.add(E1000_REG_CTRL) as *mut u32, ctrl | E1000_CTRL_SLU);
        }

        // 2. Read MAC Address from Receive Address registers (RAL/RAH)
        let mut mac_addr = [0u8; 6];
        unsafe {
            let ral = core::ptr::read_volatile(mmio_base.add(E1000_REG_RAL) as *const u32);
            let rah = core::ptr::read_volatile(mmio_base.add(E1000_REG_RAH) as *const u32);
            mac_addr[0] = (ral & 0xFF) as u8;
            mac_addr[1] = ((ral >> 8) & 0xFF) as u8;
            mac_addr[2] = ((ral >> 16) & 0xFF) as u8;
            mac_addr[3] = ((ral >> 24) & 0xFF) as u8;
            mac_addr[4] = (rah & 0xFF) as u8;
            mac_addr[5] = ((rah >> 8) & 0xFF) as u8;
        }
        crate::println!(
            "E1000: MAC Address: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac_addr[0], mac_addr[1], mac_addr[2], mac_addr[3], mac_addr[4], mac_addr[5]
        );

        // 3. Initialize Receive Descriptors
        let rx_descs_buf = alloc::vec![0u8; NUM_RX_DESCS * core::mem::size_of::<E1000RxDesc>()].leak();
        let rx_descs = rx_descs_buf.as_mut_ptr() as *mut E1000RxDesc;
        let rx_descs_phys = if (rx_descs as u64) >= phys_offset { (rx_descs as u64) - phys_offset } else { rx_descs as u64 };

        let mut rx_buffers = [core::ptr::null_mut(); NUM_RX_DESCS];
        for i in 0..NUM_RX_DESCS {
            let buf = alloc::vec![0u8; PKT_BUF_SIZE].leak();
            let buf_virt = buf.as_mut_ptr();
            let buf_phys = if (buf_virt as u64) >= phys_offset { (buf_virt as u64) - phys_offset } else { buf_virt as u64 };
            rx_buffers[i] = buf_virt;

            unsafe {
                let desc = &mut *rx_descs.add(i);
                desc.addr = buf_phys;
                desc.status = 0;
            }
        }

        unsafe {
            core::ptr::write_volatile(mmio_base.add(E1000_REG_RDBAL) as *mut u32, (rx_descs_phys & 0xFFFFFFFF) as u32);
            core::ptr::write_volatile(mmio_base.add(E1000_REG_RDBAH) as *mut u32, ((rx_descs_phys >> 32) & 0xFFFFFFFF) as u32);
            core::ptr::write_volatile(
                mmio_base.add(E1000_REG_RDLEN) as *mut u32,
                (NUM_RX_DESCS * core::mem::size_of::<E1000RxDesc>()) as u32,
            );
            core::ptr::write_volatile(mmio_base.add(E1000_REG_RDH) as *mut u32, 0);
            core::ptr::write_volatile(mmio_base.add(E1000_REG_RDT) as *mut u32, (NUM_RX_DESCS - 1) as u32);

            // Enable Receiver
            let rctl = E1000_RCTL_EN | E1000_RCTL_BAM | E1000_RCTL_BSIZE_2048 | E1000_RCTL_SECRC;
            core::ptr::write_volatile(mmio_base.add(E1000_REG_RCTL) as *mut u32, rctl);
        }

        // 4. Initialize Transmit Descriptors
        let tx_descs_buf = alloc::vec![0u8; NUM_TX_DESCS * core::mem::size_of::<E1000TxDesc>()].leak();
        let tx_descs = tx_descs_buf.as_mut_ptr() as *mut E1000TxDesc;
        let tx_descs_phys = if (tx_descs as u64) >= phys_offset { (tx_descs as u64) - phys_offset } else { tx_descs as u64 };

        let mut tx_buffers = [core::ptr::null_mut(); NUM_TX_DESCS];
        for i in 0..NUM_TX_DESCS {
            let buf = alloc::vec![0u8; PKT_BUF_SIZE].leak();
            let buf_virt = buf.as_mut_ptr();
            let buf_phys = if (buf_virt as u64) >= phys_offset { (buf_virt as u64) - phys_offset } else { buf_virt as u64 };
            tx_buffers[i] = buf_virt;

            unsafe {
                let desc = &mut *tx_descs.add(i);
                desc.addr = buf_phys;
                desc.cmd = 0;
                desc.status = E1000_TXD_STAT_DD; // Initially done
            }
        }

        unsafe {
            core::ptr::write_volatile(mmio_base.add(E1000_REG_TDBAL) as *mut u32, (tx_descs_phys & 0xFFFFFFFF) as u32);
            core::ptr::write_volatile(mmio_base.add(E1000_REG_TDBAH) as *mut u32, ((tx_descs_phys >> 32) & 0xFFFFFFFF) as u32);
            core::ptr::write_volatile(
                mmio_base.add(E1000_REG_TDLEN) as *mut u32,
                (NUM_TX_DESCS * core::mem::size_of::<E1000TxDesc>()) as u32,
            );
            core::ptr::write_volatile(mmio_base.add(E1000_REG_TDH) as *mut u32, 0);
            core::ptr::write_volatile(mmio_base.add(E1000_REG_TDT) as *mut u32, 0);

            // Enable Transmitter
            let tctl = E1000_TCTL_EN | E1000_TCTL_PSP | (15 << 4) | (0x40 << 12);
            core::ptr::write_volatile(mmio_base.add(E1000_REG_TCTL) as *mut u32, tctl);
        }

        Ok(E1000Driver {
            mmio_base,
            mac_addr,
            rx_descs,
            rx_buffers,
            rx_cur: 0,
            tx_descs,
            tx_buffers,
            tx_cur: 0,
        })
    }

    /// Transmit a packet over Ethernet
    pub fn send_packet(&mut self, packet: &[u8]) -> Result<(), &'static str> {
        if packet.len() > PKT_BUF_SIZE {
            return Err("Packet exceeds maximum transmission unit");
        }

        let desc = unsafe { &mut *self.tx_descs.add(self.tx_cur) };
        // Wait until previous transmission in this slot is done
        while (desc.status & E1000_TXD_STAT_DD) == 0 {
            core::hint::spin_loop();
        }

        unsafe {
            core::ptr::copy_nonoverlapping(packet.as_ptr(), self.tx_buffers[self.tx_cur], packet.len());
        }

        desc.length = packet.len() as u16;
        desc.cmd = E1000_TXD_CMD_EOP | E1000_TXD_CMD_IFCS | E1000_TXD_CMD_RS;
        desc.status = 0;

        self.tx_cur = (self.tx_cur + 1) % NUM_TX_DESCS;
        unsafe {
            core::ptr::write_volatile(self.mmio_base.add(E1000_REG_TDT) as *mut u32, self.tx_cur as u32);
        }

        Ok(())
    }

    /// Receive a packet from the network if one is available
    pub fn receive_packet(&mut self) -> Option<Vec<u8>> {
        let desc = unsafe { &mut *self.rx_descs.add(self.rx_cur) };
        if (desc.status & E1000_RXD_STAT_DD) == 0 {
            return None;
        }

        let len = desc.length as usize;
        let mut packet = alloc::vec![0u8; len];
        unsafe {
            core::ptr::copy_nonoverlapping(self.rx_buffers[self.rx_cur], packet.as_mut_ptr(), len);
        }

        // Reset descriptor status and update tail
        desc.status = 0;
        let prev_cur = self.rx_cur;
        self.rx_cur = (self.rx_cur + 1) % NUM_RX_DESCS;

        unsafe {
            core::ptr::write_volatile(self.mmio_base.add(E1000_REG_RDT) as *mut u32, prev_cur as u32);
        }

        Some(packet)
    }
}
