# Security policy

## Reporting a vulnerability

Use **GitHub Private Vulnerability Reporting**:

  https://github.com/sotashimozono/gr3sync/security/advisories/new

Public issues, discussions and pull requests are not the right channel.

Expect a first response within 7 calendar days and a substantive one within 30.
Coordinated disclosure is preferred; reporters who want credit will be
acknowledged in the release notes.

## What this program actually touches

gr3sync is a small tool with an unusually physical blast radius, so it is worth
being specific about where the risk is rather than reciting boilerplate.

**It writes to a camera.** The only characteristics it is ever allowed to write
are Camera Power, Operation Mode and Network Type; a test pins that set. Those
UUIDs come from reverse-engineering, not from RICOH, so a wrong constant would
mean writing an unintended value to an unintended characteristic on real
hardware. Changes to `src/protocol.rs` deserve proportionate scrutiny: a wrong
byte there is neither a compile error nor a test failure.

**It handles the camera's Wi-Fi passphrase.** Read over BLE, passed to the
host's Wi-Fi tool as a separate `argv` element — never interpolated into a
shell string — and never written to disk or to the event stream. `gr3sync wlan
on` prints it deliberately, because that is what the subcommand is for.

**It rewrites the host's network state.** It takes the Wi-Fi interface off the
current network and puts it back. An interrupted run that failed to restore
would leave the machine on an access point with no route anywhere; the restore
therefore runs unconditionally, including on failure.

**It reads the SD card and never writes to it.** No delete, no format, no
rename — over HTTP the client is read-only with respect to storage.

**`gr3sync raw write` is a loaded gun by design.** It pokes an arbitrary
characteristic on an undocumented device. It exists because first contact with
real hardware needs it. Its help says so.

## In scope

The `gr3sync` crate and binary in this repository, including the emulator
behind the `emulator` feature.

## Out of scope

- RICOH's firmware, and anything the camera does in response to a documented
  operation. gr3sync cannot fix the device; report those to RICOH.
- The reverse-engineered specification itself
  ([dm-zharov/ricoh-gr-bluetooth-api](https://github.com/dm-zharov/ricoh-gr-bluetooth-api)).
  A mistake there is a bug here only insofar as gr3sync copied it — which is
  worth reporting, but as a correctness issue.
- Damage from `raw write` used deliberately.

## Supply chain

Third-party GitHub Actions are pinned by commit SHA. `cargo deny` and
`cargo audit` run on every dependency change and weekly; a failed scheduled run
opens a tracking issue, because a cron failure on a quiet repository otherwise
notifies nobody.
