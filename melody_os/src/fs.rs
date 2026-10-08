//! Read-only FAT32/FAT16.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// A device that reads 512-byte sectors.
pub trait BlockDevice {
    /// Read `buf.len() / 512` consecutive sectors starting at `lba`.
    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), &'static str>;

    /// Largest read the device handles efficiently in one request.
    fn max_read_bytes(&self) -> usize {
        512
    }

    /// Size of the device in sectors, if known.
    fn sector_count(&self) -> Option<u64> {
        None
    }

    fn read_sector(&mut self, lba: u64, buf: &mut [u8; 512]) -> Result<(), &'static str> {
        self.read_sectors(lba, buf)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PartitionEntry {
    pub status: u8,
    pub partition_type: u8,
    pub lba_start: u32,
    pub sector_count: u32,
}

#[derive(Debug, Clone)]
pub struct MasterBootRecord {
    pub partitions: [Option<PartitionEntry>; 4],
}

impl MasterBootRecord {
    pub fn parse(sector: &[u8; 512]) -> Result<Self, &'static str> {
        if sector[510] != 0x55 || sector[511] != 0xAA {
            return Err("Invalid MBR signature (missing 0x55AA)");
        }
        let mut partitions = [None; 4];
        for (i, slot) in partitions.iter_mut().enumerate() {
            let o = 446 + i * 16;
            let partition_type = sector[o + 4];
            let lba_start = u32::from_le_bytes([sector[o + 8], sector[o + 9], sector[o + 10], sector[o + 11]]);
            let sector_count = u32::from_le_bytes([sector[o + 12], sector[o + 13], sector[o + 14], sector[o + 15]]);
            if partition_type != 0 && sector_count > 0 {
                *slot = Some(PartitionEntry { status: sector[o], partition_type, lba_start, sector_count });
            }
        }
        Ok(MasterBootRecord { partitions })
    }
}

/// Check a boot sector; returns true for FAT32, false for FAT16, or why it can't be used.
fn classify_boot_sector(s: &[u8; 512]) -> Result<bool, &'static str> {
    if &s[3..11] == b"EXFAT   " {
        return Err("exFAT volume (reformat the stick as FAT32)");
    }
    if &s[3..11] == b"NTFS    " {
        return Err("NTFS volume (reformat the stick as FAT32)");
    }
    if s[510] != 0x55 || s[511] != 0xAA {
        return Err("no boot sector signature");
    }
    let bps = u16::from_le_bytes([s[11], s[12]]);
    let spc = s[13] as u64;
    let reserved = u16::from_le_bytes([s[14], s[15]]) as u64;
    let num_fats = s[16] as u64;
    let root_entries = u16::from_le_bytes([s[17], s[18]]) as u64;
    let fat_size16 = u16::from_le_bytes([s[22], s[23]]) as u64;
    let fat_size32 = u32::from_le_bytes([s[36], s[37], s[38], s[39]]) as u64;
    if bps != 512 {
        return Err("unsupported sector size");
    }
    if spc == 0 || !spc.is_power_of_two() || num_fats == 0 || reserved == 0 {
        return Err("not a FAT volume");
    }
    if root_entries == 0 && fat_size16 == 0 && fat_size32 != 0 {
        return Ok(true);
    }
    // FAT12 vs FAT16 is decided by the cluster count (Microsoft FAT spec).
    let total = match u16::from_le_bytes([s[19], s[20]]) as u64 {
        0 => u32::from_le_bytes([s[32], s[33], s[34], s[35]]) as u64,
        n => n,
    };
    let data = total.saturating_sub(reserved + num_fats * fat_size16 + (root_entries * 32).div_ceil(512));
    if data / spc < 4085 {
        return Err("FAT12 volume (reformat the stick as FAT32)");
    }
    Ok(false)
}

/// A mounted FAT32 (or FAT16) volume.
pub struct FatVolume {
    pub fat16: bool,
    pub fat_lba: u64,
    pub data_lba: u64,
    pub sectors_per_cluster: u32,
    /// FAT32: first cluster of the root directory. FAT16: 0 (fixed root region).
    pub root_cluster: u32,
    root_dir_lba: u64,
    root_dir_sectors: u64,
    pub total_sectors: u64,
    pub label: String,
    fat_cache_lba: u64,
    fat_cache: Box<[u8; 512]>,
}

impl FatVolume {
    pub fn mount<D: BlockDevice + ?Sized>(dev: &mut D, partition_lba: u64) -> Result<Self, &'static str> {
        let mut s = [0u8; 512];
        dev.read_sector(partition_lba, &mut s)?;
        let is_fat32 = classify_boot_sector(&s)?;

