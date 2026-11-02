use std::path::{Path, PathBuf};

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let kernel = PathBuf::from(std::env::var_os("CARGO_BIN_FILE_KERNEL_kernel").unwrap());

    let bios_path = out_dir.join("bios.img");
    bootloader::BiosBoot::new(&kernel)
        .create_disk_image(&bios_path)
        .unwrap();
    println!("cargo:rustc-env=BIOS_PATH={}", bios_path.display());

    let disk_path = out_dir.join("disk.img");
    make_fat16_disk(&disk_path);
    println!("cargo:rustc-env=DISK_PATH={}", disk_path.display());
}

// --- A from-scratch FAT16 disk image, built by hand instead of shelling
// out to mkfs/newfs_msdos, so the build has no host-tool dependency beyond
// cargo/rustc. It's a real, spec-compliant FAT16 volume -- you can
// `hdiutil attach` (macOS) or mount (Linux) the resulting disk.img and see
// the same files the kernel's own FAT16 reader sees.

const BYTES_PER_SECTOR: u32 = 512;
const SECTORS_PER_CLUSTER: u32 = 4; // 2 KiB clusters
const RESERVED_SECTORS: u32 = 1; // just the boot sector
const NUM_FATS: u32 = 2;
const ROOT_ENTRIES: u32 = 512;
const TOTAL_SECTORS: u32 = 32768; // 16 MiB disk

struct DiskFile {
    name: &'static str, // 8.3, e.g. "README.TXT"
    contents: Vec<u8>,
}

