//! Command line interface.
//!
//! Built as a set of small, independently runnable subcommands rather than one
//! monolithic `sync`. Two reasons:
//!
//! * the BLE leg cannot be exercised without the camera in the room, so each
//!   step of it has to be pokeable on its own (`scan`, `info`, `wlan on`,
//!   `doctor`, `raw`) when something misbehaves;
//! * a wrapper — a photo-manager plugin, a Claude Code skill, a systemd timer —
//!   should reach for exactly the step it needs and read `--json` output rather
//!   than parse progress text.

use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::json;
use uuid::Uuid;

use crate::ble::{self, Gatt};
use crate::camera::{select, Camera, Filter, PhotoRef};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::netlink;
use crate::protocol as p;
use crate::sync::{self, local_path, Options};

#[derive(Parser)]
#[command(
    name = "gr3sync",
    version,
    about = "Sync photos off a RICOH GR III over Bluetooth + Wi-Fi.",
    after_help = "Run 'gr3sync pull' for the whole thing; the other subcommands are the individual steps."
)]
struct Cli {
    /// Machine-readable output (newline-delimited JSON for `pull`).
    #[arg(long, global = true)]
    json: bool,

    /// Show every event, including ones the human view normally hides.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Wake the camera, pull new photos, and put everything back.
    Pull(PullArgs),
    /// List GR cameras reachable over Bluetooth.
    Scan(ScanArgs),
    /// Read model, battery and storage over Bluetooth.
    Info(BleArgs),
    /// Report which documented characteristics this camera actually exposes.
    Doctor(BleArgs),
    /// Turn the camera's Wi-Fi access point on or off over Bluetooth.
    Wlan(WlanArgs),
    /// Read or write a single characteristic by UUID. For diagnosis.
    #[command(subcommand)]
    Raw(RawCommand),
    /// List files on the card over Wi-Fi.
    List(ListArgs),
    /// Download named files over Wi-Fi, e.g. 100RICOH/R0001234.JPG.
    Get(GetArgs),
    /// Show the config file and its resolved values.
    Config,
    /// Show which Wi-Fi control backends work on this host.
    Backends,
}

#[derive(Args)]
struct BleArgs {
    /// Camera BLE address, skipping the ambiguity check.
    #[arg(long)]
    address: Option<String>,
    /// BLE scan timeout, in seconds.
    #[arg(long, default_value_t = 10.0)]
    timeout: f64,
}

