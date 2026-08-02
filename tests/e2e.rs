//! End-to-end: the real `gr3sync` executable against the real `gr3-emulator`
//! executable, as separate processes.
//!
//! What this adds over the in-process tests is everything between `main` and
//! the library: argument parsing, config resolution, exit codes, what actually
//! lands on stdout, and the JSON contract a wrapper depends on. A library test
//! cannot catch a CLI that exits 0 on failure or prints its events to the wrong
//! stream.
//!
//! Only the Wi-Fi half is covered here. Driving the Bluetooth half end to end
//! needs an emulated BLE controller — see `emulator/README.md` and the
//! `e2e-ble` CI job.

#![cfg(feature = "emulator")]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const GR3SYNC: &str = env!("CARGO_BIN_EXE_gr3sync");
const EMULATOR: &str = env!("CARGO_BIN_EXE_gr3-emulator");

/// A `gr3-emulator serve` child process, killed on drop.
struct Emulator {
    child: Child,
    host: String,
    files: usize,
}

impl Emulator {
    fn start(args: &[&str]) -> Self {
        let mut child = Command::new(EMULATOR)
            .args(["serve", "--bind", "127.0.0.1:0", "--announce"])
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn gr3-emulator");

        // Wait for the readiness line rather than sleeping: a fixed sleep is
        // either slower than needed or flaky under load, and usually both.
        let stdout = child.stdout.take().expect("emulator stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("emulator readiness line");
        let ready: serde_json::Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("bad readiness line {line:?}: {e}"));

        Self {
            child,
            host: ready["host"].as_str().expect("host").to_string(),
            files: ready["files"].as_u64().expect("files") as usize,
        }
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn ndjson(&self) -> Vec<serde_json::Value> {
        self.stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str(l).unwrap_or_else(|e| panic!("bad NDJSON line {l:?}: {e}"))
            })
            .collect()
    }

    fn events(&self) -> Vec<String> {
        self.ndjson()
            .iter()
            .filter_map(|e| e.get("event").and_then(|v| v.as_str()).map(String::from))
            .collect()
    }
}

