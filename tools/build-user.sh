#!/bin/sh
# Build the guest program first: the kernel embeds the ELF with include_bytes!,
# so there is no kernel build without it. Zero dependencies, one rustc.
set -e
cd "$(dirname "$0")/.."
( cd user/hello && cargo build --release --offline )
cp user/hello/target/riscv64gc-unknown-none-elf/release/hello assets/hello.elf
echo "assets/hello.elf: $(wc -c < assets/hello.elf) bytes"
