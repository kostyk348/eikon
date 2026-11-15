#!/bin/sh
# Build eikon and boot it. No crates, no bootloader, no toolchain beyond rustc.
set -e
cd "$(dirname "$0")/.."
cargo build --release --offline
exec qemu-system-riscv64 \
    -machine virt -cpu rv64 -bios default \
    -kernel target/riscv64gc-unknown-none-elf/release/eikon \
    -nographic -monitor none -serial stdio -smp 1 -m 128M