/// Run gr3sync with an isolated HOME and XDG_CONFIG_HOME, so a config file on
/// the developer's machine cannot change what the test sees.
fn gr3sync(home: &Path, args: &[&str]) -> Run {
    let output = Command::new(GR3SYNC)
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .output()
        .expect("run gr3sync");
    Run {
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn sandbox() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("photos");
    (dir, dest)
}

// ---------------------------------------------------------------------------

#[test]
fn the_binary_reports_a_version_and_a_help_page() {
    let (home, _) = sandbox();
    let help = gr3sync(home.path(), &["--help"]);
    assert_eq!(help.status, 0);
    for expected in ["pull", "scan", "doctor", "wlan", "raw", "list", "get"] {
        assert!(help.stdout.contains(expected), "--help omits {expected}");
    }
    assert_eq!(gr3sync(home.path(), &["--version"]).status, 0);
}

#[test]
fn list_reports_the_whole_card() {
    let camera = Emulator::start(&["--pairs", "4"]);
    let (home, _) = sandbox();

    let run = gr3sync(home.path(), &["list", "--host", &camera.host]);
    assert_eq!(run.status, 0, "stderr: {}", run.stderr);
    assert!(run.stdout.contains("RICOH GR III"));
    assert!(run.stdout.contains(&format!("{} files", camera.files)));
}

#[test]
fn a_full_pull_lands_the_files_and_exits_zero() {
    let camera = Emulator::start(&["--pairs", "3"]);
    let (home, dest) = sandbox();

    let run = gr3sync(
        home.path(),
        &[
            "pull",
            dest.to_str().unwrap(),
            "--no-ble",
            "--host",
            &camera.host,
            "--wifi-backend",
            "manual",
        ],
    );

    assert_eq!(run.status, 0, "stderr: {}", run.stderr);
    let landed: Vec<String> = std::fs::read_dir(dest.join("100RICOH"))
        .expect("destination directory")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(landed.len(), 6, "{landed:?}");
    assert!(run.stdout.contains("downloaded 6 files"));
}

#[test]
fn a_second_pull_exits_zero_having_done_nothing() {
    let camera = Emulator::start(&["--pairs", "2"]);
    let (home, dest) = sandbox();
    let args = [
        "pull",
        dest.to_str().unwrap(),
        "--no-ble",
        "--host",
        &camera.host,
        "--wifi-backend",
        "manual",
    ];

    assert_eq!(gr3sync(home.path(), &args).status, 0);
    let again = gr3sync(home.path(), &args);
    assert_eq!(again.status, 0);
    assert!(
        again.stdout.contains("downloaded 0 files"),
        "{}",
        again.stdout
    );
    assert!(again.stdout.contains("skipped 4"), "{}", again.stdout);
}

#[test]
fn a_failed_file_exits_one_while_the_rest_still_land() {
    // Exit 1 means "ran, but some files failed"; exit 2 means "could not run".
    // A wrapper retries the second and not the first, so the distinction has to
    // survive all the way out through the process boundary.
    let camera = Emulator::start(&["--pairs", "3", "--broken", "R0000002.JPG"]);
    let (home, dest) = sandbox();

    let run = gr3sync(
        home.path(),
        &[
            "pull",
            dest.to_str().unwrap(),
            "--no-ble",
            "--host",
            &camera.host,
            "--wifi-backend",
            "manual",
        ],
    );

    assert_eq!(
        run.status, 1,
        "stdout: {}\nstderr: {}",
        run.stdout, run.stderr
    );
    assert!(run.stdout.contains("1 FAILED"), "{}", run.stdout);
    assert!(dest.join("100RICOH/R0000001.JPG").exists());
    assert!(!dest.join("100RICOH/R0000002.JPG").exists());
    assert!(dest.join("100RICOH/R0000003.JPG").exists());
}

#[test]
fn an_unreachable_camera_exits_two_and_says_so_on_stderr() {
    let (home, dest) = sandbox();
    let run = gr3sync(
        home.path(),
        &[
            "pull",
            dest.to_str().unwrap(),
            "--no-ble",
            "--host",
            "127.0.0.1:1",
            "--wifi-backend",
            "manual",
        ],
    );

    assert_eq!(run.status, 2, "stdout: {}", run.stdout);
    assert!(
        run.stderr.contains("did not answer"),
        "stderr: {}",
        run.stderr
    );
}

#[test]
fn dry_run_touches_nothing_on_disk() {
    let camera = Emulator::start(&["--pairs", "2"]);
    let (home, dest) = sandbox();

    let run = gr3sync(
        home.path(),
        &[
            "pull",
            dest.to_str().unwrap(),
            "--no-ble",
            "--host",
            &camera.host,
            "--wifi-backend",
            "manual",
            "--dry-run",
        ],
    );

    assert_eq!(run.status, 0);
    assert!(run.stdout.contains("would download 4 files"));
    assert!(!dest.join("100RICOH").exists());
}

#[test]
fn json_pull_is_parseable_ndjson_ending_in_done() {
    // This is the wrapper contract: one JSON object per line, every line
    // carrying an `event`, terminated by `done` with the summary.
    let camera = Emulator::start(&["--pairs", "2"]);
    let (home, dest) = sandbox();

    let run = gr3sync(
        home.path(),
        &[
            "--json",
            "pull",
            dest.to_str().unwrap(),
            "--no-ble",
            "--host",
            &camera.host,
            "--wifi-backend",
            "manual",
        ],
    );

    assert_eq!(run.status, 0, "stderr: {}", run.stderr);
    let events = run.events();
    assert_eq!(events.last().map(String::as_str), Some("done"));
    assert_eq!(events.iter().filter(|e| *e == "download.done").count(), 4);

    let done = run.ndjson().pop().unwrap();
    assert_eq!(done["ok"], serde_json::json!(true));
    assert_eq!(done["downloaded"].as_array().unwrap().len(), 4);
    assert!(done["bytes_written"].as_u64().unwrap() > 0);

    // Human-readable progress must not be mixed into the machine stream.
    assert!(!run.stdout.contains("gr3sync ->"), "{}", run.stdout);
}

#[test]
fn json_list_is_a_single_document() {
    let camera = Emulator::start(&["--pairs", "2"]);
    let (home, _) = sandbox();

    let run = gr3sync(
        home.path(),
        &["--json", "list", "--host", &camera.host, "-j"],
    );
    assert_eq!(run.status, 0, "stderr: {}", run.stderr);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("one JSON document");
    assert_eq!(value["model"], "RICOH GR III");
    assert_eq!(value["photos"].as_array().unwrap().len(), 2);
}

#[test]
fn get_downloads_one_named_file() {
    let camera = Emulator::start(&["--pairs", "1"]);
    let (home, dest) = sandbox();

    let run = gr3sync(
        home.path(),
        &[
            "--json",
            "get",
            "100RICOH/R0000001.DNG",
            "--dest",
            dest.to_str().unwrap(),
            "--host",
            &camera.host,
        ],
    );

    assert_eq!(run.status, 0, "stderr: {}", run.stderr);
    assert!(dest.join("100RICOH/R0000001.DNG").exists());
}

#[test]
fn a_config_file_supplies_the_destination() {
    let camera = Emulator::start(&["--pairs", "1"]);
    let (home, dest) = sandbox();
    let config_dir = home.path().join("config").join("gr3sync");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        // A TOML literal string, because a native path is not an escape
        // sequence: `dest = "C:\Users\..."` is a parse error, not a path.
        format!("dest = '{}'\nmin_battery = 0\n", dest.display()),
    )
    .unwrap();

    let run = gr3sync(
        home.path(),
        &[
            "pull",
            "--no-ble",
            "--host",
            &camera.host,
            "--wifi-backend",
            "manual",
        ],
    );

    assert_eq!(run.status, 0, "stderr: {}", run.stderr);
    assert!(dest.join("100RICOH/R0000001.JPG").exists());
}

