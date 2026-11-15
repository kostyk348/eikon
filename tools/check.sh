#!/bin/sh
# Boot the kernel and fail unless the self-test passes. This is the gate CI runs.
set -e
cd "$(dirname "$0")/.."
cargo build --release --offline
LOG=$(mktemp)
# The kernel halts after the report, so qemu is expected to be killed by the timeout.
timeout 30 qemu-system-riscv64 \
    -machine virt -cpu rv64 -bios default \
    -kernel target/riscv64gc-unknown-none-elf/release/eikon \
    -nographic -monitor none -serial stdio -smp 1 -m 128M > "$LOG" 2>&1 || true
sed -n '/=== boot self-test ===/,$p' "$LOG"
if ! grep -q "=== PASS (failures: 0) ===" "$LOG"; then
    echo "FAILED: boot self-test did not pass" >&2
    echo "--- qemu output (tail) ---" >&2
    tail -30 "$LOG" >&2
    exit 1
fi
echo "OK: booted, self-test passed"
