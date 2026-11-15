#!/bin/sh
# The same core, built for this machine: cargo reads .cargo/config.toml from the
# working directory, so the host crate has to be built from its own directory.
set -e
cd "$(dirname "$0")/../host"
exec cargo run --release --offline -- "$@"
