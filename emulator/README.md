# Emulating a RICOH GR III

Three layers, in increasing order of what they cost and what they buy.

| | what runs | what it exercises |
|---|---|---|
| **in-process** | `gr3sync::emulator::HttpCamera` inside `cargo test` | the HTTP client, over real sockets |
| **subprocess** | `gr3-emulator serve` + the real `gr3sync` binary (`tests/e2e.rs`) | argument parsing, config, exit codes, the `--json` contract |
| **container** | `docker compose up` on `192.168.0.1:80` | the address the firmware actually uses |
| **BLE** | Bumble on `/dev/vhci` (`ble_peripheral.py`) | btleplug → BlueZ → kernel → controller, for real |

## Read this before trusting a green run

The BLE layer serves a table **recorded from a real camera** —
[`gatt-captured.json`](gatt-captured.json), a RICOH GR IIIx on firmware 1.41.
Every value in it is a byte that camera sent, and every write permission is one
that camera advertised. The Wi-Fi layers are still a model of the endpoints,
not a recording.

That changes what a green run means, but not into everything. It answers:

- ✅ does the transport chain actually carry reads and writes end to end?
- ✅ does gr3sync issue the sequence of operations we think it does?
- ✅ **does gr3sync decode the bytes a real GR IIIx sends?**
- ✅ **does it stay off the characteristics that camera refuses writes on?**
- ✅ has anything regressed since it last worked?
- ✅ do the CLI's exit codes and JSON output hold up across the process boundary?

and it still cannot answer:

- ❌ does the camera *act* on `Network Type = 1` — does the radio come up?
- ❌ how long does the access point take to come up?
- ❌ does any of it behave the same on a GR III, or on another firmware?

The table is one body's answers, frozen. It cannot show you a camera changing
its mind. The repository README's "Verification status" remains the authority.

## Where the captured table came from

```sh
# with the camera in range, once
gr3sync doctor --json > doctor.json
gr3-emulator gatt --from-doctor doctor.json > emulator/gatt-captured.json

# from then on
python3 emulator/ble_peripheral.py --table emulator/gatt-captured.json
```

The committed capture is exactly that, from the session recorded in issue #9,
with one edit: the camera's Wi-Fi passphrase is replaced by a synthetic value
of the same length. Nothing else was touched.

A table built this way is labelled `"provenance": "captured_from_hardware"`.
The fallback is labelled `"specification"`, and the peripheral prints a warning
about it on startup; the `e2e-ble` job asserts that warning appears for one and
not for the other, so it cannot go quiet unnoticed.

Two things a capture carries that a specification cannot:

- characteristics the real camera did **not** expose are absent, so a test
  fails the same way the hardware would;
- write permissions are the camera's own. A report captured before `doctor`
  recorded them yields a **read-only** table rather than a fully writable one —
  an emulator that refuses a legitimate write fails a test where you can see
  it, while one that accepts a write the camera would reject hides the bug the
  read-only guard exists to catch.

## The Wi-Fi layer

```sh
docker compose -f emulator/docker-compose.yml up -d --build
gr3sync list --host 192.168.0.1
gr3sync pull /tmp/shots --no-ble --host 192.168.0.1 --wifi-backend manual
docker compose -f emulator/docker-compose.yml down
```

`--no-ble` is required — nothing in the container emulates Bluetooth.

Without Docker, the same thing as plain processes (this is what `cargo test
--all-features` runs):

```sh
cargo run --features emulator --bin gr3-emulator -- serve --bind 127.0.0.1:8080 --pairs 5
gr3sync list --host 127.0.0.1:8080
```

`--broken R0000002.JPG` makes the emulator announce a `Content-Length` it then
fails to deliver, which is what a transfer looks like when the access point
drops mid-file.

## The Bluetooth layer

`ble_peripheral.py` runs a [Bumble](https://google.github.io/bumble/) GATT
server attached to Linux's virtual HCI driver. That inserts a virtual
controller into the kernel, so BlueZ sees an ordinary peripheral and btleplug
never knows the difference:

```
gr3sync → btleplug → BlueZ (D-Bus) → kernel → /dev/vhci → ble_peripheral.py
```

Requirements: Linux, the `hci_vhci` kernel module, a running `dbus-daemon` and
`bluetoothd`, and root (vhci needs `CAP_NET_ADMIN`). It does not work on macOS,
and it does not work in an unprivileged container.

```sh
sudo modprobe hci_vhci
pip install bumble
cargo run --features emulator --bin gr3-emulator -- gatt > /tmp/gatt.json
sudo python3 emulator/ble_peripheral.py --table /tmp/gatt.json &

gr3sync scan
gr3sync doctor
gr3sync wlan on          # should print GR_EMULATED / emulated0
```

Every GATT operation the peripheral serves is echoed to its stdout as one JSON
object per line, so a test can assert on what gr3sync actually did rather than
only on what it reported doing:

```json
{"event": "write", "uuid": "9111cdd0-…", "name": "network_type", "hex": "01"}
{"event": "access_point_up", "ssid": "GR_EMULATED"}
```

The peripheral refuses writes to characteristics the table marks read-only. If
it accepted everything, a gr3sync bug that scribbled on a read-only
characteristic would only surface on the real camera.

### Status

Measured, not assumed:

- **GitHub-hosted runners cannot run this.** Their kernel (`6.17.0-1020-azure`)
  ships no Bluetooth modules at all — not `hci_vhci`, not even `bluetooth`.
  `modprobe` fails with `Module hci_vhci not found`. It is not a permissions
  problem and no amount of `sudo` fixes it.

So the `e2e-ble` CI job does two things instead:

1. runs the peripheral with `--transport local`, an in-process Bumble
   controller needing no kernel, no BlueZ and no root. That executes every line
   of `ble_peripheral.py` — table parsing, service construction, `power_on`,
   advertising — and **must pass**.
2. attempts `/dev/vhci`, and reports it unavailable *only after asserting the
   module really is absent*. A skip whose reason is checked is a skip; one that
   just passes is a lie.

The full chain therefore needs a self-hosted Linux runner or a developer
machine. A host with `hci_vhci` available runs it with the commands above.

`e2e-ble` is not in the branch protection required checks. Promote it once it
has somewhere it can actually run.

The Python here was written against Bumble 0.0.233 and its API use was checked
against that release's source.
