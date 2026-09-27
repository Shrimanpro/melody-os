use volatile::Volatile;

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
