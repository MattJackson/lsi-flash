# Roadmap

Five stages, each independently shippable. Realistic total for v1.0 (stages 0-3): ~6 weeks of focused engineering. Pessimistic ~10 weeks if there are nasty surprises in the MPI message layer.

## Stage 0 — Validate path-c on real hardware

**Lab work, no code. ~1 day.**

Take a stock Dell H200 still on Dell firmware. Using the existing `lsirec` + `lsiutil` tools, attempt:

```
lsirec ... hostboot 2118ir.bin   # load LSI IR firmware into IOC RAM
lsiutil -p1 -a 33,3,8,,0         # erase flash via MPI (LSI fw running, no Dell tamper)
lsiutil -p1 -a 2,yes,0 -f 2118it.bin   # flash IT firmware to NVRAM
```

The lsirec README has this procedure marked "UNTESTED" for the Dell case. If it works → the project is straightforward. If it doesn't → escalate to extracting the LSI recovery firmware blob from `megarec.exe` and re-hosting it via lsirec's HCB mechanism (much more involved).

This stage is dispositive. Do it first.

## Stage 1 — Rust port of lsirec + sbrtool, polished

**~1-2 weeks.**

Strict superset of `lsirec` + `sbrtool.py` functionality, but as one binary with proper UX:

- PCIe device discovery + BAR1 mapping
- MPT chip register access (DIAG, DOORBELL, WRSEQ, HCB)
- SBR I2C bit-bang (read, write, parse, build, patch in-place)
- HCB firmware upload with hugepage/IOMMU pre-flight
- Card identification database (known VID/SSID combos → friendly names)
- CLI: `detect`, `info`, `sbr {read,write,parse,patch,template}`, `hostboot`, `halt`, `reset`, `unbind`, `rescan`
- Embedded sample SBRs for known cards
- Structured logging, `--json` mode

Replaces `lsirec` and `sbrtool.py` outright. Doesn't yet do crossflashing — but already useful as a daily tool.

## Stage 2 — MPI message layer + flash module

**~1-2 weeks.**

Implements enough MPT/MPI 2.0 to do:

- `MPI_FUNCTION_FW_DOWNLOAD` (firmware, BIOS, NVDATA)
- `MPI_FUNCTION_FW_UPLOAD` (backup current flash)
- `MPI_FUNCTION_TOOLBOX_CLEAN` (erase)
- `MPI_FUNCTION_CONFIG` (read manufacturing pages for WWID)

Removes the `lsiutil` dependency from the workflow. CLI gains `flash {it,ir,bios,nvdata}` and `flash {erase,backup}` subcommands.

Works against a card already running compatible firmware. Doesn't yet do single-shot cross-flash.

## Stage 3 — Crossflash orchestrator

**~1-2 weeks.**

The big-button workflow:

```
lsi-flash crossflash --target it
```

Typed state machine, resumable, persistent backups in `/var/lib/lsi-flash/<sas-addr>/`, safety guards (mounted FS detection via `findmnt`/`dmsetup`/`mdadm`/`zpool`), `--dry-run`, `--keep-sas-address`.

Embeds or fetches `2118it.bin` / `2118ir.bin` / `mptsas2.rom` with SHA verification.

At end of stage 3: Dell H200 → LSI IT crossflash is one command, no reboots required (assuming Stage 0 path-c works). **This is the v1.0 release.**

## Stage 4 — Coverage expansion

**Ongoing.**

- More OEM card templates (HP H220, Fujitsu D2607, Supermicro AOC variants)
- Better diagnostic dumps for un-flashable cards ("send us this output")
- `--report` JSON for community card data collection
- Packaging: AUR, deb, TrueNAS plugin

## Stage 5 (stretch, conditional) — megarec equivalent

**Only triggered if Stage 0 path-c fails.**

Disassemble `megarec.exe`, extract the embedded LSI recovery firmware blob, re-host via lsirec HCB mechanism. ~2-6 weeks of work, depending on whether the blob has runtime fixups or checksums.

Legal review on redistribution of the blob.

## Effort summary

| Stage | Optimistic | Realistic | Pessimistic |
|---|---|---|---|
| 0 — path-c validation | 6 h | 16 h | 40 h |
| 1 — Rust port | 30 h | 60 h | 100 h |
| 2 — MPI + flash | 40 h | 80 h | 140 h |
| 3 — Orchestrator | 40 h | 80 h | 120 h |
| 4 — Coverage | 30 h | 60 h | continuous |
| 5 — megarec (conditional) | 80 h | 200 h | 500 h+ |
| **v1.0 (0-3)** | **116 h** | **236 h** | **400 h** |
