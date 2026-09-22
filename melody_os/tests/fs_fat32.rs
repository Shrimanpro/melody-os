#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(melody_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use melody_os::fs::{BlockDevice, MasterBootRecord};

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

#[allow(dead_code)]
struct MemoryBlockDevice {
    data: Vec<[u8; 512]>,
}

impl BlockDevice for MemoryBlockDevice {
    fn read_sector(&mut self, lba: u64, buffer: &mut [u8; 512]) -> Result<(), &'static str> {
        if (lba as usize) < self.data.len() {
            *buffer = self.data[lba as usize];
            Ok(())
        } else {
            Err("LBA out of bounds")
        }
    }
}

#[test_case]
fn test_mbr_parsing() {
    let mut sector = [0u8; 512];
    sector[510] = 0x55;
    sector[511] = 0xAA;

    // Partition 1 at 446 (0x1BE)
    let p1 = 446;
    sector[p1] = 0x80; // Bootable
    sector[p1 + 4] = 0x0C; // FAT32 with LBA
    // LBA start = 2048 (0x00000800)
    sector[p1 + 8] = 0x00;
    sector[p1 + 9] = 0x08;
    sector[p1 + 10] = 0x00;
    sector[p1 + 11] = 0x00;
    // Sector count = 65536
    sector[p1 + 12] = 0x00;
    sector[p1 + 13] = 0x00;
    sector[p1 + 14] = 0x01;
    sector[p1 + 15] = 0x00;

    let mbr = MasterBootRecord::parse(&sector).expect("Failed to parse valid MBR");
    let part1 = mbr.partitions[0].expect("Partition 0 missing");
    assert_eq!(part1.status, 0x80);
    assert_eq!(part1.partition_type, 0x0C);
    assert_eq!(part1.lba_start, 2048);
    assert_eq!(part1.sector_count, 65536);
    assert!(mbr.partitions[1].is_none());
}
