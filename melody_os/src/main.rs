#![no_std]
#![no_main]

mod vga_buffer;

use core::panic::PanicInfo;


#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! 
{
    use core::fmt::Write;
    vga_buffer::WRITER.lock().write_str("testing").unwrap();
    // write!(vga_buffer::WRITER.lock(), ", nums: {} {}", 42, 1.513).unwrap();

    loop {}
}


#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
