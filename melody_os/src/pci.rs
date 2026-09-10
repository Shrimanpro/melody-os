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

    /// Enable Memory Space and Bus Mastering (necessary for DMA).
    pub unsafe fn enable_bus_mastering(&self) {
        let command = unsafe { self.read_u16(0x04) };
        // Bit 0: IO Space, Bit 1: Memory Space, Bit 2: Bus Master
        let new_command = command | 0x0007;
        unsafe { self.write_u16(0x04, new_command) };
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

/// Enable Memory Space and Bus Mastering (necessary for DMA).
pub unsafe fn enable_bus_mastering(bus: u8, device: u8, function: u8) {
    let command = unsafe { pci_read_16(bus, device, function, 0x04) };
    // Bit 0: IO Space, Bit 1: Memory Space, Bit 2: Bus Master
    let new_command = command | 0x0007;
    unsafe { pci_write_16(bus, device, function, 0x04, new_command) };
}
