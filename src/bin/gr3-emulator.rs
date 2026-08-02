//! A fake RICOH GR III, for end-to-end tests.
//!
//! Serves the camera's Wi-Fi HTTP API, and can emit the GATT table as JSON for
//! the Bluetooth peripheral (`emulator/ble_peripheral.py`) to serve.
//!
//! What a green test against this proves is limited — see
//! `gr3sync::emulator::gatt` and the README's "Verification status". It is a
//! transport and regression harness, not evidence about real hardware.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use gr3sync::emulator::{Card, GattTable, HttpCamera};

#[derive(Parser)]
#[command(
    name = "gr3-emulator",
    version,
    about = "Emulate a RICOH GR III for testing."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the camera's HTTP API.
    Serve {
        /// Address to bind. The container pins this to the camera's real
        /// address so even that is not a variable in the test.
        #[arg(long, default_value = "0.0.0.0:80")]
        bind: String,
        /// How many RAW+JPEG pairs to put on the synthetic card.
        #[arg(long, default_value_t = 3)]
        pairs: usize,
        /// Filenames to cut short mid-transfer, to exercise failure handling.
        #[arg(long)]
        broken: Vec<String>,
        /// Print the chosen port and exit codes on stdout, one JSON line, so a
        /// test harness can wait for readiness instead of sleeping.
        #[arg(long)]
        announce: bool,
    },
    /// Print the GATT table as JSON.
    Gatt {
        /// Rebuild the table from a real camera's `gr3sync doctor --json`
        /// output instead of from the specification. This is what turns the
        /// emulator from an assumption into a recording.
        #[arg(long, value_name = "FILE")]
        from_doctor: Option<String>,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("gr3-emulator: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    match Cli::parse().command {
        Command::Serve {
            bind,
            pairs,
            broken,
            announce,
        } => {
            let mut card = Card::with_pairs(pairs);
            card.broken = broken;
            let total = card.count();
            let camera =
                HttpCamera::bind(&bind, card).map_err(|e| format!("binding {bind}: {e}"))?;
            if announce {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "ready",
                        "host": camera.host(),
                        "port": camera.port(),
                        "files": total
                    })
                );
            } else {
                eprintln!(
                    "gr3-emulator listening on {} with {total} files",
                    camera.host()
                );
            }
            camera.serve_forever();
        }
        Command::Gatt { from_doctor } => {
            let table = match from_doctor {
                Some(path) => {
                    let text = std::fs::read_to_string(&path)
                        .map_err(|e| format!("reading {path}: {e}"))?;
                    let report: serde_json::Value =
                        serde_json::from_str(&text).map_err(|e| format!("parsing {path}: {e}"))?;
                    GattTable::from_doctor_report(&report)?
                }
                None => GattTable::specification(),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&table).map_err(|e| e.to_string())?
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}
