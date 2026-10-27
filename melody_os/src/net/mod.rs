//! Networking: Intel e1000 or Realtek 8168 NIC + smoltcp with a static IPv4 address.

pub mod e1000;
pub mod mpd;
pub mod rtl8168;

pub const STATIC_IP: [u8; 4] = [192, 168, 1, 100];
pub const GATEWAY_IP: [u8; 4] = [192, 168, 1, 1];

use alloc::vec;
use alloc::vec::Vec;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpCidr, Ipv4Address, Ipv4Cidr};
use spin::Mutex;

use crate::pci::PciDevice;
use e1000::E1000;
use rtl8168::Rtl8168;

/// The network card, whichever driver it uses.
pub enum Nic {
    Intel(E1000),
    Realtek(Rtl8168),
}

impl Nic {
    fn mac(&self) -> [u8; 6] {
        match self { Nic::Intel(n) => n.mac, Nic::Realtek(n) => n.mac }
    }
    fn receive(&mut self, out: &mut [u8]) -> Option<usize> {
        match self { Nic::Intel(n) => n.receive(out), Nic::Realtek(n) => n.receive(out) }
    }
    fn tx_ready(&self) -> bool {
        match self { Nic::Intel(n) => n.tx_ready(), Nic::Realtek(n) => n.tx_ready() }
    }
    fn transmit(&mut self, len: usize, fill: &mut dyn FnMut(&mut [u8])) -> bool {
        match self { Nic::Intel(n) => n.transmit(len, |b| fill(b)), Nic::Realtek(n) => n.transmit(len, fill) }
    }
    fn link_up(&self) -> bool {
        match self { Nic::Intel(n) => n.link_up(), Nic::Realtek(n) => n.link_up() }
    }
    fn speed(&self) -> u32 {
        match self { Nic::Intel(n) => n.speed(), Nic::Realtek(n) => n.speed() }
    }
    /// Chip-specific details for the hardware info page.
    fn details(&self) -> alloc::string::String {
        match self {
            Nic::Intel(n) => alloc::format!("Intel {:04x}{}", n.device_id, if n.ich { " ICH LAN" } else { "" }),
            Nic::Realtek(n) => n.debug_state(),
        }
    }
    fn packets(&self) -> (u64, u64) {
        match self { Nic::Intel(n) => (n.rx_packets, n.tx_packets), Nic::Realtek(n) => (n.rx_packets, n.tx_packets) }
    }
}

pub struct Net {
    nic: Nic,
    iface: Interface,
    sockets: SocketSet<'static>,
    ip: Option<Ipv4Cidr>,
    gateway: Option<Ipv4Address>,
}

pub static NET: Mutex<Option<Net>> = Mutex::new(None);
/// PCI ID of the network controller found at boot, even if no driver attached.
static NIC_ID: Mutex<Option<(u16, u16)>> = Mutex::new(None);

fn now() -> Instant {
    Instant::from_millis(crate::interrupts::now_ms() as i64)
}

// --- smoltcp device glue -------------------------------------------------------------

pub struct NicRx {
    buf: [u8; 1536],
    len: usize,
}

impl RxToken for NicRx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.buf[..self.len])
    }
}

pub struct NicTx<'a>(&'a mut Nic);

impl TxToken for NicTx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut result = None;
        let mut f = Some(f);
        let sent = self.0.transmit(len, &mut |buf| result = Some((f.take().unwrap())(buf)));
        match result {
            Some(r) => r,
            None => {
                // Ring full: smoltcp still needs the closure run; build the frame and drop it.
                debug_assert!(!sent);
                let mut scratch = vec![0u8; len];
                (f.take().unwrap())(&mut scratch)
            }
        }
    }
}

impl Device for Nic {
    type RxToken<'a> = NicRx where Self: 'a;
    type TxToken<'a> = NicTx<'a> where Self: 'a;

    fn receive(&mut self, _ts: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut rx = NicRx { buf: [0; 1536], len: 0 };
        rx.len = self.receive(&mut rx.buf)?;
        Some((rx, NicTx(self)))
    }

    fn transmit(&mut self, _ts: Instant) -> Option<Self::TxToken<'_>> {
        if self.tx_ready() { Some(NicTx(self)) } else { None }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps.max_burst_size = Some(8);
        caps
    }
}

// --- setup and polling ------------------------------------------------------------------

/// Bring up the first supported NIC. Returns a short description for the boot log.
pub fn init(pci_devices: &[PciDevice]) -> Result<alloc::string::String, &'static str> {
    let dev = crate::pci::find_network_controller(pci_devices).ok_or("no Ethernet controller found")?;
    *NIC_ID.lock() = Some((dev.vendor_id, dev.device_id));
    let (mut nic, chip) = if E1000::is_supported(&dev) {
        let n = unsafe { E1000::new(&dev)? };
        let chip = alloc::format!("Intel {:04x}{}{}", n.device_id, if n.ich { " (ICH LAN)" } else { "" }, if n.mac_from_nvm { "" } else { " (fallback MAC)" });
        (Nic::Intel(n), chip)
    } else if Rtl8168::is_supported(&dev) {
        let n = unsafe { Rtl8168::new(&dev)? };
        let chip = alloc::format!("Realtek {:04x} ({} xid {:#x})", n.device_id, n.family(), n.xid >> 20);
        (Nic::Realtek(n), chip)
    } else {
        return Err("no driver for this Ethernet chip");
    };
    let mac = nic.mac();

    let mut config = Config::new(EthernetAddress(mac).into());
    config.random_seed = crate::interrupts::rdtsc();
    let mut iface = Interface::new(config, &mut nic, now());

    let ip = Ipv4Cidr::new(Ipv4Address::new(STATIC_IP[0], STATIC_IP[1], STATIC_IP[2], STATIC_IP[3]), 24);
    let gateway = Ipv4Address::new(GATEWAY_IP[0], GATEWAY_IP[1], GATEWAY_IP[2], GATEWAY_IP[3]);
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::Ipv4(ip));
    });
    let _ = iface.routes_mut().add_default_ipv4_route(gateway);
    let sockets = SocketSet::new(Vec::new());

    let desc = alloc::format!(
        "{} MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        chip, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    *NET.lock() = Some(Net {
        nic,
        iface,
        sockets,
        ip: Some(ip),
        gateway: Some(gateway),
    });
    Ok(desc)
}

pub fn poll() {
    let mut guard = NET.lock();
    let Some(net) = guard.as_mut() else { return };
    let ts = now();
    net.iface.poll(ts, &mut net.nic, &mut net.sockets);
}
