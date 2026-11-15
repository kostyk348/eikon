#!/bin/sh
# Drive the REPL from a file of forms, one per line, after the boot self-test.
#   tools/repl.sh program.eikon
# (the emulator muxes stdin between monitor and serial, so -monitor none is what
#  makes typed input reach the kernel instead of the qemu monitor)
set -e
cd "$(dirname "$0")/.."
cargo build --release --offline
SCRIPT=${1:?usage: tools/repl.sh <file-of-forms>}
( sleep 8; while IFS= read -r line; do printf '%s\n' "$line"; sleep 0.2; done < "$SCRIPT"; sleep 1 ) | \
timeout 40 qemu-system-riscv64 \
    -machine virt -cpu rv64 -bios default \
    -kernel target/riscv64gc-unknown-none-elf/release/eikon \
    -nographic -monitor none -serial stdio -smp 1 -m 128M || true
