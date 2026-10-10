//! Read-only FAT32/FAT16 with long file names and a streaming file reader.
//! Files are never loaded whole.

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

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_directory: bool,
    pub size: u32,
    pub first_cluster: u32,
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

    /// List a directory, assembling long file names where present.
    pub fn read_dir<D: BlockDevice + ?Sized>(&mut self, dev: &mut D, dir_cluster: u32) -> Result<Vec<DirEntry>, &'static str> {
        let mut entries = Vec::new();
        let mut lfn: [u16; 260] = [0; 260];
        let mut lfn_len = 0usize;
        let mut lfn_valid = false;
        let mut sector = [0u8; 512];
        let fixed_root = dir_cluster < 2 && self.fat16;
        let mut cluster = Some(if fixed_root { 0 } else { dir_cluster });
        let mut guard = 0;

        while let Some(c) = cluster {
            guard += 1;
            if guard > 4096 {
                break;
            }
            let (lba, sectors) = if fixed_root {
                (self.root_dir_lba, self.root_dir_sectors)
            } else {
                (self.cluster_to_lba(c), self.sectors_per_cluster as u64)
            };
            for s in 0..sectors {
                dev.read_sector(lba + s, &mut sector)?;
                for e in sector.chunks_exact(32) {
                    match e[0] {
                        0x00 => return Ok(entries),
                        0xE5 => {
                            lfn_valid = false;
                            continue;
                        }
                        _ => {}
                    }
                    let attr = e[11];
                    if attr == 0x0F {
                        // Long name fragment: 13 UCS-2 chars, stored last-fragment-first.
                        let seq = (e[0] & 0x1F) as usize;
                        if seq == 0 || seq > 20 {
                            lfn_valid = false;
                            continue;
                        }
                        if e[0] & 0x40 != 0 {
                            lfn = [0; 260];
                            lfn_len = 0;
                            lfn_valid = true;
                        }
                        let base = (seq - 1) * 13;
                        let offsets = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
                        for (k, &o) in offsets.iter().enumerate() {
                            let ch = u16::from_le_bytes([e[o], e[o + 1]]);
                            if ch == 0x0000 || ch == 0xFFFF || base + k >= 255 {
                                break;
                            }
                            lfn[base + k] = ch;
                            lfn_len = lfn_len.max(base + k + 1);
                        }
                        continue;
                    }
                    if attr & 0x08 != 0 {
                        lfn_valid = false;
                        continue; // volume label
                    }

                    let name = if lfn_valid && lfn_len > 0 {
                        decode_ucs2(&lfn[..lfn_len])
                    } else {
                        short_name(e)
                    };
                    lfn_valid = false;
                    // The high cluster word only exists on FAT32 (it's an access-rights field on FAT16).
                    let hi = if self.fat16 { 0 } else { u16::from_le_bytes([e[20], e[21]]) as u32 };
                    let lo = u16::from_le_bytes([e[26], e[27]]) as u32;
                    entries.push(DirEntry {
                        name,
                        is_directory: attr & 0x10 != 0,
                        size: u32::from_le_bytes([e[28], e[29], e[30], e[31]]),
                        first_cluster: (hi << 16) | lo,
                    });
                }
            }
            cluster = if fixed_root { None } else { self.next_cluster(dev, c)? };
        }
        Ok(entries)
    }
}

fn short_name(e: &[u8]) -> String {
    let lower_base = e[12] & 0x08 != 0;
    let lower_ext = e[12] & 0x10 != 0;
    let mut name = String::new();
    for &b in e[0..8].iter().take_while(|&&b| b != b' ') {
        name.push(if lower_base { b.to_ascii_lowercase() } else { b } as char);
    }
    if e[8] != b' ' {
        name.push('.');
        for &b in e[8..11].iter().take_while(|&&b| b != b' ') {
            name.push(if lower_ext { b.to_ascii_lowercase() } else { b } as char);
        }
    }
    name
}

