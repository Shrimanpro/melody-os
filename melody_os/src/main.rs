#![no_std]
#![no_main]

mod vga_buffer;

use core::panic::PanicInfo;


#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! 
{
    println!("wow using macro{}", "!");
    loop {}
}


#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
