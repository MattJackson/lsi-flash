# Changelog

All notable changes to `lsi-flash` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Until v1.0, breaking changes may happen on any 0.x release (per ADR-008).

## [Unreleased]

### Changed
- ADR-018 Phase 1: library extraction — split monolithic binary into `liblsi` crate; created `src/lib.rs` with public modules + curated re-exports, moved CLI entry to `src/bin/lsi-flash.rs`, renamed package from `lsi-flash` to `liblsi` while preserving `lsi-flash` binary name; `cargo build --release` still produces `target/release/lsi-flash` (mechanical refactor, no behavior change)
- MptCard `Card` impl — concrete implementation for Fusion-MPT chips (SAS2008/SAS2208/SAS3008)
  - `src/card/mpt.rs`: `MptCard` struct wrapping `MptTransport`; implements `Card::identity()`, `detect()`, `backup()`, `current_personality()` stub, plus `sbr_read()` via pluggable `SbrTransport` trait (VFIO+I²C bit-bang today; ISTWI stubbed for future)
  - Cites ADR-017 (`lsi-flash-notes/01-architecture/adr/017-card-trait-and-pluggable-transport.md`)
  - `discover_one(bdf)` now routes through chip-family dispatch in `src/card/mod.rs` — reads VID:DID from sysfs, maps to `ChipFamily` via `chip_family_from_pci()`, then constructs `MptCard` for known families or returns `UnsupportedCard(vid, did)` cleanly
  - `backup()` implements FW_UPLOAD for [Fw, Bios, FlashLayout] image types using MPI 2.0 wire format (cites `src/cli/backup.rs:185-209` for byte layout)
  - Writes firmware.bin, bios.rom, nvdata.bin + manifest.toml with SHA256 hashes per ADR-015 Rule 10
  - Tests: `identity()` returns correct values; `current_personality()` returns NotImplemented; `detect()` via mock transport; `backup_writes_artifacts` verifies file output; new tests for `sbr_read()` via MockTransport, `chip_family_from_pci()` table lookup
  - Cites ADR-017 (`lsi-flash-notes/01-architecture/adr/017-card-trait-and-pluggable-transport.md`)
- `cli/backup.rs` — refactor to use Card trait instead of inline Mpt3CtlTransport path
  - Removed `run_backup_via_mpt3ctl()` function (lines ~144-278) — functionality now in `MptCard::backup()`
  - `--pci <bdf>` branch calls `crate::card::discover_one(&bdf)?` then `card.backup(&out_dir)`
  - Kept VFIO+legacy doorbell fallback chain for operators when mpt3sas isn't loaded
  - Added `print_backup_report()` helper for unified output formatting (human-readable + JSON modes)

### Changed
- `src/sbr/transport.rs` — new `SbrTransport` trait with two implementations per ADR-017 pluggability principle:
  - `VfioI2cSbrTransport`: VFIO+BAR1 I²C bit-bang path (works on dev-1); temporarily evicts mpt3sas during operation, restores on Drop
  - `IstwiSbrTransport`: MPI TOOLBOX_ISTWI stub; currently returns `NotImplemented` due to IOCStatus 0x8004 INTERNAL_ERROR on SAS2008 with DevIndex=0, Action=READ_DATA(0x01) — wire-format research preserved from commit 8fd35ff for future enablement
- `src/card/mpt.rs::MptCard::sbr_read` — reduced to ~5 lines calling `VfioI2cSbrTransport`; when ISTWI is solved, one-line change swaps to ISTWI first with VFIO fallback
  - Wire format per mpi2_tool.h:171-200 (`MPI2_TOOLBOX_ISTWI_READ_WRITE_REQUEST`): Tool=0x03, Function=0x17, Action=0x01 (READ_DATA), TxDataLength=0, RxDataLength=256
  - Reply parsing per mpi2_tool.h:214-228: checks IOCStatus at offset 0x0E for SUCCESS
  - Output handling (raw bytes / JSON / SHA256 to stderr) unchanged from original implementation

### Added
- MPI2_FLASH_LAYOUT struct + parser (ADR-015 Rule 11a)
  - `src/mpi/messages.rs`: `FlashRegion`, `FlashRegionType` enum, `FlashLayoutReply` with parse() and region() methods
  - Wire format per mpi2_ioc.h:1469-1502 (MPI2_FLASH_REGION at :1469, MPI2_FLASH_LAYOUT at :1480)
  - Tests: from_u8/as_u8 roundtrip for all region types, golden buffer parse with 2 regions, region() lookup by type, short-buffer error handling
