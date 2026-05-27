# Changelog

All notable changes to `lsi-flash` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Until v1.0, breaking changes may happen on any 0.x release (per ADR-008).

## [Unreleased]

### Added
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
- Test count: 22 → 173 (Stage 1 → mid-Stage 3)

### Fixed
- Flash orchestrator `step_backup` was unconditionally returning
  `Phase::Hostboot`, causing infinite Halt→Backup→Hostboot→Halt loop when
  personalities already matched. Now correctly transitions to `Phase::Erase`.

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
