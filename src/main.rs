use std::env;
use std::process::{exit, Command};

fn main() {
    let bios_path = env!("BIOS_PATH");

    let show_display = env::args().any(|arg| arg == "--display");

    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.arg("-drive").arg(format!("format=raw,file={bios_path}"));
    cmd.arg("-serial").arg("mon:stdio");
    cmd.arg("-device")
        .arg("isa-debug-exit,iobase=0xf4,iosize=0x04");

    if !show_display {
        cmd.arg("-display").arg("none");
    }

    let mut child = cmd.spawn().expect("failed to start qemu-system-x86_64");
    let status = child.wait().expect("failed to wait on qemu");

    match status.code().unwrap_or(2) {
        0x10 => exit(0), // kernel signaled success
        0x11 => exit(1), // kernel signaled failure
        other => exit(other),
    }
}
