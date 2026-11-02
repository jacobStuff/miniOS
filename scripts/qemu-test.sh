#!/usr/bin/env bash
# Builds the kernel, boots it headlessly in QEMU, and reports success/failure
# based on the QemuExitCode the kernel writes to the isa-debug-exit device.
# Usage: scripts/qemu-test.sh
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --quiet

BIOS_IMG=$(find target -name "bios.img" -exec ls -t {} + | head -1)
if [ -z "$BIOS_IMG" ]; then
    echo "no bios.img found after build" >&2
    exit 1
fi

set +e
qemu-system-x86_64 \
    -drive format=raw,file="$BIOS_IMG" \
    -serial stdio \
    -no-reboot \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -display none
STATUS=$?
set -e

# isa-debug-exit encodes the written exit code as (code << 1) | 1.
# QemuExitCode::Success = 0x10 -> 33, QemuExitCode::Failed = 0x11 -> 35.
if [ "$STATUS" -eq 33 ]; then
    echo "=== BOOT OK (exit 33 = QemuExitCode::Success) ==="
    exit 0
elif [ "$STATUS" -eq 35 ]; then
    echo "=== BOOT FAILED (exit 35 = QemuExitCode::Failed / kernel panic) ==="
    exit 1
else
    echo "=== UNEXPECTED QEMU EXIT CODE: $STATUS (kernel likely hung/faulted without signaling) ==="
    exit 1
fi
