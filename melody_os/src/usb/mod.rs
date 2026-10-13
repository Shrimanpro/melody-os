pub mod ehci;
pub mod msc;
pub mod uhci;

use alloc::vec::Vec;
use crate::pci::{self, PciDevice};
use crate::println;
use self::ehci::EhciController;
use self::uhci::UhciController;
use self::msc::{UsbStorageDrive, EHCI_CONTROLLERS, UHCI_CONTROLLERS};

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
