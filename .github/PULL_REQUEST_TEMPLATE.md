<!--
Delete a section only if it genuinely does not apply.
-->

## Summary

<!-- 1-3 bullets on WHY this exists. The diff already shows what. -->

-

## Changes

<!-- The shape of the change, grouped by area. Not the diff. -->

## Verification

- [ ] `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-features`
- [ ] CI green on this branch
- [ ] Any new third-party Action is pinned by commit SHA

**What did you actually run this against?** Tick every one that applies, and
say so plainly if none:

- [ ] the in-process emulator (`cargo test`)
- [ ] the `gr3-emulator` binary as a subprocess (`tests/e2e.rs`)
- [ ] the container on `192.168.0.1`
- [ ] a Bumble peripheral over `/dev/vhci`
- [ ] **a real RICOH GR III**

<!--
This matters more here than in most projects. The emulator is built from the
same reverse-engineered specification as gr3sync, so a green test against it
agrees with our assumptions whether or not the camera does. If you changed
protocol.rs, ble.rs or a timeout, only the last box is evidence.
-->

## Does this change what the camera sees?

<!--
Answer explicitly if this PR touches src/protocol.rs, src/ble.rs, or any
timeout or settle duration. "No" is a fine answer; silence is not.
-->

## Verification status

- [ ] If this PR turns an unverified claim into a verified one, the README's
      "Verification status" section is updated to match.

<!--
If this PR was authored with AI assistance, add a trailer to your commit(s):

    Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
-->