        let spc = s[13] as u32;
        let reserved = u16::from_le_bytes([s[14], s[15]]) as u64;
        let num_fats = s[16] as u64;
        let root_entries = u16::from_le_bytes([s[17], s[18]]) as u64;
        let total_sectors = match u16::from_le_bytes([s[19], s[20]]) as u64 {
            0 => u32::from_le_bytes([s[32], s[33], s[34], s[35]]) as u64,
            n => n,
        };
        let fat_lba = partition_lba + reserved;
        let (fat_size, root_cluster, label_off) = if is_fat32 {
            (u32::from_le_bytes([s[36], s[37], s[38], s[39]]) as u64, u32::from_le_bytes([s[44], s[45], s[46], s[47]]), 71)
        } else {
            (u16::from_le_bytes([s[22], s[23]]) as u64, 0, 43)
        };
        if is_fat32 && root_cluster < 2 {
            return Err("invalid FAT32 root cluster");
        }
        let root_dir_lba = fat_lba + num_fats * fat_size;
        let root_dir_sectors = (root_entries * 32).div_ceil(512);
        let label = core::str::from_utf8(&s[label_off..label_off + 11]).unwrap_or("").trim().into();
        Ok(FatVolume {
            fat16: !is_fat32,
            fat_lba,
            data_lba: root_dir_lba + root_dir_sectors,
            sectors_per_cluster: spc,
            root_cluster,
            root_dir_lba,
            root_dir_sectors,
            total_sectors,
            label,
            fat_cache_lba: u64::MAX,
            fat_cache: Box::new([0u8; 512]),
        })
    }

    pub fn type_name(&self) -> &'static str {
        if self.fat16 { "FAT16" } else { "FAT32" }
    }

    pub fn cluster_bytes(&self) -> usize {
        self.sectors_per_cluster as usize * 512
    }

    pub fn cluster_to_lba(&self, cluster: u32) -> u64 {
        self.data_lba + (cluster.saturating_sub(2) as u64) * self.sectors_per_cluster as u64
    }

    pub fn next_cluster<D: BlockDevice + ?Sized>(&mut self, dev: &mut D, cluster: u32) -> Result<Option<u32>, &'static str> {
        let (entry, eoc, bad) = if self.fat16 { (2u64, 0xFFF8, 0xFFF7) } else { (4u64, 0x0FFF_FFF8, 0x0FFF_FFF7) };
        if cluster < 2 || cluster >= bad {
            return Ok(None);
        }
        let byte = cluster as u64 * entry;
        let lba = self.fat_lba + byte / 512;
        if lba != self.fat_cache_lba {
            self.fat_cache_lba = u64::MAX;
            dev.read_sector(lba, &mut self.fat_cache)?;
            self.fat_cache_lba = lba;
        }
        let o = (byte % 512) as usize;
        let c = &self.fat_cache;
        let next = if self.fat16 {
            u16::from_le_bytes([c[o], c[o + 1]]) as u32
        } else {
            u32::from_le_bytes([c[o], c[o + 1], c[o + 2], c[o + 3]]) & 0x0FFF_FFFF
        };
        match next {
            n if n >= eoc => Ok(None),
            n if n == bad => Err("bad cluster in FAT chain"),
            0 | 1 => Err("broken FAT chain"),
            n => Ok(Some(n)),
        }
    }
}

/// Find a FAT32/FAT16 volume on a drive: MBR partitions, GPT partitions, or an unpartitioned stick.
pub fn find_fat_volume<D: BlockDevice + ?Sized>(dev: &mut D) -> Result<FatVolume, &'static str> {
    let mut sector0 = [0u8; 512];
    dev.read_sector(0, &mut sector0)?;

    let mut candidates: Vec<u64> = Vec::new();
    if let Ok(mbr) = MasterBootRecord::parse(&sector0) {
        let gpt = mbr.partitions.iter().flatten().any(|p| p.partition_type == 0xEE);
        if gpt {
            let mut hdr = [0u8; 512];
            if dev.read_sector(1, &mut hdr).is_ok() && &hdr[0..8] == b"EFI PART" {
                let entries_lba = u64::from_le_bytes(hdr[72..80].try_into().unwrap());
                let count = u32::from_le_bytes(hdr[80..84].try_into().unwrap()).min(128) as usize;
                let esize = (u32::from_le_bytes(hdr[84..88].try_into().unwrap()) as usize).max(128);
                let per_sector = 512 / esize;
                let mut sec = [0u8; 512];
                for i in 0..count.div_ceil(per_sector.max(1)) {
                    if dev.read_sector(entries_lba + i as u64, &mut sec).is_err() {
                        break;
                    }
                    for e in sec.chunks_exact(esize) {
                        if e[0..16].iter().any(|&b| b != 0) {
                            let start = u64::from_le_bytes(e[32..40].try_into().unwrap());
                            if start > 0 {
                                candidates.push(start);
                            }
                        }
                    }
                }
            }
        } else {
            // A boot sector full of code (like the Melody OS boot stick itself) also ends in
            // 0x55AA, so only trust entries that look like real partitions.
            let limit = dev.sector_count().unwrap_or(u64::MAX);
            for p in mbr.partitions.iter().flatten() {
                let end = p.lba_start as u64 + p.sector_count as u64;
                if (p.status == 0x00 || p.status == 0x80) && p.lba_start > 0 && end <= limit {
                    candidates.push(p.lba_start as u64);
                }
            }
        }
    }
    // An unpartitioned ("superfloppy") stick has the FAT boot sector at LBA 0.
    candidates.push(0);

    // Only pass on errors that tell the user something actionable (wrong filesystem);
    // anything else just means "this drive has no FAT volume".
    let mut err = "no FAT32 partition";
    for lba in candidates {
        match FatVolume::mount(dev, lba) {
            Ok(fs) => return Ok(fs),
            Err(e) if e.contains("reformat") => err = e,
            Err(_) => {}
        }
    }
    Err(err)
}
