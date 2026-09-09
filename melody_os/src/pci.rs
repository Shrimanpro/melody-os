use x86_64::instructions::port::Port;

pub const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
pub const PCI_CONFIG_DATA: u16 = 0xCFC;

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
