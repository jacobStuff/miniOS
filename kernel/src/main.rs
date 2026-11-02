#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;

#[macro_use]
pub mod framebuffer;
#[macro_use]
pub mod serial;
pub mod allocator;
pub mod gdt;
pub mod interrupts;
pub mod memory;
pub mod qemu;

use qemu::{exit_qemu, QemuExitCode};

/// The default bootloader config does *not* map all physical memory, since
/// most kernels don't need it. We do, to walk/modify page tables and hand
/// out physical frames for the heap.
pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // Interrupts stay off until init() below has a GDT/TSS (for the
    // double-fault IST stack), an IDT, and a remapped PIC all in place.
    x86_64::instructions::interrupts::disable();

    serial_println!("miniOS booting (serial)...");

    if let Some(fb) = boot_info.framebuffer.as_mut() {
        framebuffer::init(fb);
    }

    serial_println!("memory regions: {}", boot_info.memory_regions.len());

    println!("miniOS booting...");
    println!("Hello, World!");

    interrupts::init();
    serial_println!("GDT/IDT/PIC initialized, interrupts enabled");

    // Deliberately trigger a breakpoint exception to prove the IDT handler
    // runs and execution resumes afterwards, instead of faulting.
    x86_64::instructions::interrupts::int3();
    serial_println!("resumed after int3 -- breakpoint handler worked");

    println!("Interrupts online.");

    let physical_memory_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("bootloader did not map physical memory (check BOOTLOADER_CONFIG)");
    let physical_memory_offset = x86_64::VirtAddr::new(physical_memory_offset);

    let mut mapper = unsafe { memory::init(physical_memory_offset) };
    let mut frame_allocator = unsafe { memory::BootInfoFrameAllocator::init(&boot_info.memory_regions) };

    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap initialization failed");
    serial_println!("heap mapped at {:#x}, {} bytes", allocator::HEAP_START, allocator::HEAP_SIZE);

    // Exercise the allocator: a Box (single heap allocation) and a Vec that
    // grows past its initial capacity (multiple allocations/reallocations).
    let boxed = Box::new(41);
    serial_println!("boxed value at {:p}: {}", boxed, *boxed);

    let mut vec = Vec::new();
    for i in 0..500 {
        vec.push(i);
    }
    serial_println!("vec len={} sum={}", vec.len(), vec.iter().sum::<i32>());
    assert_eq!(vec.iter().sum::<i32>(), (0..500).sum());

    println!("Heap allocator online.");

    // Wait for a handful of timer ticks to prove the PIC remap + timer
    // handler are actually alive, then report success.
    while interrupts::tick_count() < 5 {
        x86_64::instructions::hlt();
    }
    serial_println!("observed {} timer ticks", interrupts::tick_count());

    #[cfg(feature = "halt_for_screenshot")]
    loop {
        x86_64::instructions::hlt();
    }
    #[cfg(not(feature = "halt_for_screenshot"))]
    exit_qemu(QemuExitCode::Success);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    serial_println!("PANIC: {}", info);
    exit_qemu(QemuExitCode::Failed);
}
