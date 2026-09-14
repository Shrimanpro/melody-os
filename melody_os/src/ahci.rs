use volatile::Volatile;

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
