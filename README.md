# fnirsi-usb-tool

An open-source Rust toolkit for [FNIRSI](https://www.fnirsi.cn/) USB power meters. Provides both a CLI and GUI for live data logging, offline recording conversion, and firmware updates.

![Screenshot_20260308_145318](https://github.com/user-attachments/assets/5a1b73d5-54ce-4389-a448-8cc5e00b8c5c)


## Supported Devices

| Device | USB VID:PID | Connection |
|--------|-------------|------------|
| FNB48  | `0483:003A` | USB HID |
| FNB48S | `2E3C:0049` | USB HID |
| FNB58  | `2E3C:5558` | USB HID, Bluetooth LE |
| C1     | `0483:003B` | USB HID |
| FNAC28 | `0483:003B` | USB HID |

> **Note:** Only FNB58 support has been tested. Other devices should work but are unverified.

## Features

- **Live measurement streaming** — voltage, current, power, D+/D−, temperature at 100 Hz (USB) or 10 Hz (BLE)
- **Multiple export formats** — CSV, JSON Lines (`.jsonl`), Excel (`.xlsx`), and optionally Parquet (`.parquet`)
- **Native GUI** — real-time plots, configurable sample rate decimation, import/export, and energy accumulators (eframe/egui)
- **Offline recording conversion** — convert FNIRSI `.cfn`, CSV, JSONL, XLSX, and optional Parquet files between supported formats
- **DFU firmware updates** — flash `.ufn` firmware files over USB
- **Bluetooth LE support** — scan, connect, and stream data wirelessly from BLE-capable devices
- **CRC validation** — optional packet integrity checking

## Installation

Requires a toolchain that support Rust 2024 edition.

```bash
# Download
git clone https://github.com/Randomblock1/fnirsi-usb-tool
cd fnirsi-usb-tool

# Without Parquet support
cargo install --path crates/fnirsi-cli
cargo install --path crates/fnirsi-gui

# With Parquet support
cargo install --path crates/fnirsi-cli --features parquet
cargo install --path crates/fnirsi-gui --features parquet
```

### Linux Dependencies

The eframe GUI requires system libraries for the graphics backend. On Debian/Ubuntu:

```bash
sudo apt install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev
```

## Setup

### Linux udev Rules

USB HID devices require root privileges by default. Install the bundled udev rules to allow regular users to access FNIRSI devices:

```bash
sudo cp udev/99-fnirsi.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
```

## Usage

### GUI

```bash
fnirsi-gui
```

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

# Save as Parquet (requires `parquet` feature)
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
