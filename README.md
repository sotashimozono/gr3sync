# gr3sync

Pull photos off a RICOH GR III without touching the camera.

```
$ gr3sync pull
gr3sync -> /home/you/Pictures/GR3
  scanning for the camera over Bluetooth...
  found GR_4CF5C6 at AA:BB:CC:DD:EE:FF
  camera was off -> woke it
  battery 88% (battery)
  camera Wi-Fi up: GR_4CF5C6
  joining GR_4CF5C6 (was on Home Fibre)
  RICOH GR III, battery 88%
  214 files on the card
  12 to download, 202 already here
  [1/12] 101RICOH/R0001203.DNG
  ...
  back to Home Fibre
  downloaded 12 files (287.4 MiB), skipped 202
```

The camera stays in your bag for all of that.

> **Unofficial.** Nothing here is documented or endorsed by RICOH. See
> [Provenance](#provenance) and [Verification status](#verification-status).

## Why this exists

The GR III has two radios and the community has reverse-engineered both, but the
existing tools each cover only one leg:

| project | covers | still needs you to |
|---|---|---|
| [dm-zharov/ricoh-gr-bluetooth-api](https://github.com/dm-zharov/ricoh-gr-bluetooth-api) | BLE GATT **documentation** | write all the code |
| [clyang/GRsync](https://github.com/clyang/GRsync) | Wi-Fi HTTP download | turn Wi-Fi on, join the AP by hand |
| [dkogan/ricoh-download](https://github.com/dkogan/ricoh-download) | Wi-Fi HTTP download + network restore | turn Wi-Fi on by hand |
| [tomdymond/pi-python-ricohgr](https://github.com/tomdymond/pi-python-ricohgr) | polls for the camera SSID | turn Wi-Fi on by hand |
| [hhornbacher/gr3x-fw-hack](https://github.com/hhornbacher/gr3x-fw-hack) | firmware research | — (does not boot) |

gr3sync joins the two legs: BLE **wakes the camera and raises its access
point**, reads the SSID and passphrase back, and only then does the HTTP
download the other tools already do well. The manual step disappears.

## How it works

```
BLE  connect (bonded pairing)
     read  Camera Power        4b445988…/b58ce84c…   0=Off 1=On 2=Sleep
     write Camera Power = On
     read  Battery Level       4b445988…/875fc41d…
     write Network Type = 1    f37f568f…/9111cdd0…   ← raises the access point
     read  SSID / Passphrase   f37f568f…/90638e5a…, /0f38279c…
     disconnect                                       ← before any bulk transfer
host join the camera's AP (nmcli / networksetup / by hand)
HTTP GET  /v1/props            model, battery
     GET  /v1/photos           {"dirs":[{"name":"100RICOH","files":[…]}]}
     GET  /v1/photos/{d}/{f}   JPEG and DNG bodies
     POST /v1/device/wlan/finish                      ← drop the AP from its side
host restore the previous Wi-Fi association           ← always, even on failure
BLE  write Camera Power = Off  (only if gr3sync was the one that woke it)
```

BLE is dropped before the transfer on purpose: Bluetooth and 2.4 GHz Wi-Fi share
an antenna on most combo radios, and an idle BLE link costs throughput on both
sides for nothing.

The Bluetooth surface is deliberately the narrowest possible — connect, read,
write **with response**, disconnect. No notifications, no subscriptions, no
writes-without-response. Those are the operations `btleplug`'s issue tracker
reports trouble with, and none of them are needed here.

## Install

```sh
cargo install --git https://github.com/sotashimozono/gr3sync
```

One static binary, no runtime to install. Rust 1.85 or newer — that floor is
verified in CI at every dependency change, not just asserted in the manifest.
On Linux you need libdbus at build time (`libdbus-1-dev` / `dbus-devel`), which
is what `btleplug` links against; macOS uses CoreBluetooth and needs nothing
extra.

Cross-compiling for a Raspberry Pi sync box:

```sh
cross build --release --target aarch64-unknown-linux-gnu
```

## Camera setup, once

1. **Pair** the host with the camera (camera menu → Bluetooth → pairing).
   Pairing is per-device and the GR III keeps essentially one partner, so
   pairing a laptop will likely displace the phone running Image Sync.
2. Set **Bluetooth → Enable Condition → "On anytime"**. Without it, BLE only
   answers while the camera is already switched on, and waking a camera that is
   off — the whole point — will not work.

`gr3sync info` prints the Enable Condition the camera currently reports.

## Usage

```sh
gr3sync pull                          # everything new, into the configured folder
gr3sync pull ~/Pictures/GR3 -j -l 20  # last 20 JPEGs only
gr3sync pull --dry-run                # say what it would do, touch nothing
gr3sync pull --no-ble                 # camera Wi-Fi already on, skip Bluetooth

gr3sync scan                          # which cameras are reachable over BLE
gr3sync info                          # model, battery, storage, power state
gr3sync doctor                        # which documented characteristics exist
gr3sync wlan on                       # raise the AP and print the credentials
gr3sync wlan off
gr3sync list                          # what is on the card (needs to be on the AP)
gr3sync get 100RICOH/R0001234.DNG
gr3sync backends                      # which Wi-Fi backends work on this host
```

Every subcommand takes `--json`. For `pull` that is a newline-delimited event
stream; for the rest it is a single JSON document. **That is the wrapper
interface** — a photo-manager plugin, a Claude Code skill or a systemd unit
should read those events rather than scrape the progress text.

```sh
gr3sync --json pull | jq -r 'select(.event=="download.done") | .photo'
```

### Diagnosis

`doctor` reads every characteristic gr3sync knows about and reports which ones
the camera actually exposes, plus any it exposes that gr3sync does not know
about. When something does not line up, `raw` pokes a single characteristic:

```sh
gr3sync raw read network_type
gr3sync raw read 9111cdd0-9f01-45c4-a2d4-e09e8fb0424d
gr3sync raw write network_type 01     # this is what 'wlan on' does
```

`raw write` pokes an undocumented device. Know what a value means first.

## Config

`~/.config/gr3sync/config.toml` — every key optional, every key has a flag that
overrides it. An unknown key is an error rather than a silent default, so a typo
cannot quietly send a sync to the wrong directory.

```toml
dest        = "~/Pictures/GR3"
address     = "AA:BB:CC:DD:EE:FF"   # skip the BLE discovery scan
min_battery = 15                    # refuse to start below this
power_off   = true                  # only ever powers off a camera it woke
keep_dirs   = true                  # dest/100RICOH/x.JPG vs dest/x.JPG
```

## What it will not do

- **Delete anything from the card.** The only writes are to the camera's power
  and WLAN characteristics; the SD card is read-only as far as gr3sync is
  concerned. A test pins the set of characteristics it is ever allowed to write.
- **Turn off a camera you switched on yourself.** `power_off` applies only when
  gr3sync did the waking.
- **Change camera state it did not create.** With `--no-ble` the access point is
  left up, because you were the one who raised it.

## Known constraints

- **The camera's Wi-Fi is AP mode only.** There is no station mode in which the
  camera joins your network. The sync host therefore loses its normal network
  for the duration of the transfer unless it has a second adapter. That is a
  camera limitation, not something gr3sync can route around.
- **Bluetooth pairing is effectively one partner.** See "Camera setup".
- **`--last N` on a large card still lists the whole card first.** `/v1/photos`
  has no pagination.
- **The `File Transfer List` characteristic is not used as a "has new photos"
  check.** It reflects the camera's *transfer queue* — images explicitly marked
  in Image Sync — not "photos you have not downloaded", so gating on it would
  silently skip everything. New files are found by diffing `/v1/photos` against
  the destination.

## Verification status

Be aware of what has and has not been exercised.

There is an emulated camera under `emulator/` — see
[`emulator/README.md`](emulator/README.md). It is worth being precise about what
that buys, because it is easy to over-read: the emulator is built from the *same
reverse-engineered specification* as gr3sync, so it agrees with gr3sync's
assumptions whether or not a real GR III does. It proves the transport chain
carries the operations and guards against regressions. It cannot tell you the
specification is right. (It stops being a shared-convention oracle once its GATT
table is rebuilt from a real camera's `doctor --json` output, which is a
supported path.)

**Verified by `cargo test --all-features`, no hardware:**

- the HTTP client and the whole download path, against a real socket server
  reproducing the camera's endpoints — including a transfer cut short mid-body
  (which must leave no `.part` file and no truncated JPEG) and a body larger
  than 10 MB (the default cap in `ureq`'s buffered read, which would otherwise
  refuse every DNG this camera produces);
- incremental sync, the ledger, and the disk/ledger agreement that stops a
  reorganised photo library from triggering a full re-download;
- Wi-Fi backend command construction and output parsing for `nmcli` and
  `networksetup`, with the subprocess boundary stubbed;
- teardown ordering: the host's association is restored and the camera's AP is
  dropped even when the pull fails;
- the BLE **protocol** — UUIDs, value encodings, and which GATT operations are
  issued in which order — against an in-memory stand-in for the transport;
- the CLI across the process boundary: argument parsing, config resolution, exit
  codes (0 / 1 "some files failed" / 2 "could not run") and the newline-delimited
  JSON contract, by running the real binary against the real emulator binary;
- in CI, the same pull against a container holding the camera's actual address,
  `192.168.0.1:80`;
- in CI, that the Bumble peripheral serving the camera's GATT table starts and
  advertises against an in-process controller.

**Not verified — needs a GR III in the room:**

- that a real camera accepts `Network Type = 1` from a non-Image-Sync client and
  actually raises its access point;
- how long the camera takes to bring the AP up (the 3 s settle and 45 s join
  timeout are estimates, not measurements);
- whether waking a fully powered-off camera over BLE works with
  `Enable Condition = On anytime`, as the specification implies;
- the real byte layouts of Storage Information and Battery Level, decoded from a
  reverse-engineered field list rather than from observed bytes;
- everything about `networksetup` on current macOS;
- `btleplug`'s CoreBluetooth backend against this particular device;
- the Bluetooth transport chain itself — btleplug → BlueZ → kernel → controller.
  GitHub-hosted runners ship a kernel with no Bluetooth modules at all, so it
  cannot be exercised there; it needs a self-hosted Linux runner or a developer
  machine. See [`emulator/README.md`](emulator/README.md).

On macOS, Bluetooth is gated per application and the OS **terminates** a process
that uses it without permission, writing nothing to stderr. gr3sync prints a
hint before touching CoreBluetooth, because after the kill there is nothing left
to report. If a Bluetooth subcommand exits with no further message, allow the
binary under System Settings > Privacy & Security > Bluetooth.

`gr3sync scan`, `info`, `doctor`, `wlan on` and `raw` exist so those can be
checked one at a time. No release is tagged until they have been.

## Contributing

`main` is protected: pull request required, and `test (ubuntu-latest)`,
`test (macos-latest)` and `binary` must pass. Squash or rebase merges only, no
force pushes, no branch deletion. Run `cargo fmt`, `cargo clippy --all-targets
--all-features -- -D warnings` and `cargo test --all-features` before opening
one.

CI also runs `cargo deny` and `cargo audit` (weekly, and on any dependency
change), CodeQL, a typo check, and a build at the declared MSRV — plus a
scheduled one against a fresh dependency resolution, which is what catches a
transitive crate raising its own Rust requirement before it reaches users.
Third-party Actions are pinned by commit SHA; see
[`.github/SECURITY.md`](.github/SECURITY.md).

The pull request template asks what you ran the change against, and singles out
"a real RICOH GR III" as its own box. That is not ceremony: the emulator shares
its assumptions with the code, so for anything touching `protocol.rs`, `ble.rs`
or a timeout, only hardware is evidence.

## Provenance

The BLE UUIDs come from
[dm-zharov/ricoh-gr-bluetooth-api](https://github.com/dm-zharov/ricoh-gr-bluetooth-api)
(Unlicense); the HTTP endpoint list from Dima Kogan's
[firmware strings dump](https://notes.secretsauce.net/notes/2022/06/16_ricoh-gr-iiix-80211-reverse-engineering.html);
the JSON envelope shapes match what [clyang/GRsync](https://github.com/clyang/GRsync)
consumes in practice. Use at your own risk.

## License

MIT.
