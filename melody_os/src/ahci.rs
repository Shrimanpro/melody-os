use alloc::vec::Vec;
use core::ptr::write_bytes;
use volatile::Volatile;
use x86_64::VirtAddr;

use crate::pci::{Bar, PciDevice};

pub const HBA_PORT_DET_PRESENT: u32 = 0x3;
pub const HBA_PORT_IPM_ACTIVE: u32 = 0x1;

pub const SATA_SIG_ATA: u32 = 0x00000101;   // SATA drive
pub const SATA_SIG_ATAPI: u32 = 0xEB140101; // SATAPI drive
pub const SATA_SIG_SEMB: u32 = 0xC33C0101;  // Enclosure management bridge
pub const SATA_SIG_PM: u32 = 0x96690101;    // Port multiplier

pub const HBA_PXCMD_ST: u32 = 1 << 0;
pub const HBA_PXCMD_FRE: u32 = 1 << 4;
pub const HBA_PXCMD_FR: u32 = 1 << 14;
pub const HBA_PXCMD_CR: u32 = 1 << 15;

pub const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
pub const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
pub const ATA_CMD_IDENTIFY: u8 = 0xEC;

pub const FIS_TYPE_REG_H2D: u8 = 0x27;

#[repr(C)]
pub struct HbaPort {
    pub clb: Volatile<u32>,
    pub clbu: Volatile<u32>,
    pub fb: Volatile<u32>,
    pub fbu: Volatile<u32>,
    pub is: Volatile<u32>,
    pub ie: Volatile<u32>,
    pub cmd: Volatile<u32>,
    pub reserved0: Volatile<u32>,
    pub tfd: Volatile<u32>,
    pub sig: Volatile<u32>,
    pub ssts: Volatile<u32>,
    pub sctl: Volatile<u32>,
    pub serr: Volatile<u32>,
    pub sact: Volatile<u32>,
    pub ci: Volatile<u32>,
    pub sntf: Volatile<u32>,
    pub fbs: Volatile<u32>,
    pub devslp: Volatile<u32>,
    pub reserved1: [Volatile<u32>; 10],
    pub vendor: [Volatile<u32>; 4],
}

#[repr(C)]
pub struct HbaMemory {
    pub cap: Volatile<u32>,
    pub ghc: Volatile<u32>,
    pub is: Volatile<u32>,
    pub pi: Volatile<u32>,
    pub vs: Volatile<u32>,
    pub ccc_ctl: Volatile<u32>,
    pub ccc_pts: Volatile<u32>,
    pub em_loc: Volatile<u32>,
    pub em_ctl: Volatile<u32>,
    pub cap2: Volatile<u32>,
    pub bohc: Volatile<u32>,
    pub reserved: [u8; 116],
    pub vendor: [u8; 96],
    pub ports: [HbaPort; 32],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct HbaCmdHeader {
    pub flags: u16,
    pub prdtl: u16,
    pub prdbc: u32,
    pub ctba: u32,
    pub ctbau: u32,
    pub reserved: [u32; 4],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct HbaPrdtEntry {
    pub dba: u32,
    pub dbau: u32,
    pub reserved: u32,
    pub dbc: u32, // bit 31: IOC, bits 0-21: byte count (must be odd, i.e. bytes - 1)
}

#[repr(C, packed)]
pub struct HbaCmdTable {
    pub cfis: [u8; 64],
    pub acmd: [u8; 16],
    pub reserved: [u8; 48],
    pub prdt_entry: [HbaPrdtEntry; 1],
}

#[repr(C, packed)]
pub struct FisRegH2D {
    pub fis_type: u8,
    pub pm_port_c: u8,
    pub command: u8,
    pub feature_low: u8,
    pub lba0: u8,
    pub lba1: u8,
    pub lba2: u8,
    pub device: u8,
    pub lba3: u8,
    pub lba4: u8,
    pub lba5: u8,
    pub feature_high: u8,
    pub count_low: u8,
    pub count_high: u8,
    pub icc: u8,
    pub control: u8,
    pub reserved: [u8; 4],
}

pub fn check_port_type(port: &HbaPort) -> bool {
    let ssts = port.ssts.read();
    let ipm = (ssts >> 8) & 0x0F;
    let det = ssts & 0x0F;

    if det == HBA_PORT_DET_PRESENT && ipm == HBA_PORT_IPM_ACTIVE {
        let sig = port.sig.read();
        sig == SATA_SIG_ATA
    } else {
        false
    }
}

pub struct AhciPortState {
    pub port_index: usize,
    pub cmd_list_phys: u64,
    pub cmd_list_virt: *mut HbaCmdHeader,
    pub fis_phys: u64,
    pub fis_virt: *mut u8,
    pub cmd_table_phys: u64,
    pub cmd_table_virt: *mut HbaCmdTable,
    pub bounce_buffer_phys: u64,
    pub bounce_buffer_virt: *mut u8,
}

pub struct AhciPortBuffers {
    pub cmd_list_phys: u64,
    pub cmd_list_virt: *mut HbaCmdHeader,
    pub fis_phys: u64,
    pub fis_virt: *mut u8,
    pub cmd_table_phys: u64,
    pub cmd_table_virt: *mut HbaCmdTable,
    pub bounce_buffer_phys: u64,
    pub bounce_buffer_virt: *mut u8,
}

pub unsafe fn allocate_port_buffers(phys_offset: u64) -> AhciPortBuffers {
    let cmd_list_buf = alloc::vec![0u8; 1024].leak();
    let cmd_list_virt = cmd_list_buf.as_mut_ptr() as *mut HbaCmdHeader;
    let cmd_list_phys = if (cmd_list_virt as u64) >= phys_offset { (cmd_list_virt as u64) - phys_offset } else { cmd_list_virt as u64 };

    let fis_buf = alloc::vec![0u8; 256].leak();
    let fis_virt = fis_buf.as_mut_ptr();
    let fis_phys = if (fis_virt as u64) >= phys_offset { (fis_virt as u64) - phys_offset } else { fis_virt as u64 };

    let cmd_table_buf = alloc::vec![0u8; 256].leak();
    let cmd_table_virt = cmd_table_buf.as_mut_ptr() as *mut HbaCmdTable;
    let cmd_table_phys = if (cmd_table_virt as u64) >= phys_offset { (cmd_table_virt as u64) - phys_offset } else { cmd_table_virt as u64 };

    let bounce_buf = alloc::vec![0u8; 64 * 1024].leak();
    let bounce_buffer_virt = bounce_buf.as_mut_ptr();
    let bounce_buffer_phys = if (bounce_buffer_virt as u64) >= phys_offset { (bounce_buffer_virt as u64) - phys_offset } else { bounce_buffer_virt as u64 };

    AhciPortBuffers {
        cmd_list_phys,
        cmd_list_virt,
        fis_phys,
        fis_virt,
        cmd_table_phys,
        cmd_table_virt,
        bounce_buffer_phys,
        bounce_buffer_virt,
    }
}
