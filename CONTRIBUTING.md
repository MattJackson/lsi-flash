# Contributing to lsi-flash

Thanks for thinking about contributing. This project is small enough that any
contribution is genuinely useful — bug reports, hardware-compatibility
confirmations, doc fixes, code.

## Highest-leverage contributions

In rough order:

1. **Hardware-compatibility reports.** If you run `lsi-flash detect` on any
   SAS2008 card, file a [hardware compatibility issue](https://github.com/MattJackson/lsi-flash/issues/new?template=hardware_compatibility.yml)
   with the full output. We use these to fill in the support matrix. Negative
   reports (it failed, here's the error) are *more* valuable than positive ones.
2. **Bug reports with reproducible failures.** Especially anything that
   bricked a card — those are P0.
3. **Code.** See below.
4. **Documentation.** The CLI's `--help` output, the README, and the
   architecture docs all benefit from a fresh pair of eyes.

## Building

```bash
git clone https://github.com/MattJackson/lsi-flash
cd lsi-flash
cargo build              # debug build
cargo test               # 173+ tests (no hardware required — uses MockIoc)
cargo build --release    # release build
```

For the production musl static binary:

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

MSRV: Rust 1.74. CI tests against stable + 1.74 to catch MSRV regressions.

## Code style

- `cargo fmt` before committing — CI enforces this
- `cargo clippy --all-targets -- -D warnings` — CI enforces this too
- Comment **why**, not **what**. The code should make "what" obvious; "why"
  is what future-you and reviewers actually need.
- When citing an MPI 2.0 struct field offset or a flash region constant,
  include a `file:line` comment to the canonical source (`mpi2_ioc.h:N`,
  `lsirec.c:N`, etc.). The format-reference docs in the notes repo
  cross-check these.

## Testing philosophy

Four layers (per ADR-010):

1. **Unit tests** — pure functions
2. **Platform mocks** — `MockPlatform` for PCI sysfs
3. **MockIoc** — in-memory SAS2008 simulator (covers everything that doesn't
   need real hardware, including the entire `flash --dry-run` orchestrator)
4. **Real hardware** — `RealIoc` against actual cards on a test bench

A PR should keep `cargo test` green at all four layers it touches.
Hardware-bound tests can include a `#[ignore]` attribute with a comment
explaining the hardware prerequisite.

## Pull request expectations

- One logical change per PR. Refactors and feature work in separate PRs.
- Commit messages: imperative mood, what + why. Reference issues if applicable.
- **No `Co-Authored-By: Claude` (or any AI/agent attribution).** This is a
  hard rule. Commits go out under the author's name only.
- If the PR touches the flash orchestrator or any destructive code path,
  include a test that exercises the failure mode you're guarding against.
- If the PR adds support for a new card, include the card's `lsi-flash detect`
  output (sanitized SAS WWN if you prefer) and a manifest entry in
  `lsi-flash-firmware`.

## What absolutely requires senior review

Anything that writes to flash. The 2026-05-20 dev-1 brick taught us that
chained `FW_DOWNLOAD` operations + missing verify-after-write can leave the
silicon bootloader in an unrecoverable state. Any PR that adds a code path
which could end up calling `MPI_FUNCTION_FW_DOWNLOAD` against real hardware
needs explicit review focused on the brick scenario.

## Releasing

(Maintainer note, for future reference.)

1. Update `CHANGELOG.md`
2. Bump version in `Cargo.toml`
3. `git tag -a v0.1.0 -m "..."` and push
4. CI builds the musl release binary and attaches it to the GitHub release

## Questions

Open a [Discussion](https://github.com/MattJackson/lsi-flash/discussions)
(when enabled) or an issue with the `question` label. There's no Discord/IRC
yet — too small a project.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By
participating you agree to abide by its terms.
