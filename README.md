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

gr3sync joins the two legs: it uses BLE to **wake the camera and raise its
access point**, reads the SSID and passphrase back over BLE, and only then does
the HTTP download that the other tools already do well. The manual step
disappears.

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
host restore the previous Wi-Fi association           ← in a finally block
BLE  write Camera Power = Off  (only if gr3sync was the one that woke it)
```

BLE is dropped before the transfer on purpose: Bluetooth and 2.4 GHz Wi-Fi share
an antenna on most combo radios, and an idle BLE link costs throughput on both
sides for nothing.

## Install

```sh
pip install 'gr3sync[ble]'          # full: Bluetooth + Wi-Fi
pip install gr3sync                 # Wi-Fi only, zero dependencies
```

The Wi-Fi half is standard library only, so it can be dropped onto any machine
that happens to be near the camera. `bleak` is needed only for the Bluetooth leg.

## Camera setup, once

1. **Pair** the host with the camera (camera menu → Bluetooth → pairing).
   Bluetooth pairing is per-device and the GR III keeps essentially one partner,
   so pairing a laptop will likely displace the phone running Image Sync.
2. Set **Bluetooth → Enable Condition → "On anytime"**. Without this, BLE only
   answers while the camera is already switched on, and waking a camera that is
   off — the whole point — will not work.

## Usage

```sh
gr3sync pull                        # everything new, into the configured folder
gr3sync pull ~/Pictures/GR3 -j -l 20  # last 20 JPEGs only
gr3sync pull --dry-run              # say what it would do, touch nothing
gr3sync pull --no-ble               # camera Wi-Fi already on, skip Bluetooth

gr3sync scan                        # which cameras are reachable over BLE
gr3sync info                        # model, battery, storage, power state
gr3sync wlan on                     # just raise the AP and print the credentials
gr3sync wlan off
gr3sync list                        # what is on the card (needs to be on the AP)
gr3sync get 100RICOH/R0001234.DNG
gr3sync backends                    # which Wi-Fi control backends work here
```

Every subcommand takes `--json`. For `pull` that is a newline-delimited event
stream; for the rest it is a single JSON document. **That is the wrapper
interface** — a photo-manager plugin, a Claude Code skill or a systemd unit
should read those events rather than scrape the progress text.

```python
from gr3sync import SyncOptions, run_sync

result = run_sync(SyncOptions(dest=Path("~/Pictures/GR3").expanduser()), print)
print(result.downloaded, result.failed)
```

## Config

`~/.config/gr3sync/config.toml` — every key optional, every key has a flag that
overrides it.

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
  concerned.
- **Turn off a camera you switched on yourself.** `power_off` applies only when
  gr3sync did the waking.
- **Change camera state it did not create.** With `--no-ble`, the camera's AP is
  left up, because you were the one who raised it.

## Known constraints

- **The camera's Wi-Fi is AP mode only.** There is no station mode in which the
  camera joins your network. The sync host therefore loses its normal network
  for the duration of the transfer unless it has a second adapter. This is a
  camera limitation, not something gr3sync can route around.
- **Bluetooth pairing is effectively one partner.** See "Camera setup" above.
- **`--last N` on a large card still lists the whole card first.** `/v1/photos`
  has no pagination.
- **The `File Transfer List` characteristic is not used as a "has new photos"
  check.** It reflects the camera's *transfer queue* (images explicitly marked
  in Image Sync), not "photos you have not downloaded", so gating on it would
  silently skip everything. New files are found by diffing `/v1/photos` against
  the destination.

## Verification status

Be aware of what has and has not been exercised.

**Verified by the test suite** (`pytest`, no hardware):

- the HTTP client and the whole download path, against a real socket server that
  reproduces the camera's endpoints and JSON envelope — including a transfer cut
  short mid-body, which must leave no `.part` file and no truncated JPEG;
- incremental sync, the ledger, and the disk/ledger agreement that stops a
  reorganised photo library from triggering a full re-download;
- Wi-Fi backend command construction and output parsing for `nmcli` and
  `networksetup`, with the subprocess boundary stubbed;
- teardown ordering: the host's network is restored and the camera's AP is
  dropped even when the pull raises;
- the BLE **protocol** — UUIDs, value encodings, and which GATT operations are
  issued in which order — against a stand-in for `bleak`.

**Not verified — needs a GR III in the room:**

- that a real camera accepts `Network Type = 1` from a non-Image-Sync client and
  actually raises its AP;
- how long the camera takes to bring the AP up (the 3 s settle and 45 s join
  timeout are estimates);
- whether waking a fully powered-off camera over BLE works with
  `Enable Condition = On anytime`, as the specification implies;
- the real characteristic layouts for Storage Information and Battery Level,
  which are decoded from a reverse-engineered field list rather than observed
  bytes;
- everything about `networksetup` on current macOS.

`gr3sync scan`, `gr3sync info` and `gr3sync wlan on` exist so those can be
checked one at a time.

## Provenance

Nothing here is officially documented by RICOH. The BLE UUIDs come from
[dm-zharov/ricoh-gr-bluetooth-api](https://github.com/dm-zharov/ricoh-gr-bluetooth-api)
(Unlicense); the HTTP endpoint list comes from Dima Kogan's
[firmware strings dump](https://notes.secretsauce.net/notes/2022/06/16_ricoh-gr-iiix-80211-reverse-engineering.html);
the JSON envelope shapes match what [clyang/GRsync](https://github.com/clyang/GRsync)
consumes in practice. Use at your own risk.

## License

MIT.