#[test]
fn a_typo_in_the_config_stops_the_run_instead_of_syncing_somewhere_else() {
    let (home, _) = sandbox();
    let config_dir = home.path().join("config").join("gr3sync");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.toml"), "destination = \"/wrong\"\n").unwrap();

    let run = gr3sync(home.path(), &["config"]);
    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("destination"), "stderr: {}", run.stderr);
}

#[test]
fn bluetooth_subcommands_leave_the_user_with_something_to_act_on() {
    // The requirement is not that these work — that needs a camera — but that
    // the user is never left with silence.
    //
    // Four host shapes reach this test, and the invariant has to hold on all
    // of them, because bring-up runs the suite on the machine that has the
    // camera:
    //
    // * Linux without an adapter -> gr3sync catches it and prints
    //   "gr3sync: Bluetooth is unavailable: ...".
    // * macOS without Bluetooth permission -> the OS *terminates the process*.
    //   There is no error to catch and nothing is written to stderr by the
    //   kill, so the only thing that can help is the hint gr3sync prints
    //   before touching CoreBluetooth. Observed on a macos-latest runner:
    //   non-zero exit, completely empty stderr, before the hint existed.
    // * a working adapter and no camera -> `scan` treats "found nothing" as a
    //   normal empty result and says so on *stdout*, exiting 1; the other four
    //   raise CameraNotFound on stderr and exit 2.
    // * a working adapter and a reachable camera -> they succeed.
    //
    // Only CI is known to have no camera, so only CI can demand a failure.
    let cameraless_host = std::env::var_os("CI").is_some();
    let (home, _) = sandbox();
    for args in [
        vec!["scan", "--timeout", "1"],
        vec!["info", "--timeout", "1"],
        vec!["doctor", "--timeout", "1"],
        vec!["wlan", "on", "--timeout", "1"],
        vec!["raw", "read", "network_type", "--timeout", "1"],
    ] {
        let run = gr3sync(home.path(), &args);
        let context = format!(
            "{args:?} -> status {} stdout {:?} stderr {:?}",
            run.status, run.stdout, run.stderr
        );

        assert!(!run.stderr.contains("panicked"), "panicked: {context}");
        if cameraless_host {
            assert_ne!(run.status, 0, "unexpectedly succeeded: {context}");
        }
        if run.status != 0 {
            assert!(
                run.stderr.contains("gr3sync:")
                    || run.stderr.contains("Privacy & Security")
                    || !run.stdout.trim().is_empty(),
                "the user was left with nothing actionable: {context}"
            );
        }
    }
}

#[test]
fn the_emulator_can_print_its_gatt_table() {
    // The Bluetooth peripheral in the container is fed from this.
    let output = Command::new(EMULATOR).args(["gatt"]).output().expect("run");
    assert!(output.status.success());
    let table: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(table["provenance"], "specification");
    assert!(table["characteristics"]
        .as_object()
        .unwrap()
        .contains_key("9111cdd0-9f01-45c4-a2d4-e09e8fb0424d"));
}

#[test]
fn a_gatt_table_built_from_a_doctor_report_is_labelled_as_hardware() {
    // The path that turns the emulator from an assumption into a recording.
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("doctor.json");
    std::fs::write(
        &report,
        serde_json::json!({
            "documented": [{
                "name": "network_type",
                "uuid": "9111cdd0-9f01-45c4-a2d4-e09e8fb0424d",
                "exposed": true,
                "value": {"hex": "01", "text": ""}
            }],
            "undocumented_present": []
        })
        .to_string(),
    )
    .unwrap();

    let output = Command::new(EMULATOR)
        .args(["gatt", "--from-doctor", report.to_str().unwrap()])
        .output()
        .expect("run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let table: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(table["provenance"], "captured_from_hardware");
}
