#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(melody_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::string::ToString;
use alloc::vec;
use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use melody_os::net::mpd::{MpdPlaybackState, MpdServer};

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
fn test_mpd_commands() {
    let mut server = MpdServer::new();
    assert_eq!(server.welcome_banner(), "OK MPD 0.23.5\n");

    // Ping
    let res = server.handle_command("ping");
    assert_eq!(res, "OK\n");

    // Set playlist
    server.set_playlist(vec!["track_01.wav".to_string(), "track_02.flac".to_string()]);

    // Lsinfo
    let res = server.handle_command("lsinfo");
    assert!(res.contains("file: track_01.wav"));
    assert!(res.contains("file: track_02.flac"));
    assert!(res.ends_with("OK\n"));

    // Status before play
    let res = server.handle_command("status");
    assert!(res.contains("state: stop"));

    // Play
    let res = server.handle_command("play");
    assert_eq!(res, "OK\n");
    assert_eq!(server.state, MpdPlaybackState::Play);

    // Outputs
    let res = server.handle_command("outputs");
    assert!(res.contains("S/PDIF Optical"));

    // Stop
    let res = server.handle_command("stop");
    assert_eq!(res, "OK\n");
    assert_eq!(server.state, MpdPlaybackState::Stop);
}
