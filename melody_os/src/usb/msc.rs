//! USB Mass Storage, Bulk-Only Transport (BOT) with SCSI commands.
//! Shared by the EHCI and UHCI drivers through the `BulkTransport` trait.

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;
use crate::fs::BlockDevice;
use super::ehci::EhciController;
use super::uhci::UhciController;

lazy_static! {
    pub static ref EHCI_CONTROLLERS: Mutex<Vec<EhciController>> = Mutex::new(Vec::new());
    pub static ref UHCI_CONTROLLERS: Mutex<Vec<UhciController>> = Mutex::new(Vec::new());
}

#[derive(Debug, Clone, Copy)]
pub enum UsbControllerRef {
    Ehci(usize),
    Uhci(usize),
}

/// A USB flash drive / card reader / external disk.
#[derive(Debug, Clone)]
pub struct UsbStorageDrive {
    pub name: String,
    pub model: String,
    pub capacity_mb: u64,
    pub sectors: u64,
    pub sector_size: u32,
    pub controller_ref: UsbControllerRef,
    pub dev_addr: u8,
    pub ep_in: u8,
    pub ep_out: u8,
    pub max_packet: u16,
    /// Set when the stick failed a multi-sector read: from then on read one sector at a time.
    pub single_sector: bool,
}

pub trait BulkTransport {
    fn bulk(&mut self, dev_addr: u8, ep: u8, is_in: bool, max_packet: u16, data: &mut [u8]) -> Result<usize, &'static str>;
    fn clear_halt(&mut self, dev_addr: u8, ep_addr: u8);
    fn max_transfer(&self) -> usize;
}

pub enum DataDir<'a> {
    None,
    In(&'a mut [u8]),
}

pub struct MscEndpoints {
    pub ep_in: u8,
    pub ep_out: u8,
    pub max_packet: u16,
}

/// Find the bulk IN/OUT endpoints of a mass-storage BOT interface in a configuration descriptor.
pub fn parse_msc_endpoints(conf: &[u8]) -> Option<MscEndpoints> {
    let mut in_msc = false;
    let mut ep_in = 0u8;
    let mut ep_out = 0u8;
    let mut max_packet = 64u16;
    let mut i = 0;
    while i + 2 <= conf.len() {
        let len = conf[i] as usize;
        if len < 2 || i + len > conf.len() {
            break;
        }
        match conf[i + 1] {
            0x04 if len >= 9 => {
                // Interface: class 08 (mass storage), protocol 50 (bulk-only).
                in_msc = conf[i + 5] == 0x08 && conf[i + 7] == 0x50;
            }
            0x05 if len >= 7 && in_msc => {
                let addr = conf[i + 2];
                let attrs = conf[i + 3];
                let mps = u16::from_le_bytes([conf[i + 4], conf[i + 5]]) & 0x7FF;
                if attrs & 0x03 == 0x02 {
                    if addr & 0x80 != 0 {
                        ep_in = addr & 0x0F;
                    } else {
                        ep_out = addr & 0x0F;
                    }
                    max_packet = mps;
                }
            }
            _ => {}
        }
        i += len;
    }
    if ep_in != 0 && ep_out != 0 {
        Some(MscEndpoints { ep_in, ep_out, max_packet })
    } else {
        None
    }
}

pub fn read10_cdb(lba: u64, count: u16) -> [u8; 10] {
    let lba = lba as u32;
    let mut cdb = [0u8; 10];
    cdb[0] = 0x28;
    cdb[2..6].copy_from_slice(&lba.to_be_bytes());
    cdb[7..9].copy_from_slice(&count.to_be_bytes());
    cdb
}

static NEXT_TAG: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(1);

