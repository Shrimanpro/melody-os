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
