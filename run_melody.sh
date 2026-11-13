#!/bin/bash
# Build Melody OS and boot it in QEMU with hardware close to the real target:
#   - Intel HDA sound (played on the host through PulseAudio/PipeWire)
#   - a USB 2.0 stick holding your music (a FAT32 disk image)
#   - an Intel e1000 NIC; the MPD server is forwarded to localhost:16600
#
# Usage: ./run_melody.sh [music.img]
#   Make a music image with:  ./run_melody.sh --make-image ~/Music/some-folder music.img
#
# The project lives on an NTFS drive where freshly built build scripts can't be executed,
# so build output goes to a native filesystem (override with CARGO_TARGET_DIR).

set -e
cd "$(dirname "$0")"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/melody-os/target}"

if [ "$1" = "--make-image" ]; then
    src="$2"; img="$3"
    [ -d "$src" ] && [ -n "$img" ] || { echo "usage: $0 --make-image <music dir> <image file>"; exit 1; }
    command -v mcopy >/dev/null || { echo "needs mtools (mcopy): sudo pacman -S mtools"; exit 1; }
    size_mb=$(( $(du -sm "$src" | cut -f1) + 64 ))
    truncate -s "${size_mb}M" "$img"
    mkfs.fat -F 32 -n MUSIC "$img" >/dev/null
    mcopy -s -i "$img" "$src"/* ::/
    echo "Created $img (${size_mb} MB)"
    exit 0
fi

echo "[*] Building Melody OS..."
(cd melody_os && cargo bootimage --release)
BOOT="$CARGO_TARGET_DIR/x86_64-melody/release/bootimage-melody_os.bin"

USB_ARGS=(-device usb-ehci,id=ehci)
if [ -n "$1" ]; then
    USB_ARGS+=(-drive "if=none,id=stick,format=raw,file=$1" -device usb-storage,bus=ehci.0,drive=stick)
else
    echo "[!] No music image given; booting without a USB stick."
fi

AUDIO=pa
command -v pipewire >/dev/null && AUDIO=pipewire

echo "[*] Launching QEMU. MPD: mpc -p 16600 status   Serial log: this terminal"
exec qemu-system-x86_64 -m 1024 \
    -drive format=raw,file="$BOOT" \
    "${USB_ARGS[@]}" \
    -device intel-hda -device hda-output,audiodev=a0 -audiodev "$AUDIO",id=a0 \
    -netdev user,id=n0,hostfwd=tcp::16600-:6600 -device e1000,netdev=n0 \
    -serial stdio
