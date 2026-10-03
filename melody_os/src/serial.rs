use lazy_static::lazy_static;
use spin::Mutex;
use uart_16550::{Config, Uart16550Tty, backend::PioBackend};

lazy_static! {
    pub static ref SERIAL1: Mutex<Option<Uart16550Tty<PioBackend>>> = Mutex::new({
        let ports = [0x3F8, 0x2F8, 0x3E8, 0x2E8];
        let mut serial = None;
        for &port in &ports {
            if let Ok(uart) = unsafe { Uart16550Tty::new_port(port, Config::default()) } {
                serial = Some(uart);
                break;
            }
        }
        serial
    });
}

/// Returns true if a hardware UART serial port was detected and initialized.
pub fn is_present() -> bool {
    SERIAL1.lock().is_some()
}

#[doc(hidden)]
pub fn _print(args: ::core::fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        if let Some(ref mut serial) = *SERIAL1.lock() {
            let _ = serial.write_fmt(args);
        }
    });
}

/// Prints to the host through the serial interface.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*));
    };
}

/// Prints to the host through the serial interface, appending a newline.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::serial_print!(
        concat!($fmt, "\n"), $($arg)*));
}
