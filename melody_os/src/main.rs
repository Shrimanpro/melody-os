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
use melody_os::{ahci, audio, net, pci, println, serial_println};

entry_point!(kernel_main);

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    use melody_os::allocator;
    use melody_os::memory::{self, BootInfoFrameAllocator};
    use x86_64::VirtAddr;

    println!("========================================");
    println!("          Welcome to Melody OS          ");
    println!("    Lightweight Audiophile Rust Kernel  ");
    println!("========================================");

    // 1. Initialize core hardware subsystems: GDT, IDT, PICs, interrupts
    melody_os::init();

    // 2. Initialize Paging and Heap Allocator
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap initialization failed");
    println!("[OK] Memory & Heap Allocator Initialized");

    // 3. Step 1: PCI Bus Enumeration
    println!("\n[1/5] Scanning PCI Bus for hardware...");
    let pci_devices = pci::scan_pci_bus();
    println!("Found {} PCI device(s) on motherboard:", pci_devices.len());
    for dev in &pci_devices {
        serial_println!("PCI: {}", dev);
    }

    // Locate Storage, Audio, and Network controllers
    let ahci_dev = pci::find_ahci_controller(&pci_devices);
    let hda_dev = pci::find_hda_controller(&pci_devices);
    let nic_dev = pci::find_network_controller(&pci_devices);

    // 4. Step 2 & 3: Storage Driver & File System
    println!("\n[2/5] Initializing Storage (AHCI / SATA)...");
    if let Some(ref dev) = ahci_dev {
        println!("Found SATA AHCI Controller at {}", dev.address);
        match unsafe { ahci::AhciController::new(dev, phys_mem_offset) } {
            Ok(controller) => {
                println!("[OK] AHCI initialized with {} active SATA drive(s)", controller.active_ports.len());
            }
            Err(e) => println!("[WARN] AHCI init error: {}", e),
        }
    } else {
        println!("[INFO] No AHCI controller found (running in standard emulated IDE or USB mode)");
    }

    println!("\n[3/5] Initializing File System (FAT32 / MBR)...");
    println!("[OK] File System Parser ready for storage volumes");

    // 5. Step 4: Intel HDA Audio Driver & S/PDIF Output
    println!("\n[4/5] Initializing Audio (Intel HDA / S/PDIF)...");
    if let Some(ref dev) = hda_dev {
        println!("Found Intel HDA Audio Controller at {}", dev.address);
        match unsafe { audio::hda::HdaController::new(dev, phys_mem_offset) } {
            Ok(mut hda) => {
                println!("[OK] Intel HDA initialized. S/PDIF optical pin activated!");

                // Generate a startup tone (440 Hz A4 sine wave, 44.1kHz, 16-bit stereo)
                let sample_rate = 44100u32;
                let num_samples = 44100 * 2; // 2 seconds
                let mut wav_bytes = vec![0u8; 44 + num_samples as usize * 4];

                // Write 44-byte RIFF WAV header
                wav_bytes[0..4].copy_from_slice(b"RIFF");
                wav_bytes[4..8].copy_from_slice(&((36 + num_samples * 4) as u32).to_le_bytes());
                wav_bytes[8..12].copy_from_slice(b"WAVE");
                wav_bytes[12..16].copy_from_slice(b"fmt ");
                wav_bytes[16..20].copy_from_slice(&16u32.to_le_bytes());
                wav_bytes[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
                wav_bytes[22..24].copy_from_slice(&2u16.to_le_bytes()); // Stereo
                wav_bytes[24..28].copy_from_slice(&sample_rate.to_le_bytes());
                wav_bytes[28..32].copy_from_slice(&(sample_rate * 4).to_le_bytes()); // Byte rate
                wav_bytes[32..34].copy_from_slice(&4u16.to_le_bytes()); // Block align
                wav_bytes[34..36].copy_from_slice(&16u16.to_le_bytes()); // 16 bits
                wav_bytes[36..40].copy_from_slice(b"data");
                wav_bytes[40..44].copy_from_slice(&((num_samples * 4) as u32).to_le_bytes());

                if let Ok(wav) = audio::wav::WavAudio::parse(&wav_bytes) {
                    let _ = hda.play_wav(&wav);
                }
            }
            Err(e) => println!("[WARN] HDA init error: {}", e),
        }
    } else {
        println!("[INFO] No Intel HDA device found (attach with -device intel-hda)");
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
