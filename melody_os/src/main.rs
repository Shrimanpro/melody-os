#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(melody_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

mod vga_buffer;

use core::panic::PanicInfo;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("wow using macro{}", "!");

    melody_os::init();

    #[cfg(test)]
    test_main();

    melody_os::hlt_loop();
}

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
