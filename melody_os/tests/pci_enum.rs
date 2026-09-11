#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(melody_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use melody_os::pci;

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    use melody_os::allocator;
    use melody_os::memory::{self, BootInfoFrameAllocator};
    use x86_64::VirtAddr;

    melody_os::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap initialization failed");

    test_main();
    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    melody_os::test_panic_handler(info)
}

#[test_case]
fn test_pci_scan_finds_devices() {
    let devices = pci::scan_pci_bus();
    assert!(!devices.is_empty(), "PCI bus scan returned no devices");

    // In QEMU i440FX/PIIX, device 00:00.0 is the Host Bridge (Intel 0x8086:0x1237 or Q35 0x29c0)
    let host_bridge = devices.iter().find(|d| d.address.bus == 0 && d.address.device == 0 && d.address.function == 0);
    assert!(host_bridge.is_some(), "Host bridge 00:00.0 was not found on PCI bus");
}
