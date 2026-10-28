//! Networking: Intel e1000 or Realtek 8168 NIC + smoltcp (DHCP, ARP, ICMP echo, TCP)
//! + the MPD server.
//!
//! The IP shown on screen is only ever the address DHCP actually leased to us, and the
//! same address smoltcp answers ARP and ping for.

pub mod e1000;
pub mod mpd;
pub mod rtl8168;

use alloc::vec;
use alloc::vec::Vec;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::{dhcpv4, tcp};
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{EthernetAddress, IpCidr, Ipv4Address, Ipv4Cidr};
use spin::Mutex;

use crate::audio::player::PLAYER;
use crate::pci::PciDevice;
use e1000::E1000;
use rtl8168::Rtl8168;

const MPD_PORT: u16 = 6600;
const MAX_CLIENTS: usize = 4;

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
    dhcp: SocketHandle,
    mpd: [SocketHandle; MAX_CLIENTS],
    clients: [Option<mpd::Client>; MAX_CLIENTS],
    ip: Option<Ipv4Cidr>,
    gateway: Option<Ipv4Address>,
    dhcp_since_ms: u64,
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
    let iface = Interface::new(config, &mut nic, now());

    let mut sockets = SocketSet::new(Vec::new());
    let mut dhcp_socket = dhcpv4::Socket::new();
    dhcp_socket.set_max_lease_duration(Some(Duration::from_secs(3600)));
    let dhcp = sockets.add(dhcp_socket);

    let mpd = core::array::from_fn(|_| {
        let mut s = tcp::Socket::new(tcp::SocketBuffer::new(vec![0; 4096]), tcp::SocketBuffer::new(vec![0; 16384]));
        s.set_keep_alive(Some(Duration::from_secs(30)));
        let _ = s.listen(MPD_PORT);
        sockets.add(s)
    });

    let desc = alloc::format!(
        "{} MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        chip, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    *NET.lock() = Some(Net {
        nic,
        iface,
        sockets,
        dhcp,
        mpd,
        clients: Default::default(),
        ip: None,
        gateway: None,
        dhcp_since_ms: crate::interrupts::now_ms(),
    });
    Ok(desc)
}

pub fn poll() {
    let mut guard = NET.lock();
    let Some(net) = guard.as_mut() else { return };
    let ts = now();
    net.iface.poll(ts, &mut net.nic, &mut net.sockets);

    match net.sockets.get_mut::<dhcpv4::Socket>(net.dhcp).poll() {
        Some(dhcpv4::Event::Configured(cfg)) => {
            net.iface.update_ip_addrs(|addrs| {
                addrs.clear();
                let _ = addrs.push(IpCidr::Ipv4(cfg.address));
            });
            match cfg.router {
                Some(r) => {
                    let _ = net.iface.routes_mut().add_default_ipv4_route(r);
                }
                None => {
                    net.iface.routes_mut().remove_default_ipv4_route();
                }
            }
            net.ip = Some(cfg.address);
            net.gateway = cfg.router;
            crate::println!("[net] DHCP lease: {} gateway {:?}", cfg.address, cfg.router);
            crate::ui::notify(alloc::format!("Network up: {}", cfg.address.address()));
        }
        Some(dhcpv4::Event::Deconfigured) => {
            // smoltcp also reports this once at startup, before any lease existed.
            if net.ip.is_some() {
                crate::ui::notify("Network: DHCP lease lost".into());
            }
            net.iface.update_ip_addrs(|addrs| addrs.clear());
            net.iface.routes_mut().remove_default_ipv4_route();
            net.ip = None;
            net.gateway = None;
            net.dhcp_since_ms = crate::interrupts::now_ms();
        }
        None => {}
    }

    serve_mpd(net);
    // Push any MPD responses out right away instead of waiting for the next poll.
    net.iface.poll(now(), &mut net.nic, &mut net.sockets);
}

fn serve_mpd(net: &mut Net) {
    let mut player_guard = PLAYER.lock();
    let Some(player) = player_guard.as_mut() else { return };

    for i in 0..MAX_CLIENTS {
        let sock = net.sockets.get_mut::<tcp::Socket>(net.mpd[i]);

        if sock.is_active() && net.clients[i].is_none() {
            net.clients[i] = Some(mpd::Client::new());
            if let Some(ep) = sock.remote_endpoint() {
                crate::serial_println!("[mpd] client connected from {}", ep);
            }
        }
        if !sock.is_active() {
            net.clients[i] = None;
            if !sock.is_open() {
                let _ = sock.listen(MPD_PORT);
            }
            continue;
        }
        let client = net.clients[i].as_mut().unwrap();

        if sock.can_recv() {
            let mut buf = [0u8; 1024];
            if let Ok(n) = sock.recv_slice(&mut buf) {
                client.feed(&buf[..n], player);
            }
        }

        if !client.out.is_empty() && sock.can_send() {
            if let Ok(n) = sock.send_slice(&client.out) {
                client.out.drain(..n);
            }
        }
        if client.close && client.out.is_empty() {
            sock.close();
        }
        if !sock.may_recv() && sock.may_send() && client.out.is_empty() {
            sock.close(); // peer closed its side
        }
    }
}
