//! IDT, legacy PIC, PIT tick and TSC-based timing.
//!
//! Interrupt handlers here never take a lock that the main loop also takes: the timer
//! handler only bumps a counter and the keyboard handler only pushes raw scancodes into a
//! lock-free queue. Everything else (UI, audio, network) runs in the main loop.

use crate::gdt;
use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use pic8259::ChainedPics;
use x86_64::instructions::port::Port;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

const TIMER_VECTOR: u8 = PIC_1_OFFSET;
const KEYBOARD_VECTOR: u8 = PIC_1_OFFSET + 1;

pub static PICS: spin::Mutex<ChainedPics> =
    spin::Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.general_protection_fault.set_handler_fn(gpf_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt[TIMER_VECTOR].set_handler_fn(timer_interrupt_handler);
        idt[KEYBOARD_VECTOR].set_handler_fn(keyboard_interrupt_handler);
        for i in (PIC_1_OFFSET + 2)..(PIC_2_OFFSET + 8) {
            idt[i].set_handler_fn(spurious_irq_handler);
        }
        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

/// Print a fatal error on screen and serial, then stop the CPU for good.
pub fn fatal(args: core::fmt::Arguments) -> ! {
    x86_64::instructions::interrupts::disable();
    crate::vga_buffer::force_print(args);
    unsafe { crate::serial::SERIAL1.force_unlock() };
    crate::serial_println!("{}", args);
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    fatal(format_args!("\nFATAL: invalid opcode at {:?}", stack_frame.instruction_pointer));
}

extern "x86-interrupt" fn gpf_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    fatal(format_args!(
        "\nFATAL: general protection fault (code {:#x}) at {:?}",
        error_code, stack_frame.instruction_pointer
    ));
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;
    fatal(format_args!(
        "\nFATAL: page fault accessing {:?} ({:?}) at {:?}",
        Cr2::read(),
        error_code,
        stack_frame.instruction_pointer
    ));
}

extern "x86-interrupt" fn double_fault_handler(stack_frame: InterruptStackFrame, _error_code: u64) -> ! {
    fatal(format_args!("\nFATAL: double fault at {:?}", stack_frame.instruction_pointer));
}

// ---------------------------------------------------------------------------
// PIT tick (1 kHz) and keyboard queue
// ---------------------------------------------------------------------------

pub static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    TIMER_TICKS.fetch_add(1, Ordering::Relaxed);
    unsafe { PICS.lock().notify_end_of_interrupt(TIMER_VECTOR) };
}

const KEY_QUEUE_LEN: usize = 64;
static KEY_QUEUE: [AtomicU8; KEY_QUEUE_LEN] = [const { AtomicU8::new(0) }; KEY_QUEUE_LEN];
static KEY_HEAD: AtomicUsize = AtomicUsize::new(0); // written by the ISR
static KEY_TAIL: AtomicUsize = AtomicUsize::new(0); // written by the main loop

fn push_scancode(code: u8) {
    let head = KEY_HEAD.load(Ordering::Relaxed);
    let next = (head + 1) % KEY_QUEUE_LEN;
    if next == KEY_TAIL.load(Ordering::Acquire) {
        return; // queue full, drop the key
    }
    KEY_QUEUE[head].store(code, Ordering::Relaxed);
    KEY_HEAD.store(next, Ordering::Release);
}

/// Pop one raw PS/2 scancode queued by the keyboard interrupt.
pub fn pop_scancode() -> Option<u8> {
    let tail = KEY_TAIL.load(Ordering::Relaxed);
    if tail == KEY_HEAD.load(Ordering::Acquire) {
        return None;
    }
    let code = KEY_QUEUE[tail].load(Ordering::Relaxed);
    KEY_TAIL.store((tail + 1) % KEY_QUEUE_LEN, Ordering::Release);
    Some(code)
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let mut status = Port::<u8>::new(0x64);
    let mut data = Port::<u8>::new(0x60);
    // Bound the loop: some USB-legacy emulations keep the status bit stuck.
    for _ in 0..16 {
        if unsafe { status.read() } & 0x01 == 0 {
            break;
        }
        push_scancode(unsafe { data.read() });
    }
    unsafe { PICS.lock().notify_end_of_interrupt(KEYBOARD_VECTOR) };
}

extern "x86-interrupt" fn spurious_irq_handler(_stack_frame: InterruptStackFrame) {
    // All other IRQ lines are masked; if one fires anyway, acknowledge both PICs.
    unsafe {
        Port::<u8>::new(0xA0).write(0x20);
        Port::<u8>::new(0x20).write(0x20);
    }
}

