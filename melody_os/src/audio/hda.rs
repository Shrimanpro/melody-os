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

pub struct HdaController {
    base_virt: *mut u8,
}

impl HdaController {
    pub unsafe fn reset_controller(base_virt: *mut u8) {
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
}