#[derive(Args)]
struct ScanArgs {
    #[command(flatten)]
    ble: BleArgs,
    /// Do not filter by device name.
    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct WlanArgs {
    #[arg(value_enum)]
    state: WlanState,
    #[command(flatten)]
    ble: BleArgs,
}

#[derive(Clone, Copy, ValueEnum)]
enum WlanState {
    On,
    Off,
}

#[derive(Args)]
struct FilterArgs {
    /// JPEG only.
    #[arg(short = 'j', long)]
    jpg: bool,
    /// DNG only.
    #[arg(short = 'r', long)]
    raw: bool,
    /// Only the last N matching files.
    #[arg(short = 'l', long)]
    last: Option<usize>,
    /// Restrict to one card directory, e.g. 100RICOH.
    #[arg(short = 'd', long)]
    dir: Option<String>,
}

impl FilterArgs {
    /// `--jpg`/`--raw` are additive; neither flag means "both".
    fn formats(&self) -> (bool, bool) {
        match (self.jpg, self.raw) {
            (true, false) => (true, false),
            (false, true) => (false, true),
            _ => (true, true),
        }
    }
}

#[derive(Args)]
struct PullArgs {
    /// Destination directory (default: from config).
    dest: Option<String>,
    #[command(flatten)]
    ble: BleArgs,
    #[command(flatten)]
    filter: FilterArgs,
    /// Skip Bluetooth; the camera's Wi-Fi must already be on.
    #[arg(long)]
    no_ble: bool,
    /// Leave the camera switched on afterwards.
    #[arg(long)]
    no_power_off: bool,
    /// Ignore card directories and put files straight in the destination.
    #[arg(long)]
    flatten: bool,
    /// List what would be downloaded; download nothing.
    #[arg(long)]
    dry_run: bool,
    /// Camera HTTP address (default 192.168.0.1).
    #[arg(long)]
    host: Option<String>,
    /// Force a Wi-Fi backend.
    #[arg(long)]
    wifi_backend: Option<String>,
    /// Force a Wi-Fi interface.
    #[arg(long)]
    wifi_interface: Option<String>,
    /// Refuse to start below this battery percentage.
    #[arg(long)]
    min_battery: Option<i8>,
}

#[derive(Args)]
struct ListArgs {
    #[command(flatten)]
    filter: FilterArgs,
    #[arg(long)]
    host: Option<String>,
    /// Seconds to wait for the camera to answer.
    #[arg(long, default_value_t = 5.0)]
    timeout: f64,
}

#[derive(Args)]
struct GetArgs {
    /// Files to fetch, each as DIR/FILE.
    #[arg(required = true, value_name = "DIR/FILE")]
    photos: Vec<String>,
    #[arg(long)]
    dest: Option<String>,
    #[arg(long)]
    flatten: bool,
    #[arg(long)]
    host: Option<String>,
}

#[derive(Subcommand)]
enum RawCommand {
    /// Read one characteristic and print it as hex and as text.
    Read {
        /// Characteristic UUID, or a name from `gr3sync doctor`.
        name_or_uuid: String,
        #[command(flatten)]
        ble: BleArgs,
    },
    /// Write hex bytes to one characteristic.
    ///
    /// This pokes an undocumented device. Know what the value means first.
    Write {
        name_or_uuid: String,
        /// Bytes as hex, e.g. `01` or `0a1b`.
        hex: String,
        #[command(flatten)]
        ble: BleArgs,
    },
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(&cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("gr3sync: {err}");
            ExitCode::from(err.exit_code() as u8)
        }
    }
}

fn dispatch(cli: &Cli) -> Result<ExitCode> {
    match &cli.command {
        Command::Pull(args) => cmd_pull(cli, args),
        Command::Scan(args) => cmd_scan(cli, args),
        Command::Info(args) => cmd_info(cli, args),
        Command::Doctor(args) => cmd_doctor(cli, args),
        Command::Wlan(args) => cmd_wlan(cli, args),
        Command::Raw(args) => cmd_raw(cli, args),
        Command::List(args) => cmd_list(cli, args),
        Command::Get(args) => cmd_get(cli, args),
        Command::Config => cmd_config(cli),
        Command::Backends => cmd_backends(cli),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Ble(format!("starting the async runtime: {e}")))
}

fn secs(value: f64) -> Duration {
    Duration::from_secs_f64(value.max(0.0))
}

/// Warn about macOS's Bluetooth permission gate *before* touching CoreBluetooth.
///
/// macOS terminates a process that uses Bluetooth without permission, and the
/// termination produces no output of its own: the user sees a silent non-zero
/// exit with nothing to act on. There is no error to catch afterwards, so the
/// only place the hint can go is in front of the call. Observed on a CI runner,
/// where `gr3sync scan` died with an empty stderr.
#[cfg(target_os = "macos")]
fn bluetooth_permission_hint() {
    eprintln!(
        "note: macOS gates Bluetooth per application. If this exits with no further \n\
         message, allow it under System Settings > Privacy & Security > Bluetooth."
    );
}

#[cfg(not(target_os = "macos"))]
fn bluetooth_permission_hint() {}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

fn print_json(value: &serde_json::Value) {
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer_pretty(&mut out, value);
    let _ = out.write_all(b"\n");
}