/// Drain stale bytes from the 8042 output buffer without sending it any commands
/// (machines with only a USB keyboard may not answer them).
pub fn flush_ps2_buffer() {
    let mut status = Port::<u8>::new(0x64);
    let mut data = Port::<u8>::new(0x60);
    for _ in 0..16 {
        if unsafe { status.read() } & 0x01 == 0 {
            break;
        }
        let _ = unsafe { data.read() };
    }
}

/// Program PIT channel 0 for a 1 kHz periodic interrupt.
pub fn init_pit() {
    let divisor: u16 = 1193; // 1_193_182 Hz / 1000
    unsafe {
        Port::<u8>::new(0x43).write(0x36);
        Port::<u8>::new(0x40).write((divisor & 0xFF) as u8);
        Port::<u8>::new(0x40).write((divisor >> 8) as u8);
    }
}

// ---------------------------------------------------------------------------
// TSC timing
// ---------------------------------------------------------------------------

pub static TSC_CYCLES_PER_MS: AtomicU64 = AtomicU64::new(0);

/// Fallback when calibration is impossible: assume a 2 GHz TSC (a Core 2 Quad runs 2.4-3 GHz,
/// so delays come out somewhat shorter than asked, never minutes long).
const FALLBACK_CYCLES_PER_MS: u64 = 2_000_000;

#[inline]
pub fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Measure the TSC frequency using PIT channel 2 in one-shot mode. This polls port 0x61
/// directly, so it works with interrupts disabled and does not depend on IRQ 0 firing.
pub fn calibrate_tsc() {
    const PIT_HZ: u64 = 1_193_182;
    const COUNT: u16 = 23864; // ~20 ms
    let mut ctrl = Port::<u8>::new(0x61);
    let mut cmd = Port::<u8>::new(0x43);
    let mut ch2 = Port::<u8>::new(0x42);

    let cycles = unsafe {
        // Gate on, speaker off.
        let v = ctrl.read();
        ctrl.write((v & !0x02) | 0x01);
        // Channel 2, lobyte/hibyte, mode 0 (interrupt on terminal count).
        cmd.write(0xB0);
        ch2.write((COUNT & 0xFF) as u8);
        ch2.write((COUNT >> 8) as u8);

        let start = rdtsc();
        let mut spins: u64 = 0;
        // OUT2 (bit 5 of port 0x61) goes high when the count expires.
        while ctrl.read() & 0x20 == 0 {
            spins += 1;
            if spins > 50_000_000 {
                break;
            }
        }
        let end = rdtsc();
        if spins > 50_000_000 { 0 } else { end - start }
    };

    let per_ms = cycles * PIT_HZ / (COUNT as u64 * 1000);
    let per_ms = if (100_000..20_000_000).contains(&per_ms) { per_ms } else { FALLBACK_CYCLES_PER_MS };
    TSC_CYCLES_PER_MS.store(per_ms, Ordering::Relaxed);
}

#[inline]
fn cycles_per_ms() -> u64 {
    match TSC_CYCLES_PER_MS.load(Ordering::Relaxed) {
        0 => FALLBACK_CYCLES_PER_MS,
        c => c,
    }
}

/// Milliseconds since boot (TSC based, monotonic).
pub fn now_ms() -> u64 {
    rdtsc() / cycles_per_ms()
}

pub fn delay_us(us: u32) {
    let target = us as u64 * cycles_per_ms() / 1000;
    let start = rdtsc();
    while rdtsc().wrapping_sub(start) < target {
        core::hint::spin_loop();
    }
}

pub fn delay_ms(ms: u32) {
    let target = ms as u64 * cycles_per_ms();
    let start = rdtsc();
    while rdtsc().wrapping_sub(start) < target {
        core::hint::spin_loop();
    }
}

/// A point in time after which an operation should give up.
#[derive(Clone, Copy)]
pub struct Deadline(u64);

impl Deadline {
    pub fn after_ms(ms: u64) -> Self {
        Deadline(rdtsc() + ms * cycles_per_ms())
    }

    pub fn after_us(us: u64) -> Self {
        Deadline(rdtsc() + us * cycles_per_ms() / 1000)
    }

    pub fn expired(&self) -> bool {
        rdtsc() >= self.0
    }
}

/// Spin until `cond` returns true or `timeout_ms` elapses. Returns whether `cond` succeeded.
pub fn wait_until(timeout_ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Deadline::after_ms(timeout_ms);
    loop {
        if cond() {
            return true;
        }
        if deadline.expired() {
            return cond();
        }
        core::hint::spin_loop();
    }
}

#[test_case]
fn test_breakpoint_exception() {
    x86_64::instructions::interrupts::int3();
}

#[test_case]
fn test_scancode_queue_roundtrip() {
    push_scancode(0x39);
    push_scancode(0xB9);
    assert_eq!(pop_scancode(), Some(0x39));
    assert_eq!(pop_scancode(), Some(0xB9));
    assert_eq!(pop_scancode(), None);
}
