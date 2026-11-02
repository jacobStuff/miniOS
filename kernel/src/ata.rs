//! A minimal polling-PIO ATA driver, just enough to read sectors off the
//! secondary IDE bus (0x170-0x177) in 28-bit LBA mode. No DMA, no
//! interrupts, no writes -- reading is all the FAT16 driver above it needs.
//!
//! The boot drive lives on the *primary* bus (0x1F0-0x1F7); this
//! deliberately only ever touches the secondary bus, so there's no risk of
//! this driver's bugs corrupting the disk the kernel itself booted from.

use spin::Mutex;
use x86_64::instructions::port::Port;

const IO_BASE: u16 = 0x170;
const DATA: u16 = IO_BASE;
const ERROR: u16 = IO_BASE + 1;
const SECTOR_COUNT: u16 = IO_BASE + 2;
const LBA_LOW: u16 = IO_BASE + 3;
const LBA_MID: u16 = IO_BASE + 4;
const LBA_HIGH: u16 = IO_BASE + 5;
const DRIVE_HEAD: u16 = IO_BASE + 6;
const STATUS_COMMAND: u16 = IO_BASE + 7;

const CMD_READ_SECTORS: u8 = 0x20;

const STATUS_ERR: u8 = 1 << 0;
const STATUS_DRQ: u8 = 1 << 3;
const STATUS_BSY: u8 = 1 << 7;

/// Serializes access to the shared I/O ports; the ATA protocol is
/// inherently stateful (select drive, then issue command, then read data)
/// and not safe to interleave across callers.
static ATA_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug)]
pub enum AtaError {
    Timeout,
    DeviceError(u8),
}

/// Reads one 512-byte sector at `lba` from the secondary bus's master
/// drive.
pub fn read_sector(lba: u32, buf: &mut [u8; 512]) -> Result<(), AtaError> {
    assert!(lba < (1 << 28), "LBA28 only supports 28-bit addresses");

    let _guard = ATA_LOCK.lock();

    let mut drive_head: Port<u8> = Port::new(DRIVE_HEAD);
    let mut sector_count: Port<u8> = Port::new(SECTOR_COUNT);
    let mut lba_low: Port<u8> = Port::new(LBA_LOW);
    let mut lba_mid: Port<u8> = Port::new(LBA_MID);
    let mut lba_high: Port<u8> = Port::new(LBA_HIGH);
    let mut command: Port<u8> = Port::new(STATUS_COMMAND);
    let mut status: Port<u8> = Port::new(STATUS_COMMAND);
    let mut error: Port<u8> = Port::new(ERROR);
    let mut data: Port<u16> = Port::new(DATA);

    unsafe {
        wait_not_busy(&mut status)?;

        // 0xE0 = LBA mode + master drive; top nibble of LBA goes in the low
        // 4 bits of this register.
        drive_head.write(0xE0 | ((lba >> 24) & 0x0F) as u8);
        sector_count.write(1);
        lba_low.write(lba as u8);
        lba_mid.write((lba >> 8) as u8);
        lba_high.write((lba >> 16) as u8);
        command.write(CMD_READ_SECTORS);

        wait_drq_or_error(&mut status, &mut error)?;

        for chunk in buf.chunks_exact_mut(2) {
            let word = data.read();
            chunk.copy_from_slice(&word.to_le_bytes());
        }
    }

    Ok(())
}

const MAX_POLL_ITERATIONS: u32 = 10_000_000;

unsafe fn wait_not_busy(status: &mut Port<u8>) -> Result<(), AtaError> {
    for _ in 0..MAX_POLL_ITERATIONS {
        if unsafe { status.read() } & STATUS_BSY == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(AtaError::Timeout)
}

unsafe fn wait_drq_or_error(status: &mut Port<u8>, error: &mut Port<u8>) -> Result<(), AtaError> {
    for _ in 0..MAX_POLL_ITERATIONS {
        let s = unsafe { status.read() };
        if s & STATUS_ERR != 0 {
            return Err(AtaError::DeviceError(unsafe { error.read() }));
        }
        if s & STATUS_BSY == 0 && s & STATUS_DRQ != 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(AtaError::Timeout)
}