fn make_fat16_disk(path: &Path) {
    let root_dir_sectors = (ROOT_ENTRIES * 32).div_ceil(BYTES_PER_SECTOR);
    let tmp1 = TOTAL_SECTORS - (RESERVED_SECTORS + root_dir_sectors);
    let tmp2 = 256 * SECTORS_PER_CLUSTER + NUM_FATS;
    let fat_size_sectors = tmp1.div_ceil(tmp2);

    let fat_start_sector = RESERVED_SECTORS;
    let root_dir_start_sector = fat_start_sector + NUM_FATS * fat_size_sectors;
    let data_start_sector = root_dir_start_sector + root_dir_sectors;
    let cluster_count = (TOTAL_SECTORS - data_start_sector) / SECTORS_PER_CLUSTER;
    assert!(
        (4085..65525).contains(&cluster_count),
        "cluster count {cluster_count} is out of the FAT16 range -- adjust TOTAL_SECTORS/SECTORS_PER_CLUSTER"
    );

    let cluster_size = SECTORS_PER_CLUSTER * BYTES_PER_SECTOR;

    let files = vec![
        DiskFile {
            name: "README  TXT",
            contents: b"This file lives on a real FAT16-formatted disk image, read through\n\
miniOS's own ATA PIO driver and FAT16 parser (kernel/src/ata.rs and\n\
kernel/src/fat16.rs) -- not compiled into the kernel binary like the\n\
files under plain 'cat'.\n\
\n\
You can mount disk.img on your host machine (hdiutil attach on macOS,\n\
mount -o loop on Linux) and see this same file.\n"
                .to_vec(),
        },
        DiskFile {
            name: "DATA    TXT",
            contents: {
                let mut data = b"1234567890\n\
This is a second file on the disk, to prove directory listing handles\n\
more than one entry, and that files bigger than one cluster (2 KiB) are\n\
read correctly by following the FAT cluster chain rather than just\n\
reading the first cluster and stopping.\n\n"
                    .to_vec();
                // Pad well past one cluster (2048 bytes) with numbered
                // lines, so reading this file genuinely exercises
                // multi-cluster chain-following.
                for i in 0..80 {
                    data.extend_from_slice(format!("padding line {i:03}: 0123456789abcdef\n").as_bytes());
                }
                data.extend_from_slice(b"\nEnd of data.txt.\n");
                data
            },
        },
    ];

    let mut image = vec![0u8; (TOTAL_SECTORS * BYTES_PER_SECTOR) as usize];

    // --- Boot sector / BPB (FAT16) ---
    {
        let b = &mut image[0..512];
        b[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]); // jmp short + nop
        b[3..11].copy_from_slice(b"MINIOS  "); // OEM name, 8 bytes
        b[11..13].copy_from_slice(&(BYTES_PER_SECTOR as u16).to_le_bytes());
        b[13] = SECTORS_PER_CLUSTER as u8;
        b[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
        b[16] = NUM_FATS as u8;
        b[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes());
        b[19..21].copy_from_slice(&(TOTAL_SECTORS as u16).to_le_bytes()); // fits: 32768 < 65536
        b[21] = 0xF8; // media descriptor: fixed disk
        b[22..24].copy_from_slice(&(fat_size_sectors as u16).to_le_bytes());
        b[24..26].copy_from_slice(&32u16.to_le_bytes()); // sectors per track (dummy CHS geometry)
        b[26..28].copy_from_slice(&64u16.to_le_bytes()); // number of heads (dummy)
        b[28..32].copy_from_slice(&0u32.to_le_bytes()); // hidden sectors
        b[32..36].copy_from_slice(&0u32.to_le_bytes()); // total_sectors_32 (unused, we used the 16-bit field)
        b[36] = 0x80; // drive number
        b[37] = 0; // reserved
        b[38] = 0x29; // extended boot signature
        b[39..43].copy_from_slice(&0x4D49_4E49u32.to_le_bytes()); // volume id ("MINI")
        b[43..54].copy_from_slice(b"MINIOS DISK"); // volume label, 11 bytes
        b[54..62].copy_from_slice(b"FAT16   "); // filesystem type, 8 bytes
        b[510] = 0x55;
        b[511] = 0xAA;
    }

    // --- FAT tables (two identical copies) ---
    let mut fat = vec![0u16; cluster_count as usize + 2];
    fat[0] = 0xFFF8;
    fat[1] = 0xFFFF;

    let mut next_free_cluster: u32 = 2;
    let mut first_clusters = Vec::with_capacity(files.len());
    for file in &files {
        let first_cluster = next_free_cluster;
        let clusters_needed = (file.contents.len() as u32).max(1).div_ceil(cluster_size);
        for i in 0..clusters_needed {
            let this_cluster = next_free_cluster;
            next_free_cluster += 1;
            fat[this_cluster as usize] = if i == clusters_needed - 1 {
                0xFFFF
            } else {
                (this_cluster + 1) as u16
            };
        }
        first_clusters.push(first_cluster);
    }
    assert!(
        next_free_cluster <= cluster_count + 2,
        "demo files don't fit on the generated disk"
    );

    let mut fat_bytes = vec![0u8; (fat_size_sectors * BYTES_PER_SECTOR) as usize];
    for (i, entry) in fat.iter().enumerate() {
        fat_bytes[i * 2..i * 2 + 2].copy_from_slice(&entry.to_le_bytes());
    }
    for fat_copy in 0..NUM_FATS {
        let start = ((fat_start_sector + fat_copy * fat_size_sectors) * BYTES_PER_SECTOR) as usize;
        image[start..start + fat_bytes.len()].copy_from_slice(&fat_bytes);
    }

    // --- Root directory ---
    let root_dir_start = (root_dir_start_sector * BYTES_PER_SECTOR) as usize;
    for (i, file) in files.iter().enumerate() {
        let entry_start = root_dir_start + i * 32;
        let entry = &mut image[entry_start..entry_start + 32];
        assert_eq!(file.name.len(), 11, "file name must be in padded 8.3 form");
        entry[0..11].copy_from_slice(file.name.as_bytes());
        entry[11] = 0x20; // attribute: archive
        let first_cluster = first_clusters[i] as u16;
        entry[26..28].copy_from_slice(&first_cluster.to_le_bytes());
        entry[28..32].copy_from_slice(&(file.contents.len() as u32).to_le_bytes());
    }

    // --- File data ---
    for (file, &first_cluster) in files.iter().zip(&first_clusters) {
        let mut cluster = first_cluster;
        for chunk in file.contents.chunks(cluster_size as usize) {
            let sector = data_start_sector + (cluster - 2) * SECTORS_PER_CLUSTER;
            let start = (sector * BYTES_PER_SECTOR) as usize;
            image[start..start + chunk.len()].copy_from_slice(chunk);
            cluster += 1;
        }
    }

    std::fs::write(path, image).unwrap();
}
