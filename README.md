<div align="center">

#  Melody OS
### Bare-Metal Audio Engine (x86_64)

![Build](https://img.shields.io/badge/Build-Passing-brightgreen?style=for-the-badge)
![Arch](https://img.shields.io/badge/Arch-x86__64-blue?style=for-the-badge)
![Lang](https://img.shields.io/badge/Language-Rust-orange?style=for-the-badge&logo=rust)

</div>

---

## ⚡ System Status
**Melody OS** is a bare-metal kernel written from scratch in Rust, designed specifically as a low-latency, lossless audio sequencer for a 20-year-old Dell PC.

By stripping away the overhead of a general-purpose operating system,like context switching, garbage collection pauses, and heavy task schedulers,Melody OS achieves absolute deterministic execution. The CPU's sole purpose is moving audio frames from memory to the hardware with zero jitter.

> "Abstractions are earned, not assumed."

## 💎 Core Architecture

| Component | Implementation |
| :--- | :--- |
| **The Source** | Reading personal, uncompressed WAV and lossless FLAC libraries directly from locally attached hard drives. This involves writing custom **IDE/SATA storage drivers** in Rust to pull data blocks without standard OS filesystem overhead. |
| **The Output** | Targeting bit-perfect digital audio delivery. The current pipeline routes through the internal hardware DAC, with active R&D on direct **S/PDIF** output (and potentially extracting digital audio via HDMI) for zero-degradation transfer to external audiophile equipment. |
| **Headless Control** | Running completely headless. The kernel hosts a lightweight, custom network service that broadcasts its presence on the local network. Remote clients connect to this service to control the playback queue and transport, ensuring no CPU cycles are wasted on a GUI. |
| **Memory Safety** | Implementing memory-safe interrupt handling and lock-free concurrency utilizing Rust's ownership model to ensure uninterrupted, high-fidelity audio buffering. |

## 🚀 Boot Sequence (Installation)

To build and deploy this kernel to bare metal:

```bash
# Clone the repository
git clone https://github.com/Shrimanpro/melody-os.git
cd melody-os

# Build the kernel payload
cargo build --target x86_64-melody_os.json --release

# Create bootable ISO (requires grub-mkrescue)
cargo bootimage

# Flash to USB or boot in QEMU
cargo run
```

## 📜 License
MIT License.

<br><div align="center">
*"Running on high caffeine and low latency."*
</div>


