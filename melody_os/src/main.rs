#![no_std]
#![no_main]

use core::panic::PanicInfo;
use core::arch::global_asm;

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

fn puts(s: &str) {
    for byte in s.bytes() {
        if byte == b'\n' {
            putc(b'\r');
        }
        putc(byte);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_main() -> ! {
    puts("\n==================================\n");
    puts(" Hello World from Melody OS! \n");
    puts("==================================\n");

    loop {}
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    puts("\n[!] KERNEL PANIC!\n");
    loop {}
}
