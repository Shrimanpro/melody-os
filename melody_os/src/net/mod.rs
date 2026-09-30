pub mod e1000;

use alloc::vec::Vec;
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;

use self::e1000::E1000Driver;

pub const STATIC_IP: [u8; 4] = [192, 168, 1, 100];
pub const GATEWAY_IP: [u8; 4] = [192, 168, 1, 1];
pub const SUBNET_MASK: [u8; 4] = [255, 255, 255, 0];

/// Initialize network parameters with static IP address 192.168.1.100/24 and default gateway 192.168.1.1
pub fn init_network() {
    crate::println!(
        "Net: Static IP configured: {}.{}.{}.{}/24 (Gateway: {}.{}.{}.{})",
        STATIC_IP[0], STATIC_IP[1], STATIC_IP[2], STATIC_IP[3],
        GATEWAY_IP[0], GATEWAY_IP[1], GATEWAY_IP[2], GATEWAY_IP[3]
    );
}

pub struct E1000Phy<'a> {
    pub driver: &'a mut E1000Driver,
}

pub struct E1000RxToken {
    buffer: Vec<u8>,
}

impl RxToken for E1000RxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.buffer)
    }
}

pub struct E1000TxToken<'a> {
    driver: &'a mut E1000Driver,
}

impl<'a> TxToken for E1000TxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = alloc::vec![0u8; len];
        let result = f(&mut buffer);
        let _ = self.driver.send_packet(&buffer);
        result
    }
}

impl<'a> Device for E1000Phy<'a> {
    type RxToken<'b> = E1000RxToken where Self: 'b;
    type TxToken<'b> = E1000TxToken<'b> where Self: 'b;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.driver.receive_packet().map(|packet| {
            let rx = E1000RxToken { buffer: packet };
            let tx = E1000TxToken { driver: self.driver };
            (rx, tx)
        })
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(E1000TxToken { driver: self.driver })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ethernet;
        caps
    }
}
