# lsi-flash

[![CI](https://github.com/MattJackson/lsi-flash/actions/workflows/ci.yml/badge.svg)](https://github.com/MattJackson/lsi-flash/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust: 1.74+](https://img.shields.io/badge/rust-1.74%2B-orange.svg)](https://www.rust-lang.org)
[![Status: pre-release](https://img.shields.io/badge/status-pre--release-yellow.svg)](#status)

> One Linux-native static binary for cross-flashing LSI SAS2008-based HBAs between IT, IR, and OEM identities. Replaces a fragile pile of DOS tools, Python scripts, and abandonware.

```console
$ sudo lsi-flash detect
PCI 03:00.0 — Dell PERC H200 Internal Tape Adapter (1028:1f22)
  Chip:     SAS2008 rev B2
  Firmware: 07.15.08.00 (IT, Dell ITA A04)
  SAS WWN:  5f01faf0e5e06600
  Links:    7 down, 1 up @ 6.0 Gbps

$ sudo lsi-flash backup --output ~/h200-baseline
backup complete: ~/h200-baseline/{firmware.bin, bios.rom, sbr.bin, manifest.toml}

$ sudo lsi-flash flash HBA --as lsi-9211-8i --dry-run
[dry-run] would cross-flash to LSI 9211-8i IT firmware (sha 1a2b3c…)
[dry-run] safety guards: 0 mounted filesystems, 0 LVM PVs, 0 ZFS pools — OK
[dry-run] would write firmware → BIOS → SBR → reset → verify
```

## What this is

A modern Linux CLI for the LSI SAS2008 HBA family (Dell PERC H200, PERC H310, IBM M1015, LSI 9211-8i, etc.) that the homelab / TrueNAS / Proxmox / ZFS communities have been buying used for a decade. The chips are EOL but still ubiquitous because in IT mode they're cheap, reliable HBAs.

The existing toolchain works but is painful:

| Tool | Problem |
|---|---|
| `sas2flash` | x86 only (32-bit ELF from 2011), refuses to flash Dell-OEM cards |
| `lsiutil` | Interactive menu-driven, last released Sept 2014, **bricked our dev card** by silently leaving a partial flash write |
| `lsirec` | Python wrapper + small C bin, packaged for nothing, BAR1 mmap doesn't survive process restart |
| `sbrtool.py` | Python; offline only; no integration |
| `megarec` | 16-bit DOS; you boot FreeDOS off USB to use it |
| `flash-it.sh` | Bash glue holding all of the above together; opinions vary on whether the brick is the tool's fault or the user's |

This project replaces the whole chain with **one statically-linked musl binary** that drops onto any Linux live USB and runs.

## Status

**Pre-release (0.0.x).** Library layer complete; CLI verbs landing.

| Stage | Status | What |
|---|---|---|
| **Stage 1** — Rust port of `lsirec` + `sbrtool` | ✅ Shipped | 10 modules: PCI / SBR / MPI doorbell + diag / HCB hostboot / firmware parse / card DB |
| **Stage 2** — User-facing verbs | ~60% | `detect`, `sbr build/show/verify`, `firmware reverse-phy`, `pci` platform trait shipped. `sbr read/write/modify` pending. |
| **Stage 3** — `flash` orchestrator | ~70% | State machine + safety guards + MPI ops + 12-phase orchestrator + `backup` + `recover` shipped. RealIoc backend (cycle 2a) shipped; cycle 2b in flight. |
| **Stage 4** — Packaging + manipulator features | Mirror ✅; binary packaging 0% | Public firmware mirror at [`MattJackson/lsi-flash-firmware`](https://github.com/MattJackson/lsi-flash-firmware) (~300 firmware blobs covering every public SAS2008 phase + every Dell PERC H200/H310 revision). `firmware reverse-phy` first manipulator feature shipped. |

**Tests:** 173/173 passing. **Hardware-verified:** Dell PERC H200 Internal Tape Adapter (B2), 2026-05-27 baseline.

No tagged release yet. Build from source; see [Install](#install).

## Hardware compatibility

| Vendor | Card | SAS2008? | Supported | Notes |
|---|---|---|---|---|
| **LSI** | 9211-8i, 9210-8i | ✅ | ✅ | Reference cards — IT/IR/BIOS via direct MPI FW_DOWNLOAD |
| **Dell** | PERC H200 Adapter | ✅ | ✅ | Requires `megarec -cleanflash` path on factory firmware — wrapped automatically |
| **Dell** | PERC H200 Integrated/Modular/Embedded/External | ✅ | ⏳ | SBR templates landed; awaiting per-variant hardware confirmation |
| **Dell** | PERC H200 Internal Tape Adapter | ✅ | ✅ | Smaller flash (~1 MB) — refuses oversized firmware automatically. Hardware-verified on dev-1 (sha `48e89775…`). |
| **Dell** | PERC H310 (full, mini-blade, mini-mono) | ✅ | ⏳ | IMR composite format; cross-flash works but requires `FOHDeesha 310MNCRS` style script for mini-mono iDRAC interlock |
| **IBM** | ServeRAID M1015 / M1115 | ✅ | ✅ | M1115 has IBM-chassis DIMM quirk (community-documented) |
| **IBM** | 6Gb SAS HBA 9212-4i4e | ✅ | ⏳ | OEM'd LSI; should crossflash with stock LSI firmware |
| **Intel** | RS2WC080, RMS2LL040, RMS2AF040 | ✅ | ⏳ | Intel doesn't ship Linux flash tools — this fills the gap |
| **Supermicro** | AOC-USAS2-L8e, AOC-USAS2-L8i | ✅ | ⏳ | L8i requires hardware-RAID-disable jumper before flashing |
| **Fujitsu** | D2607 (and D2607-C2) | ✅ | ⏳ | SBR variant preserves `interface_byte = 0x10` for dual-connector wiring |
| **HP** | H220 | ❌ (SAS2308) | Out of scope | Different chip family; would need v2.0 |
| **Cisco** | UCSC RAID SAS 2008M-8i | ✅ | ⏳ | OEM'd LSI; supports JBOD passthrough with stock firmware per community reports |
| **Anything MegaRAID-class** | PERC H700/H710/H730, IBM ServeRAID M5014/M5015 | ❌ (SAS2108/SAS2208 ROC) | Out of scope | Different chip family; different firmware paradigm; hardware-RAID users typically *want* IR mode |

Legend: ✅ shipped + tested · ⏳ DB entry + firmware in manifest, awaiting hardware confirmation · ❌ out of scope.

If your card isn't listed, please file a [hardware-compatibility report](https://github.com/MattJackson/lsi-flash/issues/new?template=hardware_compatibility.yml).

## Install

### From source (only option until v0.1)

```bash
git clone https://github.com/MattJackson/lsi-flash
cd lsi-flash
cargo build --release --target x86_64-unknown-linux-musl
./target/x86_64-unknown-linux-musl/release/lsi-flash --help
```

Requires Rust 1.74+ and the musl target (`rustup target add x86_64-unknown-linux-musl`).

### From release binary (planned for v0.1)

Pre-built `lsi-flash-x86_64-unknown-linux-musl` will be attached to each GitHub release. Drops on any Linux distro — no glibc dependency, no Python, no DOS.

## Usage

> ⚠️ **All flash operations require root** (`sudo`). The tool refuses with a clear error if not.

### Detect cards

```bash
sudo lsi-flash detect
```

Scans the PCI bus for SAS2008 cards, parses their SBR, reads firmware version + SAS WWN. Output is human-readable by default; pass `--json` for machine consumption.

### Backup before any destructive op

```bash
sudo lsi-flash backup --pci 0000:03:00.0 --output /var/lib/lsi-flash/backups/$(date +%F)
```

Captures firmware + BIOS + SBR + manifest.toml. **Always run this before `flash`.** The default backup directory is `/var/lib/lsi-flash/backups/` — explicitly NOT `/tmp` (which `flash-it.sh` famously loses on reboot).

### Cross-flash

```bash
# Dry-run first — ALWAYS
sudo lsi-flash flash HBA --as lsi-9211-8i --dry-run

# Real
sudo lsi-flash flash HBA --as lsi-9211-8i --yes
```

Cross-flashes the chip to the target identity (LSI 9211-8i, in this example). The orchestrator runs the full state machine: detect → safety guards → backup → halt → erase → write firmware → write BIOS → write SBR → reset → verify. Refuses to start if mounted filesystems / LVM PVs / ZFS pools sit on attached disks.

### Recover from a botched flash

```bash
sudo lsi-flash recover --pci 0000:03:00.0 --from /var/lib/lsi-flash/backups/2026-05-27
```

If the card still enumerates on PCIe (so `lspci` sees it), recovery is automated via HCB hostboot + reflash. **If the card no longer enumerates** (PCIe-level brick, our dev-1 failure mode), `lsi-flash` cannot help — you need a CH341A SPI clip; community procedure linked in [SECURITY.md](SECURITY.md).

## Safety

Cross-flashing chip firmware **can brick the card** if it goes wrong. Our own dev-1 card was bricked by chaining two `lsiutil` `FW_DOWNLOAD` operations in the same session — the second hit `IOCStatus 0x0004 (Internal Error)`, left a partial write, and the silicon bootloader could no longer initialize PCIe at the next reboot.

This project's design centers on **not repeating that**:

- **Type-level personality enforcement** — you cannot accidentally write IR firmware while IT firmware is running (compile error)
- **Verify-after-write** on every `FW_DOWNLOAD` — reads back the bytes via `FW_UPLOAD` and byte-compares
- **No chained flash writes in one session** — firmware and BIOS go in separate MPI sessions (the dev-1 brick mechanism)
- **Pre-flight safety guards** — `findmnt`/`pvs`/`mdstat`/`zpool` checks; refuses to flash if any downstream disk is in use
- **Pre-flight flash size check** — refuses oversized firmware (e.g., standard `2118it.bin` won't fit Tape Adapter's smaller flash)
- **Always-backup** before destructive ops; backup dir defaults to `/var/lib/lsi-flash/backups/` (not `/tmp`)

There is **no replacement** for reading what the tool is about to do via `--dry-run` and confirming it matches your intent.

## Architecture

```
src/
├── pci.rs                # PCIe sysfs walk + BAR1 mapping (Platform trait abstraction)
├── sbr/                  # SBR (256-byte EEPROM) parse, build, I²C bit-bang
├── mpi/                  # MPI 2.0 message layer
│   ├── doorbell.rs       # WRSEQ unlock, doorbell register access
│   ├── diag.rs           # IOC halt/reset
│   ├── messages.rs       # IOC_INIT/CONFIG/FW_DOWNLOAD/FW_UPLOAD/TOOLBOX_CLEAN serializers
│   ├── session.rs        # High-level orchestration + PersonalityMatched token
│   ├── mock_ioc.rs       # In-memory IOC simulator (--dry-run backend + tests)
│   ├── real_ioc.rs       # Production backend (BAR1 mmap + doorbell + reply queue)
│   └── mmap_region.rs    # Persistent read-write mmap for BAR1
├── hcb.rs                # Host-Controlled Download Window (firmware-in-RAM boot)
├── firmware/
│   ├── inspect.rs        # MPI2_FW_IMAGE_HEADER parse + checksum verify
│   └── synthesize.rs     # Manipulator features (reverse-phy, future identity-overlay)
├── card_database.rs      # TOML loader (cards + manifest cross-reference)
└── cli/
    ├── detect.rs         # `lsi-flash detect`
    ├── backup.rs         # `lsi-flash backup`
    ├── recover.rs        # `lsi-flash recover`
    ├── flash.rs          # `lsi-flash flash` (12-phase orchestrator)
    ├── sbr.rs            # `lsi-flash sbr {show,verify,build,read,write,modify}`
    └── safety.rs         # findmnt/pvs/mdstat/zpool guards
```

## Testing strategy

Four layers:

1. **Unit tests** — pure functions (parsers, checksums, byte layouts)
2. **Platform-trait mocks** — `MockPlatform` lets the PCI sysfs walk be tested without `/sys`
3. **Mock IOC** — `MockIoc` simulates the SAS2008 MPI firmware in-memory; covers the entire flash orchestrator dry-run path + failure-injection (`IOCStatus::Busy`, `IOCStatus::InternalError`, etc.)
4. **Real hardware** — `RealIoc` against actual SAS2008 cards on a test bench (currently a Dell H200 Internal Tape Adapter, hardware-verified 2026-05-27)

`cargo test` runs layers 1-3. Layer 4 is documented manual validation against a known-good baseline.

## Related repositories

- **[`lsi-flash-firmware`](https://github.com/MattJackson/lsi-flash-firmware)** — the public firmware mirror. ~300 sha256-verified blobs covering every public SAS2008 phase + every Dell PERC H200/H310 revision found in the wild. The `lsi-flash` CLI reads its `manifest.toml` at flash time.

## Acknowledgements

This project stands on years of community work, especially:

- **[marcan/lsirec](https://github.com/marcan/lsirec)** — the HCB hostboot technique that makes path-c possible (and the C reference for everything the chip-diagnostic layer does)
- **[confusingboat/lsirec fork](https://github.com/confusingboat/lsirec)** — additional NVData / SBR analysis
- **[confusingboat/flash-it](https://github.com/confusingboat/flash-it)** — the bash glue that proved the workflow end-to-end
- **[FOHDeesha PERC guides](https://fohdeesha.com/docs/perc.html)** — community canonical for the Dell-specific procedures
- **[lrq3000 LSI SAS HBA cross-flash guide](https://github.com/lrq3000/lsi_sas_hba_crossflash_guide)** — DOS/EFI tool archive + procedure documentation
- **Broadcom** — for the original MPI 2.0 spec (`mpi2.h`, `mpi2_ioc.h`, `mpi2_tool.h`, `mpi2_cnfg.h`) that documents the wire format these tools manipulate

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Hardware-compatibility reports (good or bad — *especially* bad) are the single highest-value contribution.

## Security

If you find a bug that could brick a card, please report it privately first — see [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE). Firmware files in the companion `lsi-flash-firmware` repo retain their original Broadcom proprietary license; that repo's `LICENSE.md` explains the fair-use preservation rationale.

## Roadmap

See [ROADMAP.md](ROADMAP.md).