/// Send one SCSI command over BOT: CBW, optional data-in stage, CSW.
pub fn bot_command<T: BulkTransport + ?Sized>(
    t: &mut T,
    dev_addr: u8,
    ep_in: u8,
    ep_out: u8,
    cdb: &[u8],
    data: DataDir<'_>,
) -> Result<(), &'static str> {
    let mps = 512;
    let tag = NEXT_TAG.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let data_len = match &data {
        DataDir::None => 0,
        DataDir::In(buf) => buf.len(),
    };

    let mut cbw = [0u8; 31];
    cbw[0..4].copy_from_slice(b"USBC");
    cbw[4..8].copy_from_slice(&tag.to_le_bytes());
    cbw[8..12].copy_from_slice(&(data_len as u32).to_le_bytes());
    cbw[12] = if data_len > 0 { 0x80 } else { 0x00 };
    cbw[14] = cdb.len() as u8;
    cbw[15..15 + cdb.len()].copy_from_slice(cdb);
    t.bulk(dev_addr, ep_out, false, mps, &mut cbw)?;

    if let DataDir::In(buf) = data {
        let chunk = t.max_transfer();
        let mut off = 0;
        while off < buf.len() {
            let end = (off + chunk).min(buf.len());
            match t.bulk(dev_addr, ep_in, true, mps, &mut buf[off..end]) {
                Ok(n) if n == end - off => off = end,
                Ok(_) => break, // short read; the CSW tells us what happened
                Err(_) => {
                    // Stalled data stage: clear it and still collect the CSW.
                    t.clear_halt(dev_addr, ep_in | 0x80);
                    break;
                }
            }
        }
    }

    let mut csw = [0u8; 13];
    if t.bulk(dev_addr, ep_in, true, mps, &mut csw).is_err() {
        t.clear_halt(dev_addr, ep_in | 0x80);
        t.bulk(dev_addr, ep_in, true, mps, &mut csw)?;
    }
    if &csw[0..4] != b"USBS" || u32::from_le_bytes([csw[4], csw[5], csw[6], csw[7]]) != tag {
        return Err("bad CSW");
    }
    match csw[12] {
        0 => Ok(()),
        1 => Err("SCSI command failed"),
        _ => Err("SCSI phase error"),
    }
}

/// Bring a freshly configured drive up: wait until ready, read its name and capacity.
pub fn identify<T: BulkTransport + ?Sized>(t: &mut T, drive: &mut UsbStorageDrive) {
    let (a, i, o) = (drive.dev_addr, drive.ep_in, drive.ep_out);

    // TEST UNIT READY until the media is ready; REQUEST SENSE clears UNIT ATTENTION.
    for _ in 0..20 {
        if bot_command(t, a, i, o, &[0u8; 6], DataDir::None).is_ok() {
            break;
        }
        let mut sense = [0u8; 18];
        let _ = bot_command(t, a, i, o, &[0x03, 0, 0, 0, 18, 0], DataDir::In(&mut sense));
        crate::interrupts::delay_ms(50);
    }

    let mut inq = [0u8; 36];
    if bot_command(t, a, i, o, &[0x12, 0, 0, 0, 36, 0], DataDir::In(&mut inq)).is_ok() {
        let vendor = core::str::from_utf8(&inq[8..16]).unwrap_or("").trim();
        let product = core::str::from_utf8(&inq[16..32]).unwrap_or("").trim();
        drive.model = alloc::format!("{} {}", vendor, product).trim().into();
    }
    if drive.model.is_empty() {
        drive.model = "USB drive".into();
    }

    for _ in 0..5 {
        let mut cap = [0u8; 8];
        if bot_command(t, a, i, o, &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], DataDir::In(&mut cap)).is_ok() {
            let last_lba = u32::from_be_bytes([cap[0], cap[1], cap[2], cap[3]]) as u64;
            let block = u32::from_be_bytes([cap[4], cap[5], cap[6], cap[7]]);
            if block > 0 {
                drive.sector_size = block;
                drive.sectors = last_lba + 1;
                drive.capacity_mb = (last_lba + 1) * block as u64 / (1024 * 1024);
                return;
            }
        }
        crate::interrupts::delay_ms(40);
    }
}

impl UsbStorageDrive {
    fn read_raw(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        match self.controller_ref {
            UsbControllerRef::Ehci(idx) => {
                let mut ctrls = EHCI_CONTROLLERS.lock();
                let c = ctrls.get_mut(idx).ok_or("EHCI controller missing")?;
                c.read_sectors_msc(self, lba, buf)
            }
            UsbControllerRef::Uhci(idx) => {
                let mut ctrls = UHCI_CONTROLLERS.lock();
                let c = ctrls.get_mut(idx).ok_or("UHCI controller missing")?;
                c.read_sectors_msc(self, lba, buf)
            }
        }
    }
}

impl BlockDevice for UsbStorageDrive {
    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        if self.sector_size != 512 {
            return Err("only 512-byte sector drives are supported");
        }
        let mut off = 0;
        while off < buf.len() {
            let end = (off + self.max_read_bytes()).min(buf.len());
            match self.read_raw(lba + (off / 512) as u64, &mut buf[off..end]) {
                Ok(()) => off = end,
                Err(e) if end - off > 512 && !self.single_sector => {
                    // Some cheap sticks choke on large reads; fall back to what always works.
                    crate::println!("[usb] {}: multi-sector read failed ({}), using single sectors", self.model, e);
                    self.single_sector = true;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn sector_count(&self) -> Option<u64> {
        if self.sectors > 0 { Some(self.sectors) } else { None }
    }

    fn max_read_bytes(&self) -> usize {
        if self.single_sector {
            return 512;
        }
        match self.controller_ref {
            UsbControllerRef::Ehci(_) => super::ehci::BOUNCE_SIZE,
            UsbControllerRef::Uhci(_) => 4096,
        }
    }
}
