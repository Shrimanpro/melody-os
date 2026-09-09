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
