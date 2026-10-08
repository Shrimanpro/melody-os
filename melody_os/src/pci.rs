use alloc::vec::Vec;
use core::fmt;
use x86_64::instructions::port::Port;

pub const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
pub const PCI_CONFIG_DATA: u16 = 0xCFC;

/// PCI Class Codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PciClass {
    Unclassified = 0x00,
    MassStorage = 0x01,
    Network = 0x02,
    Display = 0x03,
    Multimedia = 0x04,
    Memory = 0x05,
    Bridge = 0x06,
    SimpleComm = 0x07,
    BasePeripheral = 0x08,
    InputDevice = 0x09,
    DockingStation = 0x0A,
    Processor = 0x0B,
    SerialBus = 0x0C,
    Wireless = 0x0D,
    Intelligent = 0x0E,
    Satellite = 0x0F,
    Encryption = 0x10,
    SignalProcessing = 0x11,
    Other = 0xFF,
}

impl From<u8> for PciClass {
    fn from(value: u8) -> Self {
        match value {
            0x00 => PciClass::Unclassified,
            0x01 => PciClass::MassStorage,
            0x02 => PciClass::Network,
            0x03 => PciClass::Display,
            0x04 => PciClass::Multimedia,
            0x05 => PciClass::Memory,
            0x06 => PciClass::Bridge,
            0x07 => PciClass::SimpleComm,
            0x08 => PciClass::BasePeripheral,
            0x09 => PciClass::InputDevice,
            0x0A => PciClass::DockingStation,
            0x0B => PciClass::Processor,
            0x0C => PciClass::SerialBus,
            0x0D => PciClass::Wireless,
            0x0E => PciClass::Intelligent,
            0x0F => PciClass::Satellite,
            0x10 => PciClass::Encryption,
            0x11 => PciClass::SignalProcessing,
            _ => PciClass::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciAddress {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl fmt::Display for PciAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02x}:{:02x}.{:x}", self.bus, self.device, self.function)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    Memory32 { address: u32, size: u32, prefetchable: bool },
    Memory64 { address: u64, size: u64, prefetchable: bool },
    Io { port: u16 },
    Empty,
}

#[derive(Debug, Clone)]
pub struct PciDevice {
    pub address: PciAddress,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: PciClass,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    pub header_type: u8,
    pub bars: [Bar; 6],
}

impl PciDevice {
    pub unsafe fn read_u32(&self, offset: u8) -> u32 {
        unsafe { pci_read_32(self.address.bus, self.address.device, self.address.function, offset) }
    }

    pub unsafe fn read_u16(&self, offset: u8) -> u16 {
        unsafe { pci_read_16(self.address.bus, self.address.device, self.address.function, offset) }
    }

    pub unsafe fn read_u8(&self, offset: u8) -> u8 {
        unsafe { pci_read_8(self.address.bus, self.address.device, self.address.function, offset) }
    }

    pub unsafe fn write_u32(&self, offset: u8, value: u32) {
        unsafe { pci_write_32(self.address.bus, self.address.device, self.address.function, offset, value); }
    }

    pub unsafe fn write_u16(&self, offset: u8, value: u16) {
        unsafe { pci_write_16(self.address.bus, self.address.device, self.address.function, offset, value); }
    }

    pub unsafe fn write_u8(&self, offset: u8, value: u8) {
        unsafe { pci_write_8(self.address.bus, self.address.device, self.address.function, offset, value); }
    }

    /// Enable Memory Space and Bus Mastering (necessary for DMA).
    pub unsafe fn enable_bus_mastering(&self) {
        unsafe { self.set_power_d0() };
        let command = unsafe { self.read_u16(0x04) };
        // Bit 0: IO Space, Bit 1: Memory Space, Bit 2: Bus Master
        let new_command = command | 0x0007;
        unsafe { self.write_u16(0x04, new_command) };
    }

    /// Bring the device to full power (D0) if the BIOS left it in a sleep state.
    pub unsafe fn set_power_d0(&self) {
        let status = unsafe { self.read_u16(0x06) };
        if status & (1 << 4) == 0 {
            return; // no capability list
        }
        let mut ptr = unsafe { self.read_u8(0x34) } & 0xFC;
        for _ in 0..48 {
            if ptr < 0x40 {
                return;
            }
            let id = unsafe { self.read_u8(ptr) };
            if id == 0x01 {
                let pmcsr = unsafe { self.read_u16(ptr + 4) };
                if pmcsr & 0x3 != 0 {
                    unsafe { self.write_u16(ptr + 4, pmcsr & !0x3) };
                    // D3hot -> D0 needs 10 ms before the device may be touched.
                    crate::interrupts::delay_ms(10);
                }
                return;
            }
            ptr = unsafe { self.read_u8(ptr + 1) } & 0xFC;
        }
    }
}

impl fmt::Display for PciDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [{:04x}:{:04x}] Class: {:?} (Subclass: 0x{:02x}, ProgIF: 0x{:02x})",
            self.address, self.vendor_id, self.device_id, self.class, self.subclass, self.prog_if
        )
    }
}

