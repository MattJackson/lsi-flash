# Security Policy

`lsi-flash` writes to hardware flash. A bug in this code can brick a SAS2008
card such that it can only be recovered with a CH341A SPI clip and a soldering
station — or not at all. Security reports get top priority.

## Supported versions

| Version | Supported |
|---|---|
| `main` branch | ✅ |
| any tagged release | ✅ (latest) |
| `0.0.x` pre-release | ⚠ best-effort (still pre-release; expect rebases) |

## Reporting a vulnerability

**Please report privately first** if the vulnerability could:

- Brick a card (especially without user confirmation)
- Cause data loss on attached disks
- Allow privilege escalation (`lsi-flash` runs as root)
- Cause silent firmware corruption (write succeeds but bytes wrong)

Send a private report via [GitHub Security Advisories](https://github.com/MattJackson/lsi-flash/security/advisories/new).
If GitHub Security Advisories isn't available to you, email `matthew@pq.io`
with the subject `[lsi-flash security]`.

Please include:

- Affected version (commit SHA preferred)
- Reproduction steps (ideally against `MockIoc` so I can reproduce without
  real hardware)
- Impact assessment (what could a malicious / careless user cause?)
- Any suggested mitigation

I'll acknowledge within 72 hours and aim to ship a fix within 14 days for
brick-risk bugs, longer for less critical issues.

## What is NOT a security vulnerability

- The tool requires root. That's intentional — you're writing to PCIe BAR1
  registers. Don't run untrusted firmware as root, same as anything else.
- The tool can brick a card if you use `--yes` to skip confirmation prompts
  and then provide the wrong firmware. That's a documented foot-gun, not a
  bug. The defaults (`--dry-run` available, confirmation required) exist
  specifically to prevent this; opting out is your call.
- The companion `lsi-flash-firmware` repo redistributes Broadcom-proprietary
  firmware files under fair use. If you believe a specific file in that repo
  should be removed for license reasons, file an issue there (not here).

## Hardware-rescue resource

If you have bricked a card and want to attempt hardware rescue, the CH341A
SPI clip procedure is documented externally (FOHDeesha, lrq3000). The
`lsi-flash` CLI deliberately does not implement SPI-clip operations — that's
out-of-band hardware work, not a software flow.

## Hall of fame

Reporters who identify and responsibly disclose vulnerabilities will be
credited in `CHANGELOG.md` for the fix release (unless they prefer to remain
anonymous).