- Card trait scaffold per ADR-017 — pluggable abstraction over flash-capable cards
  - `src/card/mod.rs`: `Card` trait with detect/backup/current_personality methods
  - `CardIdentity` struct for PCI-based identification (BDF, VID:DID, chip family)
  - `ChipFamily` enum: Sas2008/Sas2208/Sas3008/Unknown — maps to card-database.toml entries
  - `CardError` error type following project's thiserror pattern
  - `DetectReport` + `BackupReport` stubs mirroring existing CLI shapes (TODO: senior flesh out)
  - `discover()` factory walks `/sys/bus/pci/devices/` via `crate::pci::Platform`, dispatches by VID:DID
  - `discover_one(bdf)` for specific BDF lookup — both return NotImplemented today (MptCard impl in follow-up)
  - Re-exports `Personality` from mpi::session for convenient access: `use crate::card::{Card, Personality}`
  - Cites ADR-017 (`lsi-flash-notes/01-architecture/adr/017-card-trait-and-pluggable-transport.md`)
  - ChipFamily mapping cites card_database.rs entries (SAS2008: LSI 9211-8i, Dell H200/H310, IBM M1015; SAS2208/SAS3008 future targets)
- `lsi-flash sbr read` hardware-bound verb for reading SBR from chip EEPROM via I2C
  - Cites `src/sbr/i2c.rs::i2c_read_sbr` signature and wire protocol (lsirec.c:570-630)
  - Accepts `--pci <bdf>` to specify target card, defaults to first SAS2008 if omitted
  - Accepts `--output <path>` for file output, defaults to stdout as raw bytes
  - Accepts `--json` flag for JSON serialization of SBR struct fields
  - Computes SHA256 hash of SBR bytes and prints to stderr in all modes
- `src/sbr/parse.rs` serde Serialize/Deserialize derives on MfgFields and Sbr structs
- src/cli/sbr.rs test module with canned SBR byte tests for JSON serialization round-trip
- `src/card/mod.rs`: extend `Card` trait with `sbr_read()` method (NotImplemented default)
  - Added `chip_family_from_pci(vid, did)` helper function with VID:DID → ChipFamily table lookup
  - Rewrote `discover_one(bdf)` to read sysfs vendor/device IDs, dispatch by chip family
  - Returns `UnsupportedCard(vid, did)` cleanly for unknown devices instead of misleading "no IOC" errors
- `src/card/mpt.rs`: implement `MptCard::sbr_read()` using TOOLBOX_ISTWI via Mpt3CtlTransport
  - Wire format per mpi2_tool.h:171-200; reply parsing per mpi2_tool.h:214-228
  - DevIndex = 0x00 (SBR EEPROM on SAS2008); Action = READ_DATA (0x01); RxDataLength = 256
  - Tests: `test_mptcard_sbr_read_via_mock_transport()` verifies wire format + canned payload
- `src/card/mpt.rs`: unit tests for chip_family_from_pci table lookup (known + Unknown cases)
- `src/mpi/real_ioc.rs` `RealIoc` backend scaffolding (cycle 1) — `IocBackend`
  trait impl with `todo!()` bodies; tests against `MockPlatform`
- `src/mpi/mmap_region.rs` — persistent read-write BAR1 mmap (cycle 2a). Holds
  the mapping for its lifetime; `munmap()` on Drop