pub unsafe fn pci_read_32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address = (1u32 << 31)
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC);

    let mut config_address = Port::<u32>::new(PCI_CONFIG_ADDRESS);
    let mut config_data = Port::<u32>::new(PCI_CONFIG_DATA);

    unsafe {
        config_address.write(address);
        config_data.read()
    }
}

pub unsafe fn pci_read_16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let dword = unsafe { pci_read_32(bus, device, function, offset) };
    ((dword >> ((offset & 2) * 8)) & 0xFFFF) as u16
}

pub unsafe fn pci_read_8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let dword = unsafe { pci_read_32(bus, device, function, offset) };
    ((dword >> ((offset & 3) * 8)) & 0xFF) as u8
}

pub unsafe fn pci_write_32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let address = (1u32 << 31)
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC);

    let mut config_address = Port::<u32>::new(PCI_CONFIG_ADDRESS);
    let mut config_data = Port::<u32>::new(PCI_CONFIG_DATA);

    unsafe {
        config_address.write(address);
        config_data.write(value);
    }
}

pub unsafe fn pci_write_16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    let shift = (offset & 2) * 8;
    let old = unsafe { pci_read_32(bus, device, function, offset) };
    let new = (old & !(0xFFFF << shift)) | ((value as u32) << shift);
    unsafe { pci_write_32(bus, device, function, offset, new) };
}

pub unsafe fn pci_write_8(bus: u8, device: u8, function: u8, offset: u8, value: u8) {
    let shift = (offset & 3) * 8;
    let old = unsafe { pci_read_32(bus, device, function, offset) };
    let new = (old & !(0xFF << shift)) | ((value as u32) << shift);
    unsafe { pci_write_32(bus, device, function, offset, new) };
}

unsafe fn read_bar(bus: u8, device: u8, function: u8, bar_index: usize) -> (Bar, bool) {
    let offset = 0x10 + (bar_index as u8) * 4;
    let original = unsafe { pci_read_32(bus, device, function, offset) };
    if original == 0 || original == 0xFFFFFFFF {
        return (Bar::Empty, false);
    }

    if (original & 1) == 1 {
        // I/O space BAR
        let port = (original & !0x3) as u16;
        (Bar::Io { port }, false)
    } else {
        // Memory space BAR
        let bar_type = (original >> 1) & 0x3;
        let prefetchable = ((original >> 3) & 1) == 1;

        if bar_type == 0b10 {
            // 64-bit BAR
            let original_high = unsafe { pci_read_32(bus, device, function, offset + 4) };
            let address = ((original_high as u64) << 32) | ((original & !0xF) as u64);

            // Size determination
            unsafe {
                pci_write_32(bus, device, function, offset, 0xFFFFFFFF);
                pci_write_32(bus, device, function, offset + 4, 0xFFFFFFFF);
            }
            let size_mask_low = unsafe { pci_read_32(bus, device, function, offset) };
            let size_mask_high = unsafe { pci_read_32(bus, device, function, offset + 4) };
            unsafe {
                pci_write_32(bus, device, function, offset, original);
                pci_write_32(bus, device, function, offset + 4, original_high);
            }

            let mask = ((size_mask_high as u64) << 32) | ((size_mask_low & !0xF) as u64);
            let size = if mask != 0 { (!mask).wrapping_add(1) } else { 0 };

            (Bar::Memory64 { address, size, prefetchable }, true)
        } else {
            // 32-bit BAR
            let address = original & !0xF;

            unsafe { pci_write_32(bus, device, function, offset, 0xFFFFFFFF) };
            let size_mask = unsafe { pci_read_32(bus, device, function, offset) };
            unsafe { pci_write_32(bus, device, function, offset, original) };

            let mask = size_mask & !0xF;
            let size = if mask != 0 { (!mask).wrapping_add(1) } else { 0 };

            (Bar::Memory32 { address, size, prefetchable }, false)
        }
    }
}

