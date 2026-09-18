/// Trait for a generic block storage device that can read 512-byte sectors.
pub trait BlockDevice {
    fn read_sector(&mut self, lba: u64, buffer: &mut [u8; 512]) -> Result<(), &'static str>;
}
