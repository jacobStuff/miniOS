#!/usr/bin/env python3
"""Builds the kernel, boots it headlessly, scripts a few shell commands in
through the QEMU monitor's `sendkey`, and checks the result.

Replaces scripts/qemu-test.sh now that the kernel ends in an interactive
shell instead of self-exiting: there's no longer a "just boot and wait"
story, so the regression test has to drive the shell itself.
"""

import json
import os
import socket
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# QEMU `sendkey` names for the characters our scripted commands use.
KEY_NAMES = {
    " ": "spc",
    ".": "dot",
    "\n": "ret",
}


def keys_for(text):
    for ch in text:
        yield KEY_NAMES.get(ch, ch)


def find_newest(filename):
    newest = None
    newest_mtime = -1
    for dirpath, _dirs, files in os.walk(os.path.join(ROOT, "target")):
        if filename in files:
            path = os.path.join(dirpath, filename)
            mtime = os.path.getmtime(path)
            if mtime > newest_mtime:
                newest_mtime = mtime
                newest = path
    return newest


def qmp_connect(sock_path, timeout=10):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if os.path.exists(sock_path):
            try:
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.connect(sock_path)
                return s
            except OSError:
                pass
        time.sleep(0.1)
    raise TimeoutError(f"QMP socket {sock_path} never appeared")


def qmp_cmd(f, payload):
    f.write(json.dumps(payload).encode() + b"\n")
    f.flush()
    return json.loads(f.readline())


def wait_for_serial(path, marker, timeout=15):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if os.path.exists(path):
            with open(path, "r", errors="replace") as fh:
                content = fh.read()
                if marker in content:
                    return content
        time.sleep(0.2)
    raise TimeoutError(f"serial log never contained {marker!r} within {timeout}s")


def main():
    subprocess.run(["cargo", "build", "--quiet"], cwd=ROOT, check=True)

    bios_img = find_newest("bios.img")
    disk_img = find_newest("disk.img")
    if not bios_img:
        print("no bios.img found after build", file=sys.stderr)
        return 1
    if not disk_img:
        print("no disk.img found after build", file=sys.stderr)
        return 1

    serial_log = "/tmp/minios_test_serial.log"
    qmp_sock = "/tmp/minios_test_qmp.sock"
    for p in (serial_log, qmp_sock):
        if os.path.exists(p):
            os.remove(p)

    proc = subprocess.Popen(
        [
            "qemu-system-x86_64",
            "-drive", f"format=raw,file={bios_img}",
            "-drive", f"id=disk1,if=none,format=raw,file={disk_img}",
            "-device", "ide-hd,drive=disk1,bus=ide.1,unit=0",
            "-serial", f"file:{serial_log}",
            "-qmp", f"unix:{qmp_sock},server,nowait",
            "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
            "-display", "none",
        ],
    )

    try:
        # Wait for the shell's greeting -- our signal that boot, interrupts,
        # paging/heap, the scheduler, and the shell task are all up.
        wait_for_serial(serial_log, "shell: ready for input", timeout=20)

        qmp = qmp_connect(qmp_sock)
        f = qmp.makefile("rwb")
        f.readline()  # greeting
        qmp_cmd(f, {"execute": "qmp_capabilities"})

        commands = [
            "ls",
            "cat readme.txt",
            "echo hi there",
            "lsdisk",
            "catdisk readme.txt",
            "catdisk data.txt",
            "run",
            "run",
            "exit",
        ]
        for cmd in commands:
            for key in keys_for(cmd + "\n"):
                reply = qmp_cmd(f, {
                    "execute": "human-monitor-command",
                    "arguments": {"command-line": f"sendkey {key}"},
                })
                if "error" in reply:
                    print(f"sendkey {key!r} failed: {reply}", file=sys.stderr)
                time.sleep(0.05)
            time.sleep(0.2)

        try:
            status = proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
            print("=== TEST FAILED: QEMU never exited after 'exit' command ===")
            print(open(serial_log, errors="replace").read())
            return 1

        serial_output = open(serial_log, errors="replace").read()
        print(serial_output)

        checks = [
            ("readme.txt" in serial_output, "ls should list readme.txt"),
            ("Welcome to miniOS" in serial_output, "cat readme.txt should print its contents"),
            ("hi there" in serial_output, "echo hi there should echo its args"),
            ("README.TXT" in serial_output, "lsdisk should list README.TXT from the FAT16 disk"),
            ("DATA.TXT" in serial_output, "lsdisk should list DATA.TXT from the FAT16 disk"),
            (
                "own ATA PIO driver and FAT16 parser" in serial_output,
                "catdisk readme.txt should print the real file contents read via ATA/FAT16",
            ),
            (
                "End of data.txt." in serial_output,
                "catdisk data.txt should read all the way through a multi-cluster file",
            ),
            (
                serial_output.count("process: entering ring 3") == 2,
                "run should enter ring 3 both times it's invoked",
            ),
            (
                serial_output.count("Hello from ring 3!") == 2,
                "the ring-3 process's write syscall should print both times",
            ),
            (
                serial_output.count("process: back in the kernel, exit code 42") == 2,
                "both ring-3 runs should return to the kernel with the right exit code",
            ),
            ("shell: exit command received" in serial_output, "exit command should be logged"),
        ]
        ok = all(c for c, _ in checks)
        for c, desc in checks:
            print(f"  [{'OK' if c else 'FAIL'}] {desc}")

        # exit code 33 == QemuExitCode::Success via isa-debug-exit encoding.
        if status == 33 and ok:
            print("=== TEST PASSED ===")
            return 0
        else:
            print(f"=== TEST FAILED (qemu exit status={status}) ===")
            return 1
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()


if __name__ == "__main__":
    sys.exit(main())
