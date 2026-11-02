//! A minimal read-only FAT16 driver, layered on `ata::read_sector`.
//!
//! Supports exactly what the shell needs: list the root directory, read a
//! file by following its FAT cluster chain. No subdirectories, no writes,
//! no long filenames -- just classic 8.3 names in a flat root directory,
//! which is all `build.rs`'s disk image generator produces anyway.

use crate::ata;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Once;

const SECTOR_SIZE: usize = 512;

struct Fat16Info {
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    fat_start_sector: u32,
    root_dir_start_sector: u32,
    root_dir_sectors: u32,
    data_start_sector: u32,
}

static FS: Once<Fat16Info> = Once::new();

fn fs() -> &'static Fat16Info {
    FS.call_once(|| {
        let mut boot_sector = [0u8; SECTOR_SIZE];
        ata::read_sector(0, &mut boot_sector).expect("failed to read FAT16 boot sector");

        assert_eq!(
            (boot_sector[510], boot_sector[511]),
            (0x55, 0xAA),
            "disk1 does not have a valid FAT boot sector signature"
        );

        let bytes_per_sector = u16::from_le_bytes([boot_sector[11], boot_sector[12]]);
        let sectors_per_cluster = boot_sector[13];
        let reserved_sectors = u16::from_le_bytes([boot_sector[14], boot_sector[15]]);
        let num_fats = boot_sector[16];
        let root_entries = u16::from_le_bytes([boot_sector[17], boot_sector[18]]);
        let fat_size_sectors = u16::from_le_bytes([boot_sector[22], boot_sector[23]]);

        let root_dir_sectors =
            (root_entries as u32 * 32).div_ceil(bytes_per_sector as u32);
        let fat_start_sector = reserved_sectors as u32;
        let root_dir_start_sector = fat_start_sector + num_fats as u32 * fat_size_sectors as u32;
        let data_start_sector = root_dir_start_sector + root_dir_sectors;

        Fat16Info {
            bytes_per_sector,
            sectors_per_cluster,
            fat_start_sector,
            root_dir_start_sector,
            root_dir_sectors,
            data_start_sector,
        }
    })
}

pub struct DirEntry {
    pub name: String,
    pub size: u32,
    first_cluster: u16,
}

pub fn list() -> Vec<DirEntry> {
    let info = fs();
    let mut entries = Vec::new();
    let mut buf = [0u8; SECTOR_SIZE];

    'sectors: for s in 0..info.root_dir_sectors {
        ata::read_sector(info.root_dir_start_sector + s, &mut buf)
            .expect("failed to read FAT16 root directory");
        for raw_entry in buf.chunks_exact(32) {
            match raw_entry[0] {
                0x00 => break 'sectors, // end of directory
                0xE5 => continue,       // deleted entry
                _ => {}
            }
            let attributes = raw_entry[11];
            if attributes & 0x08 != 0 || attributes & 0x10 != 0 {
                continue; // volume label or subdirectory; we don't handle either
            }

            let name = format_8_3(&raw_entry[0..8], &raw_entry[8..11]);
            let first_cluster = u16::from_le_bytes([raw_entry[26], raw_entry[27]]);
            let size = u32::from_le_bytes([
                raw_entry[28],
                raw_entry[29],
                raw_entry[30],
                raw_entry[31],
            ]);
            entries.push(DirEntry {
                name,
                size,
                first_cluster,
            });
        }
    }

    entries
}

pub fn read(name: &str) -> Option<Vec<u8>> {
    let entry = list()
        .into_iter()
        .find(|e| e.name.eq_ignore_ascii_case(name))?;
    let info = fs();

    let mut data = Vec::with_capacity(entry.size as usize);
    let mut cluster = entry.first_cluster;
    let mut buf = [0u8; SECTOR_SIZE];

    while (0x0002..0xFFF8).contains(&cluster) {
        let start_sector =
            info.data_start_sector + (cluster as u32 - 2) * info.sectors_per_cluster as u32;
        for s in 0..info.sectors_per_cluster as u32 {
            ata::read_sector(start_sector + s, &mut buf).expect("failed to read file data");
            data.extend_from_slice(&buf);
        }
        cluster = next_cluster(cluster);
    }

    data.truncate(entry.size as usize);
    Some(data)
}

fn next_cluster(cluster: u16) -> u16 {
    let info = fs();
    let byte_offset = cluster as u32 * 2;
    let sector = info.fat_start_sector + byte_offset / info.bytes_per_sector as u32;
    let offset_in_sector = (byte_offset % info.bytes_per_sector as u32) as usize;

    let mut buf = [0u8; SECTOR_SIZE];
    ata::read_sector(sector, &mut buf).expect("failed to read FAT table");
    u16::from_le_bytes([buf[offset_in_sector], buf[offset_in_sector + 1]])
}

fn format_8_3(raw_name: &[u8], raw_ext: &[u8]) -> String {
    let name = core::str::from_utf8(raw_name).unwrap_or("").trim_end();
    let ext = core::str::from_utf8(raw_ext).unwrap_or("").trim_end();
    if ext.is_empty() {
        String::from(name)
    } else {
        format!("{name}.{ext}")
    }
}
