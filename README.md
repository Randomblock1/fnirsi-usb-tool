# fnirsi-usb-tool

An open-source Rust toolkit for [FNIRSI](https://www.fnirsi.cn/) USB power meters. Provides both a CLI and a native GUI for live data logging, offline recording conversion, and firmware updates — no proprietary Windows software required.

## Supported Devices

| Device | USB VID:PID | Connection |
|--------|-------------|------------|
| FNB48  | `0483:003A` | USB HID |
| FNB48S | `2E3C:0049` | USB HID |
| FNB58  | `2E3C:5558` | USB HID, Bluetooth LE |
| C1     | `0483:003B` | USB HID |
| FNAC28 | `0483:003B` | USB HID |

> **Note:** Only FNB58 support is actively tested. Other devices should work but are unverified.

## Features

- **Live measurement streaming** — voltage, current, power, D+/D−, temperature at ~100 Hz (USB) or ~10 Hz (BLE)
- **Multiple export formats** — CSV, JSON Lines (`.jsonl`), Excel (`.xlsx`), and optional Parquet (`.parquet`)
- **Native GUI** — real-time plots, configurable sample rate decimation, import/export, and energy accumulators (eframe/egui)
- **Offline recording conversion** — convert FNIRSI `.cfn`, CSV, JSONL, XLSX, and optional Parquet files between supported formats
- **DFU firmware updates** — flash `.ufn` firmware files over USB
- **Bluetooth LE support** — scan, connect, and stream data wirelessly from BLE-capable devices
- **CRC validation** — optional packet integrity checking

## Project Structure

```
fnirsi-usb-tool/
├── crates/
│   ├── fnirsi-protocol/   # Protocol library (USB HID, BLE, DFU, packet decoding, file I/O)
│   ├── fnirsi-cli/        # Command-line interface
│   └── fnirsi-gui/        # Native GUI application (eframe/egui)
└── udev/                  # Linux udev rules for non-root USB access
```

## Building

Requires a recent [Rust](https://rustup.rs/) toolchain (edition 2024).

```bash
# Build everything
cargo build --release

# Build the CLI or GUI with optional Parquet support
cargo build --release -p fnirsi-cli --features parquet
cargo build --release -p fnirsi-gui --features parquet

# Build just the CLI
cargo build --release -p fnirsi-cli

# Build just the GUI
cargo build --release -p fnirsi-gui
```

### Linux Dependencies

The GUI requires system libraries for the graphics backend. On Debian/Ubuntu:

```bash
sudo apt install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
    libxkbcommon-dev libssl-dev libgtk-3-dev libudev-dev
```

## Setup

### Linux — udev Rules

USB HID devices require root privileges by default. Install the bundled udev rules to allow regular users to access FNIRSI devices:

```bash
sudo cp udev/99-fnirsi.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

## Usage

### CLI

```bash
# List connected devices (USB + BLE scan)
fnirsi-cli info

# Stream live data to stdout (tab-separated)
fnirsi-cli log

# Stream to a CSV file for 5 minutes
fnirsi-cli log --output data.csv --duration 5m

# Stream as JSON Lines to stdout
fnirsi-cli log --json

# Save as JSON Lines to a file
fnirsi-cli log --output data.jsonl

# Save as Parquet (requires `--features parquet`)
fnirsi-cli log --output data.parquet

# Stop after 1000 samples
fnirsi-cli log --output data.csv --max-samples 1000

# Downsample to 10 Hz
fnirsi-cli log --rate 10

# Stream via Bluetooth LE
fnirsi-cli log --ble

# Enable CRC validation
fnirsi-cli log --crc

# Convert a CFN recording to CSV
fnirsi-cli convert recording.cfn output.csv

# Convert to JSON Lines, Excel, or Parquet
fnirsi-cli convert recording.cfn output.jsonl
fnirsi-cli convert recording.cfn output.xlsx
fnirsi-cli convert recording.cfn output.parquet

# Flash firmware
fnirsi-cli flash Fnb58V1.11.ufn
```

### GUI

```bash
fnirsi-gui
```

The GUI provides:

- USB and Bluetooth LE connectivity
- Real-time voltage, current, power, D+/D−, and temperature plots
- Adjustable sample rate (1–100 Hz) and configurable buffer size
- Live readouts of V, A, W, D+, D−, and temperature
- Energy (Wh) and capacity (mAh) accumulators and plots
- Timed recordings with auto-pause
- Import `.cfn`, `.csv`, `.jsonl`, `.xlsx`, and optional `.parquet` files for offline viewing
- Export captured data to CSV, JSON Lines, Excel, and optional Parquet

## Logging

Both tools use [`tracing`](https://docs.rs/tracing) for diagnostics. Set the `RUST_LOG` environment variable to control log verbosity:

```bash
RUST_LOG=debug fnirsi-cli log
```
