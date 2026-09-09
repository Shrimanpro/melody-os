#![no_std]
#![no_main]

mod vga_buffer;

use core::panic::PanicInfo;


#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! 
{
    println!("wow using macro{}", "!");
    panic!("oh no");
    loop {}
}


#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    loop {}
}
