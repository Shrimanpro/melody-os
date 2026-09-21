/// Trait for a generic block storage device that can read 512-byte sectors.
pub trait BlockDevice {
    fn read_sector(&mut self, lba: u64, buffer: &mut [u8; 512]) -> Result<(), &'static str>;
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
}
