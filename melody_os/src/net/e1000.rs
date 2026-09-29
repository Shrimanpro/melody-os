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
