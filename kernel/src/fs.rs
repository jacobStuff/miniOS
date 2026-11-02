//! A deliberately simple "filesystem": a fixed, read-only set of files
//! baked into the kernel binary at compile time (an initrd-style ramdisk,
//! minus the disk -- there's no block device driver yet, so nothing here
//! is backed by persistent storage; everything resets on reboot).
//!
//! A real next step would be an ATA/AHCI driver plus an on-disk format
//! (even something as simple as FAT16), but that's a substantial chunk of
//! work on its own and out of scope for this milestone.

pub struct File {
    pub name: &'static str,
    pub contents: &'static str,
}

pub static FILES: &[File] = &[
    File {
        name: "readme.txt",
        contents: "Welcome to miniOS!\n\nThis \"filesystem\" is just a fixed list of files\ncompiled into the kernel -- see kernel/src/fs.rs.\nType 'help' to see what the shell can do.\n",
    },
    File {
        name: "hello.txt",
        contents: "Hello from a file that isn't really on a disk!\n",
    },
];

pub fn list() -> impl Iterator<Item = &'static str> {
    FILES.iter().map(|f| f.name)
}

pub fn read(name: &str) -> Option<&'static str> {
    FILES.iter().find(|f| f.name == name).map(|f| f.contents)
}
