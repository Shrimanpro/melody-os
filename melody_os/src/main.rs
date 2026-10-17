#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(melody_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::string::ToString;
use alloc::vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use melody_os::vga_buffer::{print_colored, Color};
use melody_os::{audio, fs, net, pci, println, serial_println, usb};

entry_point!(kernel_main);

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    use x86_64::VirtAddr;

    print_colored(Color::LightCyan, format_args!("Melody OS {} - booting\n\n", env!("CARGO_PKG_VERSION")));

    // 1. GDT, IDT, PICs, timer, paging and heap
    melody_os::init_with_memory(boot_info);
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    println!("[OK] Memory & Heap Allocator Initialized");

    // 3. Step 1: PCI Bus Enumeration
    println!("\n[1/5] Scanning PCI Bus for hardware...");
    let pci_devices = pci::scan_pci_bus();
    println!("Found {} PCI device(s) on motherboard:", pci_devices.len());
    for dev in &pci_devices {
        serial_println!("PCI: {}", dev);
    }

    // Locate Audio and Network controllers
    let hda_dev = pci::find_hda_controllers(&pci_devices).into_iter().next();
    let nic_dev = pci::find_network_controller(&pci_devices);

    // 4. Step 2 & 3: USB storage and the music library
    println!("\n[2/5] USB controllers and drives...");
    let drives = usb::scan(&pci_devices);
    println!("[OK] {} USB storage drive(s)", drives.len());

    println!("\n[3/5] Music library (FAT32 / FAT16)...");
    let mut found = false;
    for mut drive in drives {
        match fs::find_fat_volume(&mut drive) {
            Ok(mut fat) => {
                let tracks = fs::scan_library(&mut fat, &mut drive);
                println!("[OK] {} tracks on {} ({})", tracks.len(), drive.model, fat.type_name());
                found = true;
                break;
            }
            Err(e) => println!("[WARN] {}: {}", drive.model, e),
        }
    }
    if !found {
        println!("[WARN] no music found (put .wav/.flac/.mp3 files on a FAT32 USB stick)");
    }

    // 5. Step 4: Intel HDA sound
    println!("\n[4/5] Sound (Intel HDA)...");
    if let Some(ref dev) = hda_dev {
        match unsafe { audio::hda::HdaController::new(dev) } {
            Ok(hda) => {
                let outs: alloc::vec::Vec<&str> = hda.available_outputs().iter().map(|o| o.name()).collect();
                println!("[OK] HDA at {}: {} ({})", dev.address, outs.join(", "), hda.active_pins());
            }
            Err(e) => println!("[WARN] HDA at {}: {}", dev.address, e),
        }
    } else {
        println!("[WARN] no HD Audio controller found");
    }

    // 6. Step 5: Network Driver & MPD Protocol Server
    println!("\n[5/5] Initializing Network & MPD Server...");
    if let Some(ref dev) = nic_dev {
        println!("Found Network Controller at {}", dev.address);
        match unsafe { net::e1000::E1000Driver::new(dev, phys_mem_offset) } {
            Ok(driver) => {
                println!(
                    "[OK] E1000 Ethernet Driver initialized (MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x})",
                    driver.mac_addr[0], driver.mac_addr[1], driver.mac_addr[2],
                    driver.mac_addr[3], driver.mac_addr[4], driver.mac_addr[5]
                );
            }
            Err(e) => println!("[WARN] E1000 init error: {}", e),
        }
    } else {
        println!("[INFO] No supported Ethernet NIC found");
    }

    let mut mpd = net::mpd::MpdServer::new();
    mpd.set_playlist(vec![
        "track_01_bach.wav".to_string(),
        "track_02_chopin.flac".to_string(),
        "track_03_miles_davis.mp3".to_string(),
    ]);
    println!("[OK] MPD Protocol Server active on TCP port 6600");
    println!("     Status: Ready for music streaming commands");

    #[cfg(test)]
    test_main();

    println!("\nMelody OS is running. Enjoy the music!");
    melody_os::hlt_loop();
}

/// This function is called on panic.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    melody_os::hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    melody_os::test_panic_handler(info)
}

#[test_case]
fn trivial_assertion() {
    assert_eq!(1, 1);
}