pub fn scan_pci_bus() -> Vec<PciDevice> {
    let mut devices = Vec::new();

    let host_bridge_vendor = unsafe { pci_read_16(0, 0, 0, 0x00) };
    let host_bridge_device = unsafe { pci_read_16(0, 0, 0, 0x02) };

    for bus in 0..=31 {
        // If bus > 0 and host bridge is valid, check if this bus aliases bus 0
        if bus > 0 && host_bridge_vendor != 0xFFFF {
            let b0_vendor = unsafe { pci_read_16(bus, 0, 0, 0x00) };
            let b0_device = unsafe { pci_read_16(bus, 0, 0, 0x02) };
            if b0_vendor == host_bridge_vendor && b0_device == host_bridge_device {
                // Non-existent secondary bus aliased directly to bus 0 by chipset
                continue;
            }
        }

        for device in 0..32 {
            for function in 0..8 {
                let func_vendor_id = unsafe { pci_read_16(bus, device, function, 0x00) };
                if func_vendor_id == 0xFFFF {
                    continue;
                }

                let device_id = unsafe { pci_read_16(bus, device, function, 0x02) };
                let class_subclass = unsafe { pci_read_16(bus, device, function, 0x0A) };
                let class = PciClass::from((class_subclass >> 8) as u8);
                let subclass = (class_subclass & 0xFF) as u8;
                let prog_if = unsafe { pci_read_8(bus, device, function, 0x09) };
                let revision = unsafe { pci_read_8(bus, device, function, 0x08) };
                let func_header_type = unsafe { pci_read_8(bus, device, function, 0x0E) } & 0x7F;

                // Check for duplicate device from aliased bus
                let is_duplicate = devices.iter().any(|d: &PciDevice| {
                    d.vendor_id == func_vendor_id
                        && d.device_id == device_id
                        && d.class == class
                        && d.subclass == subclass
                        && d.address.device == device
                        && d.address.function == function
                });
                if is_duplicate {
                    continue;
                }

                let mut bars = [Bar::Empty; 6];
                if func_header_type == 0x00 {
                    let mut i = 0;
                    while i < 6 {
                        let (bar, is_64) = unsafe { read_bar(bus, device, function, i) };
                        bars[i] = bar;
                        if is_64 {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                }

                devices.push(PciDevice {
                    address: PciAddress { bus, device, function },
                    vendor_id: func_vendor_id,
                    device_id,
                    class,
                    subclass,
                    prog_if,
                    revision,
                    header_type: func_header_type,
                    bars,
                });
            }
        }
    }

    devices
}

/// Find all Intel High Definition Audio controllers (class 04, subclass 03).
/// Find the SATA AHCI Controller (Class 0x01, Subclass 0x06, ProgIF 0x01)
pub fn find_ahci_controller(devices: &[PciDevice]) -> Option<PciDevice> {
    devices.iter().find(|d| {
        d.class == PciClass::MassStorage && d.subclass == 0x06 && d.prog_if == 0x01
    }).cloned()
}

pub fn find_hda_controllers(devices: &[PciDevice]) -> Vec<PciDevice> {
    devices.iter().filter(|d| d.class == PciClass::Multimedia && d.subclass == 0x03).cloned().collect()
}

/// Find the first Ethernet controller (class 02, subclass 00).
pub fn find_network_controller(devices: &[PciDevice]) -> Option<PciDevice> {
    devices.iter().find(|d| d.class == PciClass::Network && d.subclass == 0x00).cloned()
}

/// Find USB host controllers by programming interface: 0x00 UHCI, 0x10 OHCI, 0x20 EHCI, 0x30 xHCI.
pub fn find_usb_controllers(devices: &[PciDevice], prog_if: u8) -> Vec<PciDevice> {
    devices
        .iter()
        .filter(|d| d.class == PciClass::SerialBus && d.subclass == 0x03 && d.prog_if == prog_if)
        .cloned()
        .collect()
}