- `RealIoc::open()` now actually mmaps `/sys/bus/pci/devices/<bdf>/resource1`
- `MpiError::NotImplementedYet { op }` and `MpiError::Io(String)` variants
- `cli/flash.rs` 12-phase orchestrator state machine (Step 5 part 1)
- `cli/safety.rs` mount / LVM / mdraid / ZFS safety guards (Step 5 part 2)
- Real MPI ops wired into orchestrator destructive states (Step 5 part 3)
- `cli/backup.rs` — `lsi-flash backup` verb
- `cli/recover.rs` — `lsi-flash recover` verb
- `firmware/synthesize.rs` — first manipulator feature (`firmware reverse-phy`)
- `mpi/messages.rs` — typed Request/Reply for IOC_INIT/CONFIG/FW_{DOWNLOAD,UPLOAD}/TOOLBOX_CLEAN
- `mpi/session.rs` — `Session` orchestrator + `PersonalityMatched` token (ADR-015 Rule 1)
- `mpi/mock_ioc.rs` — in-memory SAS2008 IOC simulator
- `cli/sbr.rs` `build` real implementation (replaces ADR-014 stub)
- `cli/detect.rs` — `lsi-flash detect` verb
- `pci.rs` `Platform` trait refactor (Task #22 per ADR-004)
- Embedded card-database via `include_str!` (ADR-014)
- `RealIoc::send_ioc_init` + `send_fw_upload` doorbell-handshake impls (cycle
  2b-1). Both serialize the MPI request to wire format and post via the
  DOORBELL register at BAR1+0x00, then parse the reply. `send_fw_upload`
  also pre-validates `image_size <= payload_buffer.len()` before any BAR1
  access so it remains testable on non-Linux targets.
- GitHub Actions CI: rustfmt + clippy + test (stable) + musl static binary,
  plus issue/PR templates and dependabot
- Test count: 22 → 174 (Stage 1 → mid-Stage 3)
- `src/mpi/messages.rs` — `IocFactsRequest` + `IocFactsReply` types with function code
  `0x03` per mpi2_ioc.h:191; reply includes firmware version, NVDATA vendor/product ID,
  board name (16B), board tracer (16B) — cites mpi2_ioc.h:231-281 for exact field layout
- `src/mpi/session.rs` — `send_ioc_facts()` method on `IocBackend` trait with
  `raw_ioc_facts()` helper in `Session`
- `src/mpi/mock_ioc.rs` — `send_ioc_facts()` returns canned Tape Adapter data
  (vendor=0x1000, product=`LSI2008`, fw_version=7.15.8.0, board=`Dell H200`) for
  tests without real hardware
- `src/mpi/real_ioc.rs` — `send_ioc_facts()` follows same doorbell-handshake pattern as
  `send_ioc_init`; writes function code + size to DOORBELL register, sends request payload,
  reads 96-byte reply (TODO: add IOC_DOORBELL_INT wait for real hardware bring-up)
- `src/cli/detect.rs` — extended output with NVDATA Vendor/Product ID, Firmware Product ID,
  NVDATA Version (distinct from FW version per baseline.md:15), Board Name, and Board Tracer;
  graceful skip on MPI failure (no panic when card not initialized or hardware absent)
- `src/cli/detect.rs` — JSON output (`--json`) includes all extended fields as optional keys
- Manufacturing Page 0 fetch via CONFIG read (action=0x06 NVRAM copy, page type=0x09 Mfg, page=0);
  parses NVDATA vendor ID at offset 0x08, product ID string at 0x0A, NVDATA version at 0x18,
  firmware product ID at 0x28 per toolbox-and-config.md §5 and baseline.md:14-15
- `src/mpt/mpt3ctl.rs` — `Mpt3CtlTransport` kernel-mediated MPI transport impl (freshman cycle). Wraps
  `/dev/mpt3ctl` character device + `MPT3COMMAND` ioctl from Linux's `mpt3sas` driver. Implements the
  `MptTransport` trait for read-safe operations where the card stays bound to mpt3sas. Uses kernel-allocated
  bounce pages for DMA, avoiding ~2000 LoC of user-space post-queue plumbing (Path B from ADR-017). Hardcodes
  SGE offset to word 5 (byte 0x14) for FW_UPLOAD_REQUEST per v1 scope. Includes ioctl ABI verification tests
  and musl portability support via local `IoctlReq` type alias matching patterns in `src/hw/vfio.rs`.

### Fixed
- Flash orchestrator `step_backup` was unconditionally returning
  `Phase::Hostboot`, causing infinite Halt→Backup→Hostboot→Halt loop when
  personalities already matched. Now correctly transitions to `Phase::Erase`.

### Known issues
- `RealIoc::send_ioc_init` / `send_fw_upload` do not currently wait for the
  `IOC_DOORBELL_INT` bit in HISTATUS between dword writes / reads. On real
  silicon this can race the IOC; the write/read loops assume the host can
  drive the doorbell at memory speed. Will be addressed during dev-1
  hardware bring-up — both ops are still read-only / safe from a brick
  standpoint regardless.
- ~30 pre-existing clippy warnings (dead code reserved for future stages,
  capitalized acronyms `HBA` / `IMR` / `RAID` that are legitimate domain
  terms). CI keeps clippy advisory until a dedicated sweep lands.

### Safety
- Destructive `RealIoc` ops (`send_fw_download`, `send_toolbox_clean`) return
  `MpiError::NotImplementedYet` and stay gated until CH341A SPI clip + cold-spare
  card on hand. Brick-risk hardware paths remain senior-review-only.

## Notes on pre-history

Before the unreleased section above, the project had a long "no code yet"
period during which the firmware archive (~300 blobs covering every public
SAS2008 phase) was collected, the MPI 2.0 wire format was reverse-engineered
from `mpi2_ioc.h` / `mpi2_cnfg.h` / `mpi2_tool.h`, and the four chip-side
cryptographic walls (anti-rollback, encryption, OTP, chip-side secure boot)
were characterized as ABSENT — clearing the architectural path for the Rust
port. None of that produced binary releases; the artifacts live in the
companion `lsi-flash-firmware` repo and (privately) `lsi-flash-notes`.

The first tagged release will be v0.1.0 once Stage 2 closes (`sbr` hardware-bound
verbs land + `detect` extended fields surface what `sas2flash -list` does).