/// Render one sync event. `None` means "not worth showing unless verbose".
fn render(event: &serde_json::Value) -> Option<String> {
    let string = |key: &str| {
        event
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string()
    };
    let number = |key: &str| event.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
    Some(match event.get("event")?.as_str()? {
        "ble.scan" => "  scanning for the camera over Bluetooth...".into(),
        "ble.found" => format!(
            "  found {} at {}",
            event
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("camera"),
            string("address")
        ),
        "ble.awake" => {
            let woken = event
                .get("woken_by_us")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            format!(
                "  camera was {}{}",
                string("was").to_lowercase(),
                if woken { " -> woke it" } else { "" }
            )
        }
        "ble.battery" => format!(
            "  battery {}% ({})",
            number("level"),
            string("source").to_lowercase()
        ),
        "ble.ap_up" => format!("  camera Wi-Fi up: {}", string("ssid")),
        "ble.skipped" => "  skipping Bluetooth (turn the camera's Wi-Fi on by hand)".into(),
        "wifi.join" => format!(
            "  joining {} (was on {})",
            string("ssid"),
            event
                .get("from")
                .and_then(|v| v.as_str())
                .unwrap_or("nothing")
        ),
        "http.props" => format!(
            "  {}, battery {}%",
            string("model"),
            event.get("battery").and_then(|v| v.as_i64()).unwrap_or(-1)
        ),
        "http.listed" => format!("  {} files on the card", number("total")),
        "plan" => format!(
            "  {} to download, {} already here",
            number("pending"),
            number("skipped")
        ),
        "download.start" => format!(
            "  [{}/{}] {}",
            number("index"),
            number("of"),
            string("photo")
        ),
        "download.failed" => format!("      FAILED: {}", string("error")),
        "ledger.save_failed" => {
            format!("  WARNING: could not save the ledger: {}", string("error"))
        }
        "wifi.restore" => format!(
            "  back to {}",
            event
                .get("ssid")
                .and_then(|v| v.as_str())
                .unwrap_or("the previous network")
        ),
        "wifi.restore_failed" => format!(
            "  WARNING: could not restore the previous network: {}",
            string("error")
        ),
        "ble.power_off_failed" => format!(
            "  note: could not power the camera off: {}",
            string("error")
        ),
        "done" => {
            let mib = event
                .get("bytes_written")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as f64
                / (1024.0 * 1024.0);
            let count = |key: &str| {
                event
                    .get(key)
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0)
            };
            let verb = if event
                .get("dry_run")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                "would download"
            } else {
                "downloaded"
            };
            let mut line = format!(
                "  {verb} {} files ({mib:.1} MiB), skipped {}",
                count("downloaded"),
                count("skipped")
            );
            if count("failed") > 0 {
                line.push_str(&format!(", {} FAILED", count("failed")));
            }
            line
        }
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

fn cmd_pull(cli: &Cli, args: &PullArgs) -> Result<ExitCode> {
    let config = Config::load()?;
    let mut options = Options::from_config(&config, args.dest.as_deref());
    let (jpeg, raw) = args.filter.formats();
    options.use_ble = !args.no_ble;
    options.jpeg = jpeg;
    options.raw = raw;
    options.last = args.filter.last;
    options.directory = args.filter.dir.clone();
    options.dry_run = args.dry_run;
    options.keep_dirs = !args.flatten;
    options.address = args.ble.address.clone().or(options.address);
    options.scan_timeout = secs(args.ble.timeout);
    if let Some(host) = &args.host {
        options.host = host.clone();
    }
    if args.wifi_backend.is_some() {
        options.wifi_backend = args.wifi_backend.clone();
    }
    if args.wifi_interface.is_some() {
        options.wifi_interface = args.wifi_interface.clone();
    }
    if let Some(floor) = args.min_battery {
        options.min_battery = floor;
    }
    if args.no_power_off {
        options.power_off = false;
    }

    if options.use_ble {
        bluetooth_permission_hint();
    }
    if !cli.json {
        println!("gr3sync -> {}", options.dest.display());
    }

    let as_json = cli.json;
    let verbose = cli.verbose;
    let mut sink = move |event: serde_json::Value| {
        if as_json {
            let mut out = std::io::stdout().lock();
            let _ = serde_json::to_writer(&mut out, &event);
            let _ = out.write_all(b"\n");
            let _ = out.flush();
        } else if let Some(line) = render(&event) {
            println!("{line}");
        } else if verbose {
            println!(
                "  · {}",
                event.get("event").and_then(|v| v.as_str()).unwrap_or("?")
            );
        }
    };

    let outcome = runtime()?.block_on(sync::run(&options, &mut sink))?;
    Ok(if outcome.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn cmd_scan(cli: &Cli, args: &ScanArgs) -> Result<ExitCode> {
    bluetooth_permission_hint();
    let found = runtime()?.block_on(ble::scan(secs(args.ble.timeout), args.all))?;
    if cli.json {
        print_json(&serde_json::to_value(&found).unwrap_or(serde_json::Value::Null));
        return Ok(ExitCode::SUCCESS);
    }
    if found.is_empty() {
        println!(
            "no camera found. Is the camera paired with this host and Bluetooth enabled on it?"
        );
        return Ok(ExitCode::from(1));
    }
    for camera in &found {
        let rssi = camera
            .rssi
            .map(|r| format!("  {r} dBm"))
            .unwrap_or_default();
        println!(
            "{}  {}{rssi}",
            camera.address,
            camera.name.as_deref().unwrap_or("(unnamed)")
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Open a session, run `body`, and always disconnect.
fn with_session<T>(
    ble_args: &BleArgs,
    body: impl AsyncFnOnce(&ble::Session<ble::BtleplugGatt>) -> Result<T>,
) -> Result<T> {
    bluetooth_permission_hint();
    let config = Config::load()?;
    let address = ble_args.address.clone().or(config.address);
    let timeout = secs(ble_args.timeout);
    runtime()?.block_on(async move {
        let target = ble::find_one(address.as_deref(), timeout).await?;
        let gatt = ble::BtleplugGatt::connect(&target.address, timeout).await?;
        let session = ble::Session::new(gatt);
        let result = body(&session).await;
        session.gatt().disconnect().await;
        result
    })
}

fn cmd_info(cli: &Cli, args: &BleArgs) -> Result<ExitCode> {
    let info = with_session(args, async |session| {
        // Each read is independent: one unsupported characteristic must not
        // blank out the rest of the report.
        let text = |r: Result<String>| r.unwrap_or_else(|e| format!("<unavailable: {e}>"));
        Ok(json!({
            "model": text(session.model().await),
            "firmware": text(session.firmware().await),
            "serial": text(session.serial().await),
            "power": session.power().await.map(|v| v.name().to_string())
                .unwrap_or_else(|e| format!("<unavailable: {e}>")),
            "wlan": session.network_type().await.map(|v| v.name().to_string())
                .unwrap_or_else(|e| format!("<unavailable: {e}>")),
            "ble_enable_condition": session.ble_enable_condition().await
                .map(|v| v.name().to_string())
                .unwrap_or_else(|e| format!("<unavailable: {e}>")),
            "battery": session.battery().await.map(|b| json!({
                "level": b.level, "source": b.source.name()
            })).unwrap_or_else(|e| json!(format!("<unavailable: {e}>"))),
            "storage": session.storage().await.map(|slots| json!(slots))
                .unwrap_or_else(|e| json!(format!("<unavailable: {e}>"))),
        }))
    })?;
    if cli.json {
        print_json(&info);
    } else if let Some(object) = info.as_object() {
        for (key, value) in object {
            println!("{key:>22}: {value}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_doctor(cli: &Cli, args: &BleArgs) -> Result<ExitCode> {
    let report = with_session(args, async |session| {
        let present = session.gatt().available();
        let mut rows = Vec::new();
        for (name, uuid) in p::KNOWN_CHARACTERISTICS {
            let exposed = present.contains(uuid);
            let value = if exposed {
                match session.gatt().read(*uuid).await {
                    Ok(bytes) => json!({ "hex": hex(&bytes), "text": p::decode_utf8(&bytes) }),
                    Err(err) => json!({ "error": err.to_string() }),
                }
            } else {
                serde_json::Value::Null
            };
            rows.push(json!({
                "name": name,
                "uuid": uuid.to_string(),
                // Recorded so `gr3-emulator gatt --from-doctor` can rebuild a
                // peripheral from this report: a GATT client reaches a
                // characteristic through its service, not by UUID alone.
                "service": p::service_of(*uuid).map(|s| s.to_string()),
                "exposed": exposed,
                "value": value
            }));
        }
        let known: Vec<Uuid> = p::KNOWN_CHARACTERISTICS.iter().map(|(_, u)| *u).collect();
        let unknown: Vec<String> = present
            .iter()
            .filter(|u| !known.contains(u))
            .map(|u| u.to_string())
            .collect();
        Ok(json!({ "documented": rows, "undocumented_present": unknown }))
    })?;

    if cli.json {
        print_json(&report);
        return Ok(ExitCode::SUCCESS);
    }
    for row in report["documented"].as_array().into_iter().flatten() {
        let mark = if row["exposed"].as_bool().unwrap_or(false) {
            "yes"
        } else {
            "NO "
        };
        let value = row["value"]
            .get("hex")
            .and_then(|v| v.as_str())
            .map(|h| format!("  {h}"))
            .unwrap_or_default();
        println!("{mark}  {:<24}{value}", row["name"].as_str().unwrap_or(""));
    }
    let unknown = report["undocumented_present"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    if unknown > 0 {
        println!("\n{unknown} characteristic(s) present that gr3sync does not know about:");
        for uuid in report["undocumented_present"]
            .as_array()
            .into_iter()
            .flatten()
        {
            println!("  {}", uuid.as_str().unwrap_or(""));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_wlan(cli: &Cli, args: &WlanArgs) -> Result<ExitCode> {
    let state = args.state;
    let result = with_session(&args.ble, async move |session| match state {
        WlanState::Off => {
            session.stop_ap().await?;
            Ok(json!({ "wlan": "Off" }))
        }
        WlanState::On => {
            session.wake(Duration::from_millis(1500)).await?;
            let credentials = session.start_ap(Duration::from_secs(3)).await?;
            Ok(json!({
                "wlan": "ApMode",
                "ssid": credentials.ssid,
                "passphrase": credentials.passphrase
            }))
        }
    })?;

    if cli.json {
        print_json(&result);
    } else if matches!(state, WlanState::Off) {
        println!("camera Wi-Fi off");
    } else {
        println!("SSID: {}", result["ssid"].as_str().unwrap_or(""));
        println!("Pass: {}", result["passphrase"].as_str().unwrap_or(""));
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_raw(cli: &Cli, command: &RawCommand) -> Result<ExitCode> {
    let (name, ble_args, payload) = match command {
        RawCommand::Read { name_or_uuid, ble } => (name_or_uuid, ble, None),
        RawCommand::Write {
            name_or_uuid,
            hex,
            ble,
        } => (name_or_uuid, ble, Some(parse_hex(hex)?)),
    };
    let uuid = resolve_characteristic(name)?;
    let result = with_session(ble_args, async move |session| match &payload {
        Some(bytes) => {
            session.gatt().write(uuid, bytes).await?;
            Ok(json!({ "uuid": uuid.to_string(), "wrote": hex(bytes) }))
        }
        None => {
            let bytes = session.gatt().read(uuid).await?;
            Ok(json!({
                "uuid": uuid.to_string(),
                "hex": hex(&bytes),
                "text": p::decode_utf8(&bytes),
                "len": bytes.len()
            }))
        }
    })?;
    if cli.json {
        print_json(&result);
    } else {
        println!("{result:#}");
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_list(cli: &Cli, args: &ListArgs) -> Result<ExitCode> {
    let config = Config::load()?;
    let host = args.host.clone().unwrap_or_else(|| config.host.clone());
    let camera = Camera::new(&host, Duration::from_secs(15));
    camera.wait_until_up(secs(args.timeout)).map_err(|_| {
        Error::Http(format!(
            "no camera at {host}. Join the camera's Wi-Fi first, or use 'gr3sync wlan on' \
             to raise it over Bluetooth."
        ))
    })?;

    let props = camera.props()?;
    let (jpeg, raw) = args.filter.formats();
    let photos = select(
        &camera.photos()?,
        Filter {
            jpeg,
            raw,
            last: args.filter.last,
            directory: args.filter.dir.as_deref(),
        },
    );

    if cli.json {
        print_json(&json!({
            "model": props.model,
            "battery": props.battery,
            "photos": photos.iter().map(|p| json!({
                "dir": p.directory, "file": p.filename, "key": p.key()
            })).collect::<Vec<_>>()
        }));
        return Ok(ExitCode::SUCCESS);
    }
    println!("{}, battery {}%", props.model, props.battery.unwrap_or(-1));
    for photo in &photos {
        println!("{}", photo.key());
    }
    println!("{} files", photos.len());
    Ok(ExitCode::SUCCESS)
}

fn cmd_get(cli: &Cli, args: &GetArgs) -> Result<ExitCode> {
    let config = Config::load()?;
    let host = args.host.clone().unwrap_or_else(|| config.host.clone());
    let camera = Camera::new(&host, Duration::from_secs(300));
    let dest = config.resolved_dest(args.dest.as_deref());
    let props = camera.props()?;

    let mut written = Vec::new();
    for key in &args.photos {
        let photo = parse_photo_key(key)?;
        let target = local_path(&dest, &photo, !args.flatten);
        let bytes = camera.download(&photo, &target, props.is_legacy_path())?;
        if !cli.json {
            println!(
                "{} -> {} ({:.0} KiB)",
                photo.key(),
                target.display(),
                bytes as f64 / 1024.0
            );
        }
        written.push(
            json!({ "photo": photo.key(), "path": target.display().to_string(), "bytes": bytes }),
        );
    }
    if cli.json {
        print_json(&serde_json::Value::Array(written));
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_config(cli: &Cli) -> Result<ExitCode> {
    let path = Config::path();
    let config = Config::load()?;
    let mut value = serde_json::to_value(&config).unwrap_or(serde_json::Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.insert("_path".into(), json!(path.display().to_string()));
        object.insert("_exists".into(), json!(path.exists()));
        object.insert(
            "_resolved_dest".into(),
            json!(config.resolved_dest(None).display().to_string()),
        );
    }
    if cli.json {
        print_json(&value);
    } else if let Some(object) = value.as_object() {
        for (key, item) in object {
            println!("{key:>16}: {item}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_backends(cli: &Cli) -> Result<ExitCode> {
    let rows: Vec<serde_json::Value> = netlink::BACKEND_NAMES
        .iter()
        .map(|name| json!({ "name": name, "available": netlink::backend_available(name) }))
        .collect();
    if cli.json {
        print_json(&serde_json::Value::Array(rows));
    } else {
        for row in &rows {
            let mark = if row["available"].as_bool().unwrap_or(false) {
                "yes"
            } else {
                "no "
            };
            let name = row["name"].as_str().unwrap_or("");
            let note = if name == "manual" {
                "  (asks you to switch networks)"
            } else {
                ""
            };
            println!("{mark}  {name}{note}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

pub fn parse_photo_key(key: &str) -> Result<PhotoRef> {
    match key.rsplit_once('/') {
        Some((directory, filename)) if !directory.is_empty() && !filename.is_empty() => {
            Ok(PhotoRef::new(directory, filename))
        }
        _ => Err(Error::Config(format!(
            "{key:?} must be given as DIR/FILE, e.g. 100RICOH/R0001234.JPG"
        ))),
    }
}

/// Accept either a raw UUID or one of the friendly names `doctor` prints.
pub fn resolve_characteristic(name_or_uuid: &str) -> Result<Uuid> {
    if let Ok(uuid) = Uuid::parse_str(name_or_uuid) {
        return Ok(uuid);
    }
    p::KNOWN_CHARACTERISTICS
        .iter()
        .find(|(name, _)| *name == name_or_uuid)
        .map(|(_, uuid)| *uuid)
        .ok_or_else(|| {
            Error::Config(format!(
                "{name_or_uuid:?} is neither a UUID nor a known characteristic. Known: {}",
                p::KNOWN_CHARACTERISTICS
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

pub fn parse_hex(text: &str) -> Result<Vec<u8>> {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':')
        .collect();
    if cleaned.is_empty() || cleaned.len() % 2 != 0 {
        return Err(Error::Config(format!(
            "{text:?} is not an even-length hex string"
        )));
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&cleaned[i..i + 2], 16)
                .map_err(|_| Error::Config(format!("{text:?} contains a non-hex digit")))
        })
        .collect()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_parser_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_subcommand_is_required() {
        assert!(Cli::try_parse_from(["gr3sync"]).is_err());
    }

    #[test]
    fn pull_accepts_the_full_flag_set() {
        let cli = Cli::try_parse_from([
            "gr3sync",
            "pull",
            "/tmp/shots",
            "--no-ble",
            "--dry-run",
            "--flatten",
            "-r",
            "-l",
            "5",
            "-d",
            "101RICOH",
        ])
        .unwrap();
        let Command::Pull(args) = cli.command else {
            panic!("wrong subcommand")
        };
        assert_eq!(args.dest.as_deref(), Some("/tmp/shots"));
        assert!(args.no_ble && args.dry_run && args.flatten);
        assert_eq!(args.filter.last, Some(5));
        assert_eq!(args.filter.dir.as_deref(), Some("101RICOH"));
        assert_eq!(args.filter.formats(), (false, true));
    }

    #[test]
    fn neither_format_flag_means_both() {
        let cli = Cli::try_parse_from(["gr3sync", "list"]).unwrap();
        let Command::List(args) = cli.command else {
            panic!()
        };
        assert_eq!(args.filter.formats(), (true, true));
    }

    #[test]
    fn both_format_flags_also_mean_both() {
        let cli = Cli::try_parse_from(["gr3sync", "list", "-j", "-r"]).unwrap();
        let Command::List(args) = cli.command else {
            panic!()
        };
        assert_eq!(args.filter.formats(), (true, true));
    }

    #[test]
    fn wlan_only_takes_on_or_off() {
        assert!(Cli::try_parse_from(["gr3sync", "wlan", "on"]).is_ok());
        assert!(Cli::try_parse_from(["gr3sync", "wlan", "sideways"]).is_err());
    }

    #[test]
    fn json_is_accepted_on_every_subcommand() {
        for argv in [
            vec!["gr3sync", "--json", "list"],
            vec!["gr3sync", "--json", "scan"],
            vec!["gr3sync", "--json", "config"],
            vec!["gr3sync", "--json", "backends"],
            vec!["gr3sync", "--json", "doctor"],
        ] {
            assert!(Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
        }
    }

    #[test]
    fn photo_keys_must_name_a_directory() {
        assert_eq!(
            parse_photo_key("100RICOH/R0000001.JPG").unwrap(),
            PhotoRef::new("100RICOH", "R0000001.JPG")
        );
        assert!(parse_photo_key("R0000001.JPG").is_err());
        assert!(parse_photo_key("100RICOH/").is_err());
    }

    #[test]
    fn characteristics_resolve_by_name_or_uuid() {
        assert_eq!(
            resolve_characteristic("network_type").unwrap(),
            p::CHAR_NETWORK_TYPE
        );
        assert_eq!(
            resolve_characteristic("9111cdd0-9f01-45c4-a2d4-e09e8fb0424d").unwrap(),
            p::CHAR_NETWORK_TYPE
        );
        let err = resolve_characteristic("nonsense").unwrap_err();
        assert!(err.to_string().contains("network_type"), "{err}");
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(parse_hex("01").unwrap(), vec![1]);
        assert_eq!(parse_hex("0a1b").unwrap(), vec![0x0a, 0x1b]);
        assert_eq!(parse_hex("0a:1b").unwrap(), vec![0x0a, 0x1b]);
        assert_eq!(hex(&[0x0a, 0x1b]), "0a1b");
    }

    #[test]
    fn malformed_hex_is_rejected() {
        assert!(parse_hex("").is_err());
        assert!(parse_hex("0").is_err());
        assert!(parse_hex("zz").is_err());
    }

    #[test]
    fn the_done_event_renders_a_summary() {
        let line = render(&json!({
            "event": "done",
            "dry_run": false,
            "bytes_written": 5 * 1024 * 1024,
            "downloaded": ["a", "b"],
            "skipped": ["c"],
            "failed": [{"photo": "d", "error": "boom"}]
        }))
        .unwrap();
        assert!(line.contains("downloaded 2 files (5.0 MiB)"), "{line}");
        assert!(line.contains("1 FAILED"), "{line}");
    }

    #[test]
    fn noise_events_are_hidden_from_the_human_view() {
        assert!(render(&json!({"event": "ble.disconnected"})).is_none());
        assert!(render(&json!({"event": "download.done", "photo": "a", "bytes": 1})).is_none());
    }
}
