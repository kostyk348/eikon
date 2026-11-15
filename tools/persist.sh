#!/bin/sh
# Two boots, one disk: prove that a generation survives a power cycle.
#   boot 1: commit a form and save the image
#   boot 2: boot again and read the image back
set -e
cd "$(dirname "$0")/.."
cargo build --release --offline -q
DISK=${1:-/tmp/eikon-disk.img}
dd if=/dev/zero of="$DISK" bs=1M count=4 status=none
QEMU="qemu-system-riscv64 -machine virt -cpu rv64 -bios default \
  -kernel target/riscv64gc-unknown-none-elf/release/eikon \
  -drive file=$DISK,format=raw,if=none,id=d0 -device virtio-blk-device,drive=d0 \
  -nographic -monitor none -serial stdio -smp 1 -m 128M"

echo "=== boot 1: commit a form and save the image ==="
( sleep 10; printf "(commit (list 'plant 42 7))\n"; sleep 0.5; printf "(save)\n"; sleep 0.5 ) | \
  timeout 30 $QEMU 2>&1 | sed -n '/eikon repl/,$p' | head -8

echo "=== boot 2: the same disk, nothing typed ==="
timeout 20 $QEMU 2>&1 | sed -n '/virtio-blk/,/self-test/p' | head -4
