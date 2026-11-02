#![no_std]
#![no_main]

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;

#[macro_use]
pub mod framebuffer;
#[macro_use]
pub mod serial;
pub mod qemu;

use qemu::{exit_qemu, QemuExitCode};

entry_point!(kernel_main);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // No IDT exists yet, so keep interrupts masked until the next milestone
    // sets one up (otherwise the PIT timer fires straight into an empty
    // IDT and we start taking faults for no reason).
    x86_64::instructions::interrupts::disable();

    serial_println!("miniOS booting (serial)...");

    if let Some(fb) = boot_info.framebuffer.as_mut() {
        let info = fb.info();
        serial_println!(
            "fb ptr={:p} len={} w={} h={} stride={} bpp={} fmt={:?}",
            fb.buffer_mut().as_ptr(),
            fb.buffer_mut().len(),
            info.width,
            info.height,
            info.stride,
            info.bytes_per_pixel,
            info.pixel_format
        );
        framebuffer::init(fb);
    }

    serial_println!("memory regions: {}", boot_info.memory_regions.len());

    println!("miniOS booting...");
    println!("Hello, World!");

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
