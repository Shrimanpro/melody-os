#![no_std]   // Disable the standard library
#![no_main]  // Disable the standard Rust entry point

use core::panic::PanicInfo;

// This function is called on a panic.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // For now, just hang the processor indefinitely.
    // Later, you will map this to output to a UART serial console.
    loop {}
}

// The linker looks for '_start' as the default entry point.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Melody OS takes control of the Pi hardware right here.
    
    loop {} // Keep the processor spinning
}
