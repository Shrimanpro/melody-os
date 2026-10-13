pub mod ehci;
pub mod msc;

use alloc::vec::Vec;
use crate::pci::{self, PciDevice};
use crate::println;
use self::ehci::EhciController;
use self::msc::{UsbStorageDrive, EHCI_CONTROLLERS};

/// Bring up every EHCI controller and return the USB mass storage drives found.
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

    for dev in pci::find_usb_controllers(pci_devices, 0x30) {
        println!("       xHCI controller at {} (USB 3.0, not supported - use a USB 2.0 port)", dev.address);
    }
    drives
}
