#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(melody_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use melody_os::fs::{find_fat_volume, scan_library, AudioKind, BlockDevice, FileReader};

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    melody_os::init_with_memory(boot_info);
    test_main();
    melody_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    melody_os::test_panic_handler(info)
}

struct RamDisk(Vec<u8>);

impl BlockDevice for RamDisk {
    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        let start = lba as usize * 512;
        let src = self.0.get(start..start + buf.len()).ok_or("read past end")?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn sector_count(&self) -> Option<u64> {
        Some((self.0.len() / 512) as u64)
    }
}

const SECTORS: usize = 200;
const PART: usize = 1; // partition starts at LBA 1
const FAT: usize = PART + 2;
const DATA: usize = FAT + 1;

fn cluster_off(c: usize) -> usize {
    (DATA + c - 2) * 512
}

fn short_entry(name: &[u8; 11], attr: u8, cluster: u32, size: u32) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0..11].copy_from_slice(name);
    e[11] = attr;
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

/// Long-name entries for `name`, in on-disk order (last fragment first).
fn lfn_entries(name: &str, short: &[u8; 11]) -> Vec<[u8; 32]> {
    let mut sum: u8 = 0;
    for &c in short {
        sum = (sum >> 1 | sum << 7).wrapping_add(c);
    }
    let mut units: Vec<u16> = name.bytes().map(|b| b as u16).collect();
    units.push(0);
    while units.len() % 13 != 0 {
        units.push(0xFFFF);
    }
    let parts = units.len() / 13;
    let offsets = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    let mut out = Vec::new();
    for i in (0..parts).rev() {
        let mut e = [0u8; 32];
        e[0] = (i + 1) as u8 | if i == parts - 1 { 0x40 } else { 0 };
        e[11] = 0x0F;
        e[13] = sum;
        for (k, &o) in offsets.iter().enumerate() {
            e[o..o + 2].copy_from_slice(&units[i * 13 + k].to_le_bytes());
        }
        out.push(e);
    }
    out
}

fn put_dir(img: &mut [u8], cluster: usize, entries: &[[u8; 32]]) {
    let base = cluster_off(cluster);
    for (i, e) in entries.iter().enumerate() {
        img[base + i * 32..base + i * 32 + 32].copy_from_slice(e);
    }
}

fn file_bytes() -> Vec<u8> {
    (0..700u32).map(|i| (i * 7 + 3) as u8).collect()
}

/// MBR + tiny FAT32: "/Artist Name/Song Name.flac" (700 bytes, fragmented over
/// clusters 5 and 8) and "/NOTES.TXT".
fn build_image() -> RamDisk {
    let mut img = vec![0u8; SECTORS * 512];
    // MBR
    img[446] = 0x80;
    img[446 + 4] = 0x0C;
    img[446 + 8..446 + 12].copy_from_slice(&(PART as u32).to_le_bytes());
    img[446 + 12..446 + 16].copy_from_slice(&((SECTORS - PART) as u32).to_le_bytes());
    img[510] = 0x55;
    img[511] = 0xAA;
    // FAT32 boot sector
    let b = PART * 512;
    img[b + 3..b + 11].copy_from_slice(b"MSWIN4.1");
    img[b + 11..b + 13].copy_from_slice(&512u16.to_le_bytes());
    img[b + 13] = 1; // sectors per cluster
    img[b + 14..b + 16].copy_from_slice(&2u16.to_le_bytes()); // reserved
    img[b + 16] = 1; // one FAT
    img[b + 32..b + 36].copy_from_slice(&((SECTORS - PART) as u32).to_le_bytes());
    img[b + 36..b + 40].copy_from_slice(&1u32.to_le_bytes()); // FAT size
    img[b + 44..b + 48].copy_from_slice(&2u32.to_le_bytes()); // root cluster
    img[b + 71..b + 82].copy_from_slice(b"TESTSTICK  ");
    img[b + 510] = 0x55;
    img[b + 511] = 0xAA;
    // FAT
    let fat = [0x0FFF_FFF8u32, 0xFFFF_FFFF, 0x0FFF_FFFF, 0x0FFF_FFFF, 0x0FFF_FFFF, 8, 0, 0, 0x0FFF_FFFF];
    for (i, v) in fat.iter().enumerate() {
        img[FAT * 512 + i * 4..FAT * 512 + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    // Root directory
    let dir_short = *b"ARTIST~1   ";
    let mut root = lfn_entries("Artist Name", &dir_short);
    root.push(short_entry(&dir_short, 0x10, 3, 0));
    root.push(short_entry(b"NOTES   TXT", 0x20, 4, 5));
    put_dir(&mut img, 2, &root);
    // Artist directory
    let song_short = *b"SONGNA~1FLA";
    let mut artist = vec![short_entry(b".          ", 0x10, 3, 0), short_entry(b"..         ", 0x10, 0, 0)];
    artist.extend(lfn_entries("Song Name.flac", &song_short));
    artist.push(short_entry(&song_short, 0x20, 5, 700));
    put_dir(&mut img, 3, &artist);
    // File data, fragmented
    let data = file_bytes();
    img[cluster_off(5)..cluster_off(5) + 512].copy_from_slice(&data[..512]);
    img[cluster_off(8)..cluster_off(8) + 188].copy_from_slice(&data[512..]);
    RamDisk(img)
}

#[test_case]
fn mounts_and_finds_long_names() {
    let mut disk = build_image();
    let mut fs = find_fat_volume(&mut disk).expect("mount");
    assert_eq!(fs.label, "TESTSTICK");
    let tracks = scan_library(&mut fs, &mut disk);
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].path, "Artist Name/Song Name.flac");
    assert_eq!(tracks[0].title(), "Song Name");
    assert_eq!(tracks[0].kind, AudioKind::Flac);
}

#[test_case]
fn reads_fragmented_file_and_seeks() {
    let mut disk = build_image();
    let mut fs = find_fat_volume(&mut disk).unwrap();
    let t = scan_library(&mut fs, &mut disk).remove(0);
    let mut r = FileReader::new(t.first_cluster, t.size);
    let mut buf = vec![0u8; 1024];
    let n = r.read(&mut fs, &mut disk, &mut buf).unwrap();
    assert_eq!(n, 700);
    assert_eq!(&buf[..700], &file_bytes()[..]);
    assert_eq!(r.read(&mut fs, &mut disk, &mut buf).unwrap(), 0);

    r.seek(&mut fs, &mut disk, 600).unwrap();
    let n = r.read(&mut fs, &mut disk, &mut buf).unwrap();
    assert_eq!(&buf[..n], &file_bytes()[600..]);
}

#[test_case]
fn rejects_code_sector_as_mbr() {
    // A boot sector full of code ends in 0x55AA too: no bogus partitions may be tried.
    let mut img = vec![0x90u8; 64 * 512];
    img[510] = 0x55;
    img[511] = 0xAA;
    assert!(find_fat_volume(&mut RamDisk(img)).is_err());
}
