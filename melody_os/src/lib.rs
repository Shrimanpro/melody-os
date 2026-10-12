#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![feature(abi_x86_interrupt)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

pub mod ahci;
pub mod allocator;
pub mod audio;
pub mod fs;
pub mod gdt;
pub mod interrupts;
pub mod memory;
pub mod net;
pub mod pci;
pub mod serial;
pub mod usb;
pub mod vga_buffer;

/// Set once we've seen the PIT tick; only then is it safe to `hlt` in the main loop.
static TIMER_WORKS: AtomicBool = AtomicBool::new(false);

pub fn init() {
    gdt::init();
    interrupts::init_idt();
    interrupts::flush_ps2_buffer();
    interrupts::calibrate_tsc();
    unsafe {
        let mut pics = interrupts::PICS.lock();
        pics.initialize();
        // Only IRQ 0 (timer) and IRQ 1 (keyboard). Everything else is polled.
        pics.write_masks(0xFC, 0xFF);
    }
    interrupts::init_pit();
    x86_64::instructions::interrupts::enable();

    let start = interrupts::TIMER_TICKS.load(Ordering::Relaxed);
    interrupts::delay_ms(20);
    TIMER_WORKS.store(interrupts::TIMER_TICKS.load(Ordering::Relaxed) > start, Ordering::Relaxed);
}

pub fn timer_works() -> bool {
    TIMER_WORKS.load(Ordering::Relaxed)
}

pub trait Testable {
    fn run(&self) -> ();
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error: {}\n", info);
    exit_qemu(QemuExitCode::Failed);
    hlt_loop();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

/// Initialise CPU tables, memory and the heap (shared by the kernel and test binaries).
pub fn init_with_memory(boot_info: &'static bootloader::BootInfo) {
    use x86_64::VirtAddr;
    init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { memory::BootInfoFrameAllocator::init(&boot_info.memory_map) };
    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap initialization failed");
    memory::register_frame_allocator(frame_allocator);
}

#[cfg(test)]
bootloader::entry_point!(test_kernel_main);

#[cfg(test)]
fn test_kernel_main(boot_info: &'static bootloader::BootInfo) -> ! {
    init_with_memory(boot_info);
    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}
