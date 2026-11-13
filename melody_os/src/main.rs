#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use melody_os::audio::hda::HdaController;
use melody_os::audio::player::{Library, Player, PLAYER};
use melody_os::vga_buffer::{print_colored, Color};
use melody_os::{fs, net, pci, print, println, ui, usb};

entry_point!(kernel_main);

/// Print the name of a boot step *before* doing it, so if the machine hangs the last line
/// on screen says exactly where.
fn step(name: &str) {
    print!("  {:<28}", name);
}

fn ok(detail: &str) {
    print_colored(Color::LightGreen, format_args!("[ OK ] "));
    println!("{}", detail);
}

fn warn(detail: &str) {
    print_colored(Color::Yellow, format_args!("[WARN] "));
    println!("{}", detail);
}

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    print_colored(Color::LightCyan, format_args!("Melody OS {} - booting\n\n", env!("CARGO_PKG_VERSION")));

    step("CPU, interrupts, timer");
    melody_os::init_with_memory(boot_info);
    let mhz = melody_os::interrupts::TSC_CYCLES_PER_MS.load(core::sync::atomic::Ordering::Relaxed) / 1000;
    if melody_os::timer_works() {
        ok(&alloc::format!("{} MHz, heap {} MiB", mhz, melody_os::allocator::HEAP_SIZE >> 20));
    } else {
        warn(&alloc::format!("{} MHz, PIT timer not ticking (will busy-poll)", mhz));
    }

    step("PCI bus");
    let pci_devices = pci::scan_pci_bus();
    ok(&alloc::format!("{} devices", pci_devices.len()));
    for dev in &pci_devices {
        melody_os::serial_println!("  pci {}", dev);
    }

    // Sound first: it never touches storage and tells us which outputs exist.
    step("Sound (Intel HDA)");
    let mut hdas: Vec<HdaController> = Vec::new();
    let hda_devs = pci::find_hda_controllers(&pci_devices);
    if hda_devs.is_empty() {
        warn("no HD Audio controller found");
    } else {
        println!();
        for dev in &hda_devs {
            match unsafe { HdaController::new(dev) } {
                Ok(h) => {
                    let outs: Vec<&str> = h.available_outputs().iter().map(|o| o.name()).collect();
                    print!("    {} ", dev.address);
                    ok(&outs.join(", "));
                    hdas.push(h);
                }
                Err(e) => {
                    print!("    {} ", dev.address);
                    warn(e);
                }
            }
        }
    }

    step("Graphics (HDMI)");
    match melody_os::gpu::init(&pci_devices) {
        Some(line) => ok(&line),
        None => warn("no Intel G4x graphics (HDMI audio unavailable)"),
    }

    step("USB controllers and drives");
    println!();
    let drives = usb::scan(&pci_devices);
    let usb_drives: Vec<String> = drives.iter().map(|d| alloc::format!("{} {}MB", d.model, d.capacity_mb)).collect();
    step("  USB summary");
    ok(&alloc::format!("{} storage drive(s), {} keyboard(s)", drives.len(), usb::keyboard_count()));

    step("Music library");
    let mut library: Option<Library> = None;
    let mut storage_desc = String::from("no USB stick");
    let mut why = String::new();
    for mut drive in drives {
        match fs::find_fat_volume(&mut drive) {
            Ok(mut fat) => {
                let tracks = fs::scan_library(&mut fat, &mut drive);
                let label = if fat.label.is_empty() || fat.label == "NO NAME" { drive.model.clone() } else { fat.label.clone() };
                let better = library.as_ref().map(|l| l.tracks.is_empty() && !tracks.is_empty()).unwrap_or(true);
                if better {
                    storage_desc = alloc::format!("{} ({})", label, fat.type_name());
                    library = Some(Library { drive, fs: fat, tracks });
                }
            }
            // The boot stick has no FAT32 partition; only report problems if nothing works,
            // and prefer messages that say how to fix the stick.
            Err(e) => {
                if why.is_empty() || e.contains("reformat") {
                    why = alloc::format!("{}: {}", drive.model, e);
                }
            }
        }
    }
    match &library {
        Some(lib) if !lib.tracks.is_empty() => ok(&alloc::format!("{} tracks on {}", lib.tracks.len(), storage_desc)),
        Some(_) => warn(&alloc::format!("{} has no .wav/.flac/.mp3 files", storage_desc)),
        None if !why.is_empty() => warn(&why),
        None => warn("no USB storage found"),
    }

    step("Network + MPD server");
    match net::init(&pci_devices) {
        Ok(desc) => ok(&desc),
        Err(e) => warn(e),
    }

    let have_tracks = library.as_ref().map(|l| !l.tracks.is_empty()).unwrap_or(false);
    let player = Player::new(hdas, library);
    *PLAYER.lock() = Some(player);

    println!("\nStarting player... (press L in the player to see this log again)");
    melody_os::interrupts::delay_ms(3000); // leave the boot log readable for a moment
    ui::start(ui::BootInfo { storage: storage_desc, cpu_mhz: mhz, usb_drives });
    if have_tracks {
        ui::notify(String::from("Ready. Pick a track and press ENTER, or use an MPD client on port 6600"));
    } else if !why.is_empty() {
        ui::notify(why);
    }
    melody_os::run();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    melody_os::interrupts::fatal(format_args!("\nKERNEL PANIC: {}", info));
}