/// UCS-2 to a printable string. Non-ASCII characters become their closest ASCII
/// equivalent where obvious, otherwise '?', because VGA text mode can't show them anyway.
fn decode_ucs2(chars: &[u16]) -> String {
    let mut s = String::with_capacity(chars.len());
    for &c in chars {
        let ch = match c {
            0x20..=0x7E => c as u8 as char,
            0xC0..=0xC5 => 'A',
            0xE0..=0xE5 => 'a',
            0xC8..=0xCB => 'E',
            0xE8..=0xEB => 'e',
            0xCC..=0xCF => 'I',
            0xEC..=0xEF => 'i',
            0xD2..=0xD6 | 0xD8 => 'O',
            0xF2..=0xF6 | 0xF8 => 'o',
            0xD9..=0xDC => 'U',
            0xF9..=0xFC => 'u',
            0xC7 => 'C',
            0xE7 => 'c',
            0xD1 => 'N',
            0xF1 => 'n',
            0xDF => 's',
            0x2018 | 0x2019 => '\'',
            0x201C | 0x201D => '"',
            0x2013 | 0x2014 => '-',
            _ => '?',
        };
        s.push(ch);
    }
    s
}

/// Sequential reader over a file's cluster chain.
pub struct FileReader {
    first_cluster: u32,
    pub size: u64,
    pos: u64,
    cluster: u32,
    cluster_index: u64,
}

impl FileReader {
    pub fn new(first_cluster: u32, size: u32) -> Self {
        FileReader { first_cluster, size: size as u64, pos: 0, cluster: first_cluster, cluster_index: 0 }
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Move to an absolute byte offset. Seeking backwards walks the chain from the start.
    pub fn seek<D: BlockDevice + ?Sized>(&mut self, fs: &mut FatVolume, dev: &mut D, pos: u64) -> Result<(), &'static str> {
        let pos = pos.min(self.size);
        let target = pos / fs.cluster_bytes() as u64;
        if target < self.cluster_index {
            self.cluster = self.first_cluster;
            self.cluster_index = 0;
        }
        while self.cluster_index < target {
            self.cluster = fs.next_cluster(dev, self.cluster)?.ok_or("seek past end of chain")?;
            self.cluster_index += 1;
        }
        self.pos = pos;
        Ok(())
    }

    /// Read up to `buf.len()` bytes; returns 0 at end of file.
    pub fn read<D: BlockDevice + ?Sized>(&mut self, fs: &mut FatVolume, dev: &mut D, buf: &mut [u8]) -> Result<usize, &'static str> {
        let cbytes = fs.cluster_bytes() as u64;
        let mut done = 0usize;
        while done < buf.len() && self.pos < self.size {
            // Advance to the cluster holding `pos`.
            while self.pos / cbytes > self.cluster_index {
                self.cluster = fs.next_cluster(dev, self.cluster)?.ok_or("file shorter than its size")?;
                self.cluster_index += 1;
            }
            let in_cluster = self.pos % cbytes;
            let file_left = self.size - self.pos;
            let buf_left = (buf.len() - done) as u64;
            // Extend the read across physically consecutive clusters so small-cluster
            // volumes still get large USB transfers.
            let mut span = cbytes - in_cluster;
            let mut c = self.cluster;
            while span < dev.max_read_bytes() as u64 && span < buf_left && span < file_left {
                match fs.next_cluster(dev, c)? {
                    Some(n) if n == c + 1 => {
                        c = n;
                        span += cbytes;
                    }
                    _ => break,
                }
            }
            let want = buf_left.min(span).min(file_left) as usize;
            let lba = fs.cluster_to_lba(self.cluster) + in_cluster / 512;
            let in_sector = (in_cluster % 512) as usize;

            if in_sector == 0 && want >= 512 {
                // Whole sectors straight into the caller's buffer.
                let bytes = (want / 512 * 512).min(dev.max_read_bytes());
                dev.read_sectors(lba, &mut buf[done..done + bytes])?;
                done += bytes;
                self.pos += bytes as u64;
            } else {
                let mut sector = [0u8; 512];
                dev.read_sector(lba, &mut sector)?;
                let n = want.min(512 - in_sector);
                buf[done..done + n].copy_from_slice(&sector[in_sector..in_sector + n]);
                done += n;
                self.pos += n as u64;
            }
        }
        Ok(done)
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
