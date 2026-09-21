use alloc::string::String;
use alloc::vec::Vec;

/// Trait for a generic block storage device that can read 512-byte sectors.
pub trait BlockDevice {
    fn read_sector(&mut self, lba: u64, buffer: &mut [u8; 512]) -> Result<(), &'static str>;
}

#[derive(Debug, Clone)]
pub struct FatDirEntry {
    pub name: String,
    pub is_directory: bool,
    pub size: u32,
    pub first_cluster: u32,
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
        for i in 0..4 {
            let offset = 446 + i * 16;
            let status = sector[offset];
            let partition_type = sector[offset + 4];
            let lba_start = u32::from_le_bytes([
                sector[offset + 8],
                sector[offset + 9],
                sector[offset + 10],
                sector[offset + 11],
            ]);
            let sector_count = u32::from_le_bytes([
                sector[offset + 12],
                sector[offset + 13],
                sector[offset + 14],
                sector[offset + 15],
            ]);

            if partition_type != 0 && sector_count > 0 {
                partitions[i] = Some(PartitionEntry {
                    status,
                    partition_type,
                    lba_start,
                    sector_count,
                });
            }
        }

        Ok(MasterBootRecord { partitions })
    }
}

#[derive(Debug, Clone)]
pub struct Fat32BootSector {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub num_fats: u8,
    pub sectors_per_fat: u32,
    pub root_cluster: u32,
    pub fat_lba: u64,
    pub data_lba: u64,
}

pub struct Fat32FileSystem<'a, D: BlockDevice> {
    device: &'a mut D,
    pub bpb: Fat32BootSector,
    pub partition_lba: u64,
}

impl<'a, D: BlockDevice> Fat32FileSystem<'a, D> {
    pub fn new(device: &'a mut D, partition_lba: u64) -> Result<Self, &'static str> {
        let mut sector = [0u8; 512];
        device.read_sector(partition_lba, &mut sector)?;

        if sector[510] != 0x55 || sector[511] != 0xAA {
            return Err("Invalid FAT32 boot sector signature");
        }

        let bytes_per_sector = u16::from_le_bytes([sector[11], sector[12]]);
        if bytes_per_sector != 512 {
            return Err("Only 512-byte sectors are supported");
        }
        let sectors_per_cluster = sector[13];
        let reserved_sectors = u16::from_le_bytes([sector[14], sector[15]]);
        let num_fats = sector[16];
        let sectors_per_fat = u32::from_le_bytes([sector[36], sector[37], sector[38], sector[39]]);
        let root_cluster = u32::from_le_bytes([sector[44], sector[45], sector[46], sector[47]]);

        let fat_lba = partition_lba + reserved_sectors as u64;
        let data_lba = fat_lba + (num_fats as u64) * (sectors_per_fat as u64);

        let bpb = Fat32BootSector {
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            num_fats,
            sectors_per_fat,
            root_cluster,
            fat_lba,
            data_lba,
        };

        Ok(Fat32FileSystem {
            device,
            bpb,
            partition_lba,
        })
    }

    pub fn cluster_to_lba(&self, cluster: u32) -> u64 {
        self.bpb.data_lba + ((cluster - 2) as u64) * (self.bpb.sectors_per_cluster as u64)
    }

    pub fn next_cluster(&mut self, current_cluster: u32) -> Result<Option<u32>, &'static str> {
        let fat_offset = (current_cluster as u64) * 4;
        let fat_sector_lba = self.bpb.fat_lba + (fat_offset / 512);
        let sector_offset = (fat_offset % 512) as usize;

        let mut sector = [0u8; 512];
        self.device.read_sector(fat_sector_lba, &mut sector)?;

        let next = u32::from_le_bytes([
            sector[sector_offset],
            sector[sector_offset + 1],
            sector[sector_offset + 2],
            sector[sector_offset + 3],
        ]) & 0x0FFFFFFF;

        if next >= 0x0FFFFFF8 {
            Ok(None) // End of cluster chain
        } else if next == 0x0FFFFFF7 {
            Err("Encountered bad cluster in FAT")
        } else {
            Ok(Some(next))
        }
    }

    pub fn list_directory(&mut self, dir_cluster: u32) -> Result<Vec<FatDirEntry>, &'static str> {
        let mut entries = Vec::new();
        let mut current_cluster = Some(dir_cluster);
        let mut sector_buf = [0u8; 512];

        while let Some(cluster) = current_cluster {
            let start_lba = self.cluster_to_lba(cluster);
            for s in 0..self.bpb.sectors_per_cluster {
                self.device.read_sector(start_lba + s as u64, &mut sector_buf)?;

                for entry_idx in 0..(512 / 32) {
                    let offset = entry_idx * 32;
                    let first_byte = sector_buf[offset];
                    if first_byte == 0x00 {
                        // End of directory entries
                        return Ok(entries);
                    }
                    if first_byte == 0xE5 {
                        // Deleted entry
                        continue;
                    }

                    let attr = sector_buf[offset + 11];
                    if attr == 0x0F {
                        // Long filename entry (skip for basic parser)
                        continue;
                    }

                    // Format 8.3 filename
                    let mut name = String::new();
                    let raw_name = &sector_buf[offset..offset + 8];
                    let raw_ext = &sector_buf[offset + 8..offset + 11];

                    for &b in raw_name {
                        if b != b' ' {
                            name.push(b as char);
                        }
                    }
                    let mut ext = String::new();
                    for &b in raw_ext {
                        if b != b' ' {
                            ext.push(b as char);
                        }
                    }
                    if !ext.is_empty() {
                        name.push('.');
                        name.push_str(&ext);
                    }

                    let is_directory = (attr & 0x10) != 0;
                    let cluster_high = u16::from_le_bytes([sector_buf[offset + 20], sector_buf[offset + 21]]) as u32;
                    let cluster_low = u16::from_le_bytes([sector_buf[offset + 26], sector_buf[offset + 27]]) as u32;
                    let file_first_cluster = (cluster_high << 16) | cluster_low;
                    let size = u32::from_le_bytes([
                        sector_buf[offset + 28],
                        sector_buf[offset + 29],
                        sector_buf[offset + 30],
                        sector_buf[offset + 31],
                    ]);

                    entries.push(FatDirEntry {
                        name,
                        is_directory,
                        size,
                        first_cluster: file_first_cluster,
                    });
                }
            }

            current_cluster = self.next_cluster(cluster)?;
        }

        Ok(entries)
    }
}
