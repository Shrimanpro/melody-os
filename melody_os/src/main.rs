#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

mod vga_buffer;

use core::panic::PanicInfo;

#[cfg(test)]
pub fn test_runner(_tests: &[&dyn Fn()]) {}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("wow using macro{}", "!");
    panic!("oh no");
    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    loop {}
}
