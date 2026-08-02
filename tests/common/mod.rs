//! A stand-in for the camera's HTTP server, and a stub Wi-Fi backend.
//!
//! The fake camera is a real socket server rather than a mock, so the client is
//! exercised against actual sockets, streaming and partial reads — the places
//! where a mock would simply agree with whatever the client happens to do.
//!
//! Shapes follow the firmware endpoint list recovered in
//! <https://notes.secretsauce.net/notes/2022/06/16_ricoh-gr-iiix-80211-reverse-engineering.html>
//! and the `errCode`/`dirs` envelope that clyang/GRsync consumes in production.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gr3sync::netlink::{Output, Runner, WifiBackend, WifiState};

#[derive(Default)]
pub struct CardState {
    pub model: String,
    pub battery: i64,
    /// directory -> filename -> body
    pub card: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    /// Filenames the server should cut short mid-body.
    pub broken: Vec<String>,
    pub wlan_finished: bool,
    pub device_finished: bool,
    pub requests: Vec<String>,
}

impl CardState {
    pub fn new() -> Self {
        Self {
            model: "RICOH GR III".into(),
            battery: 88,
            ..Default::default()
        }
    }

    pub fn add(&mut self, directory: &str, filename: &str, body: Vec<u8>) {
        self.card
            .entry(directory.to_string())
            .or_default()
            .insert(filename.to_string(), body);
    }

    /// Three RAW+JPEG pairs in one directory.
    pub fn with_three_pairs() -> Self {
        let mut state = Self::new();
        for index in 1u8..=3 {
            let stem = format!("R000000{index}");
            state.add(
                "100RICOH",
                &format!("{stem}.JPG"),
                [b"jpeg-".as_slice(), &vec![index; 512]].concat(),
            );
            state.add(
                "100RICOH",
                &format!("{stem}.DNG"),
                [b"dng-".as_slice(), &vec![index; 2048]].concat(),
            );
        }
        state
    }
}

pub struct FakeCamera {
    pub state: Arc<Mutex<CardState>>,
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
}

impl FakeCamera {
    pub fn start(state: CardState) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("local_addr");
        let state = Arc::new(Mutex::new(state));
        let shutdown = Arc::new(AtomicBool::new(false));

        let worker_state = state.clone();
        let worker_shutdown = shutdown.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if worker_shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let state = worker_state.clone();
                std::thread::spawn(move || {
                    let _ = handle(stream, state);
                });
            }
        });

        Self {
            state,
            address,
            shutdown,
        }
    }

    /// Host in the `127.0.0.1:PORT` form the client expects.
    pub fn host(&self) -> String {
        self.address.to_string()
    }

    pub fn wlan_finished(&self) -> bool {
        self.state.lock().unwrap().wlan_finished
    }
}

impl Drop for FakeCamera {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Unblock the accept loop.
        let _ = TcpStream::connect(self.address);
    }
}

fn handle(mut stream: TcpStream, state: Arc<Mutex<CardState>>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }

    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body)?;
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    state
        .lock()
        .unwrap()
        .requests
        .push(format!("{method} {path}"));

    match method.as_str() {
        "GET" => serve_get(&mut stream, &path, &state),
        "POST" => serve_post(&mut stream, &path, &state),
        _ => respond(&mut stream, 405, "text/plain", b"method not allowed"),
    }
}

