#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;

#[macro_use]
pub mod framebuffer;
#[macro_use]
pub mod serial;
pub mod gdt;
pub mod interrupts;
pub mod qemu;

use qemu::{exit_qemu, QemuExitCode};

entry_point!(kernel_main);

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
