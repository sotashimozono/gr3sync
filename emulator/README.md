# Emulating a RICOH GR III

Three layers, in increasing order of what they cost and what they buy.

| | what runs | what it exercises |
|---|---|---|
| **in-process** | `gr3sync::emulator::HttpCamera` inside `cargo test` | the HTTP client, over real sockets |
| **subprocess** | `gr3-emulator serve` + the real `gr3sync` binary (`tests/e2e.rs`) | argument parsing, config, exit codes, the `--json` contract |
| **container** | `docker compose up` on `192.168.0.1:80` | the address the firmware actually uses |
| **BLE** | Bumble on `/dev/vhci` (`ble_peripheral.py`) | btleplug → BlueZ → kernel → controller, for real |

## Read this before trusting a green run

**The emulator is built from the same reverse-engineered specification as
gr3sync.** It is an oracle that shares a convention with the thing it is
testing. If the specification is wrong about what `Network Type = 1` does, the
emulator is wrong in exactly the same way and every test still passes.

So an emulator run answers:

- ✅ does the transport chain actually carry reads and writes end to end?
- ✅ does gr3sync issue the sequence of operations we think it does?
- ✅ has anything regressed since it last worked?
- ✅ do the CLI's exit codes and JSON output hold up across the process boundary?

and it cannot answer:

- ❌ does a real camera accept `Network Type = 1` from a non-Image-Sync client?
- ❌ how long does the access point take to come up?
- ❌ does BLE reach a camera that is switched off?
- ❌ are the Storage Information and Battery Level byte layouts right?

Those need hardware. The repository README's "Verification status" is the
authority on which is which.

## Making it a real oracle

The GATT table is data, not code, and it can be replaced with one captured from
an actual camera. Once that is done the table encodes observation rather than
assumption, and the same tests start meaning something:

```sh
# with the camera in range, once
gr3sync doctor --json > doctor.json
gr3-emulator gatt --from-doctor doctor.json > emulator/gatt-captured.json

# from then on
python3 emulator/ble_peripheral.py --table emulator/gatt-captured.json
```

A table built this way is labelled `"provenance": "captured_from_hardware"`;
the default one is labelled `"specification"` and the peripheral prints a
warning about it on startup. Nothing silently pretends to be evidence.

Characteristics the real camera did **not** expose are dropped from a captured
table, so a test then fails the same way the hardware would.

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
