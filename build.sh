#!/bin/bash
# Build the Melody OS boot image.
#
# The repository lives on an NTFS drive where cargo's build scripts can't be executed
# ("Permission denied (os error 13)"), so the build output goes to a native filesystem.
# Usage: ./build.sh            -> prints the path of the image to flash

set -e
cd "$(dirname "$0")/melody_os"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/melody-os/target}"
cargo bootimage --release
IMG="$CARGO_TARGET_DIR/x86_64-melody/release/bootimage-melody_os.bin"
echo
echo "Boot image: $IMG"
echo "Flash with: sudo dd if=$IMG of=/dev/sdX bs=4M conv=fsync"
