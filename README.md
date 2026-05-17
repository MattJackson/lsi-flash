# lsi-flash

Modern Linux CLI for cross-flashing LSI SAS2008-based HBAs (Dell PERC H200/H310, IBM M1015/M1115, LSI 9211-8i/9210-8i, Fujitsu D2607, HP H220, Supermicro AOC variants) between IT and IR firmware modes.

**Status:** planning / scoping. No code yet. See [`ROADMAP.md`](ROADMAP.md).

## Why

The existing tooling for cross-flashing these cards is a pile of bash scripts (`flash-it.sh`), 16-bit DOS binaries (`megarec.exe`), separate compiled tools (`lsirec`, `lsiutil`, `sas2flash`), and Python dependencies (`sbrtool.py`). The procedure typically requires booting FreeDOS off USB, multiple reboots, and intimate knowledge of which tool refuses what for which reason.

These cards (originally ~2010 vintage) are EOL but still actively bought used by the homelab / TrueNAS / Proxmox / ZFS community — because in IT mode they're cheap, reliable HBAs. There is no modern, polished, single-binary tool. This repo is the attempt at one.

## Scope

**v1.0:** SAS2008-based HBAs only — the chip family in PERC H200/H310, M1015/M1115, LSI 9211-8i.

**Out of scope:** MegaRAID-class hardware RAID cards (PERC H700/H710/H730 family). Different chip generation, different firmware paradigm, different audience (hardware-RAID users typically *want* IR mode). Possible v2.0 if community demand justifies the additional surface area.

## Design goals

- Single statically-linked binary (musl). Drops onto any Linux live USB and runs.
- Auto-detect everything (PCI address, current firmware mode, card OEM).
- Crash-safe, resumable (state machine + persistent on-disk session in `/var/lib/lsi-flash/`).
- Refuse to operate when unsafe (mounted FS, active LVM, ZFS pool on attached disks).
- Honest error messages with actionable next steps.
- Pre-flight checks for the common environment issues (hugepages, IOMMU, kernel driver state).

## Language

Rust. Specifically because:
- `mmap` + `volatile` for PCIe MMIO ports cleanly from the C reference code in `lsirec`.
- `cargo build --target x86_64-unknown-linux-musl` solves the "works on every distro" problem.
- `clap`, `tracing`, `indicatif`, `miette` for serious-sysadmin-tool UX without writing it from scratch.
- Cross-compile and embedded-data-file story are both pleasant.

## Status & next step

Scoping doc exists privately. First real engineering task is **Stage 0**: validate on real hardware that the "host-boot LSI IR firmware then erase NVRAM via standard MPI" sequence works on a Dell H200. If yes, the whole project is dramatically smaller than the worst case (which involves reverse-engineering the megarec ARM stub). See [`ROADMAP.md`](ROADMAP.md) for the full staged plan.

## License

MIT. See [`LICENSE`](LICENSE).
