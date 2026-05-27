<!--
Thanks for the PR. A few things to double-check before requesting review:

- Commits do NOT include `Co-Authored-By: Claude` (or any AI/agent attribution).
  That trailer is a hard no — see CONTRIBUTING.md.
- `cargo fmt --all` is clean.
- `cargo clippy --all-targets -- -D warnings` is clean.
- `cargo test` is green.
- If this touches a destructive code path (anything that could end up calling
  `MPI_FUNCTION_FW_DOWNLOAD` against real hardware), it needs senior review with
  the brick scenario in mind.
-->

## Summary

<!-- One or two sentences: what does this change, and why? -->

## Type of change

- [ ] Bug fix
- [ ] New feature
- [ ] Refactor (no behavior change)
- [ ] Docs only
- [ ] CI / build / tooling
- [ ] Hardware-compatibility report (added card to support matrix)

## Test plan

<!-- How did you verify this works? Tick what applies. -->

- [ ] `cargo test` (MockIoc — no hardware needed)
- [ ] `cargo test --features hardware-tests` against a real card
- [ ] Tested against real card (model + firmware before/after below)
- [ ] N/A (docs / CI only)

## Hardware tested against (if applicable)

<!--
Card: e.g. Dell PERC H200 Adapter (1000:0072 1028:1f1c)
Firmware before: e.g. Dell ITA A04 IT 07.15.08.00
Firmware after:  e.g. sas2008-9211-p20-it 20.00.07.00
Outcome: worked / failed / bricked
-->

## Brick-risk callout

<!--
If this PR adds or modifies any code path that writes to flash, describe:
- which orchestrator phases are affected
- what guards prevent partial writes from leaving an unrecoverable card
- whether MockIoc coverage exercises the failure modes
-->

## Related issues

<!-- Fixes #N / refs #N -->
