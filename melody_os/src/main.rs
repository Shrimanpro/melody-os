#![no_std]
#![no_main]

use core::panic::PanicInfo;
use core::arch::global_asm;
use core::fmt;

// prevents crashing
global_asm!(
    ".section .text._start",
    ".global _start",    
    "_start:",
    "mrs x1, mpidr_el1",
    "and x1, x1, #3",
    "cbz x1, 2f",
    "1: wfe",
    "b 1b",
    "2:",
    "ldr x1, =0x80000",
    "mov sp, x1",
    "b rust_main"
);

struct Dummy;

// hardware addresses
const MMIO_BASE: u32 = 0x3F00_0000;
const UART0_DR: *mut u32 = (MMIO_BASE + 0x0020_1000) as *mut u32;
const UART0_FR: *mut u32 = (MMIO_BASE + 0x0020_1018) as *mut u32;

fn putc(c: u8) {
    unsafe {
        while core::ptr::read_volatile(UART0_FR) & (1 << 5) != 0 {
            core::hint::spin_loop();
        }
        core::ptr::write_volatile(UART0_DR, c as u32);
    }
}

impl fmt::Write for Dummy {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' { 
                putc(b'\r'); 
            }
            putc(byte);
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) 
{
    use core::fmt::Write;
    Dummy.write_fmt(args).unwrap();
}

// macros
#[macro_export]
macro_rules! print 
{
    ($($arg:tt)*) => ($crate::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println 
{
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_main() -> ! {
    println!("\n==================================");
    println!(" Hello World from Melody OS!");
    println!("==================================\n");
    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("\n[!] KERNEL PANIC!");
    println!("{}", info);
    loop {}
}
