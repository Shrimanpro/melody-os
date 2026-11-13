pub mod ehci;
pub mod msc;
pub mod uhci;

use alloc::vec::Vec;
use spin::Mutex;
use crate::keyboard::Key;
use crate::pci::{self, PciDevice};
use crate::println;
use self::ehci::EhciController;
use self::uhci::UhciController;
use self::msc::{UsbStorageDrive, EHCI_CONTROLLERS, UHCI_CONTROLLERS};

pub struct UsbKeyboard {
    pub uhci_idx: usize,
    pub dev_addr: u8,
    pub low_speed: bool,
    pub ep_in: u8,
    pub toggle: u32,
    last_keys: [u8; 6],
}

static USB_KEYBOARDS: Mutex<Vec<UsbKeyboard>> = Mutex::new(Vec::new());
static LAST_POLL_MS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn register_usb_keyboard(uhci_idx: usize, dev_addr: u8, low_speed: bool, ep_in: u8) {
    USB_KEYBOARDS.lock().push(UsbKeyboard { uhci_idx, dev_addr, low_speed, ep_in, toggle: 0, last_keys: [0; 6] });
}

pub fn keyboard_count() -> usize {
    USB_KEYBOARDS.lock().len()
}

/// Poll USB keyboards (at most every 10 ms) and report newly pressed keys.
pub fn poll_usb_keyboards(mut on_key: impl FnMut(Key)) {
    let now = crate::interrupts::now_ms();
    if now.wrapping_sub(LAST_POLL_MS.load(core::sync::atomic::Ordering::Relaxed)) < 10 {
        return;
    }
    LAST_POLL_MS.store(now, core::sync::atomic::Ordering::Relaxed);

    let mut kbds = USB_KEYBOARDS.lock();
    if kbds.is_empty() {
        return;
    }
    let mut ctrls = UHCI_CONTROLLERS.lock();
    for kbd in kbds.iter_mut() {
        let Some(ctrl) = ctrls.get_mut(kbd.uhci_idx) else { continue };
        let mut report = [0u8; 8];
        if !ctrl.poll_hid_keyboard_report(kbd, &mut report) {
            continue;
        }
        for &usage in &report[2..8] {
            if usage > 3 && !kbd.last_keys.contains(&usage) {
                if let Some(k) = crate::keyboard::from_usb_usage(usage) {
                    on_key(k);
                }
            }
        }
        kbd.last_keys.copy_from_slice(&report[2..8]);
    }
}

/// Bring up every EHCI and UHCI controller and return the USB mass storage drives found.
/// EHCI goes first: it hands full/low-speed devices to its UHCI companions.
pub fn scan(pci_devices: &[PciDevice]) -> Vec<UsbStorageDrive> {
    let mut drives = Vec::new();

    for dev in pci::find_usb_controllers(pci_devices, 0x20) {
        println!("       EHCI controller at {}", dev.address);
        match unsafe { EhciController::new(&dev) } {
            Ok(mut ctrl) => {
                let mut ctrls = EHCI_CONTROLLERS.lock();
                let idx = ctrls.len();
                drives.extend(ctrl.scan_storage_devices(idx));
                ctrls.push(ctrl);
            }
            Err(e) => println!("       EHCI at {} failed: {}", dev.address, e),
        }
    }

    let mut next_addr = 1u8;
    for dev in pci::find_usb_controllers(pci_devices, 0x00) {
        match unsafe { UhciController::new(&dev) } {
            Ok(mut ctrl) => {
                let mut ctrls = UHCI_CONTROLLERS.lock();
                let idx = ctrls.len();
                let name = alloc::format!("{}", dev.address);
                drives.extend(ctrl.scan_devices(idx, &mut next_addr, &name));
                ctrls.push(ctrl);
            }
            Err(_) => {} // no devices on this companion
        }
    }

    for dev in pci::find_usb_controllers(pci_devices, 0x30) {
        println!("       xHCI controller at {} (USB 3.0, not supported - use a USB 2.0 port)", dev.address);
    }
    drives
}