fn serve_get(
    stream: &mut TcpStream,
    path: &str,
    state: &Arc<Mutex<CardState>>,
) -> std::io::Result<()> {
    if path == "/v1/ping" {
        return json(stream, 200, r#"{"errCode":200,"errMsg":"OK"}"#);
    }
    if path == "/v1/props" {
        let guard = state.lock().unwrap();
        let body = format!(
            r#"{{"errCode":200,"errMsg":"OK","model":"{}","battery":{},"firmwareVersion":"1.90","serialNo":"01234567"}}"#,
            guard.model, guard.battery
        );
        return json(stream, 200, &body);
    }
    if path == "/v1/photos" {
        let guard = state.lock().unwrap();
        let dirs: Vec<String> = guard
            .card
            .iter()
            .map(|(name, files)| {
                let listing: Vec<String> = files.keys().map(|f| format!("\"{f}\"")).collect();
                format!(r#"{{"name":"{name}","files":[{}]}}"#, listing.join(","))
            })
            .collect();
        let body = format!(
            r#"{{"errCode":200,"errMsg":"OK","dirs":[{}]}}"#,
            dirs.join(",")
        );
        return json(stream, 200, &body);
    }

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    if segments.len() == 5
        && segments[0] == "v1"
        && segments[1] == "photos"
        && segments[4] == "info"
    {
        let guard = state.lock().unwrap();
        return match guard
            .card
            .get(segments[2])
            .and_then(|files| files.get(segments[3]))
        {
            Some(body) => json(
                stream,
                200,
                &format!(r#"{{"errCode":200,"errMsg":"OK","n":{}}}"#, body.len()),
            ),
            // HTTP 404 *and* an errCode: the harder of the two shapes the real
            // camera might use, so the client has to handle both.
            None => json(stream, 404, r#"{"errCode":404,"errMsg":"not found"}"#),
        };
    }
    if segments.len() == 4 && segments[0] == "v1" && segments[1] == "photos" {
        return serve_photo(stream, segments[2], segments[3], state);
    }
    respond(stream, 404, "text/plain", b"not found")
}

fn serve_photo(
    stream: &mut TcpStream,
    directory: &str,
    filename: &str,
    state: &Arc<Mutex<CardState>>,
) -> std::io::Result<()> {
    let guard = state.lock().unwrap();
    let Some(body) = guard.card.get(directory).and_then(|f| f.get(filename)) else {
        return respond(stream, 404, "text/plain", b"not found");
    };
    let broken = guard.broken.iter().any(|b| b == filename);
    let body = body.clone();
    drop(guard);

    if broken {
        // Announce more bytes than we send, then hang up. This is what a
        // transfer cut short by the access point dropping looks like.
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len() + 4096
        );
        stream.write_all(header.as_bytes())?;
        stream.write_all(&body[..body.len() / 2])?;
        stream.flush()?;
        return Ok(());
    }
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(&body)?;
    stream.flush()
}

fn serve_post(
    stream: &mut TcpStream,
    path: &str,
    state: &Arc<Mutex<CardState>>,
) -> std::io::Result<()> {
    match path {
        "/v1/device/wlan/finish" => {
            state.lock().unwrap().wlan_finished = true;
            json(stream, 200, r#"{"errCode":200,"errMsg":"OK"}"#)
        }
        "/v1/device/finish" => {
            state.lock().unwrap().device_finished = true;
            json(stream, 200, r#"{"errCode":200,"errMsg":"OK"}"#)
        }
        _ => respond(stream, 404, "text/plain", b"not found"),
    }
}

fn json(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    respond(stream, status, "application/json", body.as_bytes())
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

// ---------------------------------------------------------------------------
// Wi-Fi stub
// ---------------------------------------------------------------------------

/// A Wi-Fi backend that records what it was asked to do and changes nothing.
pub struct StubBackend {
    pub state: WifiState,
    pub log: Mutex<Vec<String>>,
}

impl StubBackend {
    pub fn on(ssid: &str) -> Self {
        Self {
            state: WifiState {
                interface: Some("wlan0".into()),
                ssid: Some(ssid.into()),
                profile: Some(ssid.into()),
            },
            log: Mutex::new(Vec::new()),
        }
    }

    pub fn actions(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

impl WifiBackend for StubBackend {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn current(&self) -> gr3sync::Result<WifiState> {
        self.log.lock().unwrap().push("current".into());
        Ok(self.state.clone())
    }

    fn join(&self, ssid: &str, passphrase: &str, _interface: Option<&str>) -> gr3sync::Result<()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("join {ssid} {passphrase}"));
        Ok(())
    }

    fn restore(&self, state: &WifiState) -> gr3sync::Result<()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("restore {}", state.ssid.as_deref().unwrap_or("-")));
        Ok(())
    }
}

/// A [`Runner`] that fails every command, for asserting nothing shells out.
pub struct ForbiddenRunner;

impl Runner for ForbiddenRunner {
    fn run(&self, argv: &[&str], _timeout: std::time::Duration) -> gr3sync::Result<Output> {
        panic!("unexpected command: {}", argv.join(" "));
    }
}
