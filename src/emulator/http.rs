//! The camera's Wi-Fi HTTP API, emulated.
//!
//! A real socket server rather than a mock, so the client is exercised against
//! actual sockets, streaming and partial reads. Used three ways:
//!
//! * in-process by the unit and integration tests;
//! * as the `gr3-emulator` binary, so the end-to-end tests can drive the real
//!   `gr3sync` executable against it as a separate process;
//! * inside a container pinned to `192.168.0.1`, so even the address is the one
//!   the camera actually uses.
//!
//! Shapes follow the firmware endpoint list recovered in
//! <https://notes.secretsauce.net/notes/2022/06/16_ricoh-gr-iiix-80211-reverse-engineering.html>
//! and the `errCode`/`dirs` envelope that clyang/GRsync consumes in practice.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The synthetic SD card, plus the switches a test needs to make things fail.
#[derive(Debug, Clone)]
pub struct Card {
    pub model: String,
    pub battery: i64,
    /// directory -> filename -> body
    pub files: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    /// Filenames the server should cut short mid-body, to exercise the client's
    /// handling of an access point that drops during a transfer.
    pub broken: Vec<String>,
    pub wlan_finished: bool,
    pub device_finished: bool,
    pub requests: Vec<String>,
}

impl Default for Card {
    fn default() -> Self {
        Self {
            model: "RICOH GR III".into(),
            battery: 88,
            files: BTreeMap::new(),
            broken: Vec::new(),
            wlan_finished: false,
            device_finished: false,
            requests: Vec::new(),
        }
    }
}

impl Card {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, directory: &str, filename: &str, body: Vec<u8>) {
        self.files
            .entry(directory.to_string())
            .or_default()
            .insert(filename.to_string(), body);
    }

    /// `pairs` RAW+JPEG pairs in `100RICOH`, with plausible sizes.
    pub fn with_pairs(pairs: usize) -> Self {
        let mut card = Self::new();
        for index in 1..=pairs {
            let stem = format!("R{index:07}");
            card.add(
                "100RICOH",
                &format!("{stem}.JPG"),
                synthetic_body(b"jpeg", index, 512),
            );
            card.add(
                "100RICOH",
                &format!("{stem}.DNG"),
                synthetic_body(b"dng-", index, 2048),
            );
        }
        card
    }

    pub fn count(&self) -> usize {
        self.files.values().map(|f| f.len()).sum()
    }
}

fn synthetic_body(tag: &[u8], index: usize, repeat: usize) -> Vec<u8> {
    let mut body = tag.to_vec();
    body.extend(std::iter::repeat_n(index as u8, repeat));
    body
}

pub struct HttpCamera {
    pub card: Arc<Mutex<Card>>,
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
}

impl HttpCamera {
    /// Bind and start serving. Port 0 asks the OS for a free port.
    pub fn bind(bind_address: &str, card: Card) -> std::io::Result<Self> {
        let listener = TcpListener::bind(bind_address)?;
        let address = listener.local_addr()?;
        let card = Arc::new(Mutex::new(card));
        let shutdown = Arc::new(AtomicBool::new(false));

        let worker_card = card.clone();
        let worker_shutdown = shutdown.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if worker_shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let card = worker_card.clone();
                std::thread::spawn(move || {
                    let _ = handle(stream, card);
                });
            }
        });

        Ok(Self {
            card,
            address,
            shutdown,
        })
    }

    /// Host in the `ADDRESS:PORT` form the client expects.
    pub fn host(&self) -> String {
        self.address.to_string()
    }

    pub fn port(&self) -> u16 {
        self.address.port()
    }

    pub fn wlan_finished(&self) -> bool {
        self.card.lock().unwrap().wlan_finished
    }

    pub fn device_finished(&self) -> bool {
        self.card.lock().unwrap().device_finished
    }

    pub fn requests(&self) -> Vec<String> {
        self.card.lock().unwrap().requests.clone()
    }

    /// Block until the process is killed. Used by the `gr3-emulator` binary.
    pub fn serve_forever(&self) -> ! {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
}

impl Drop for HttpCamera {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Unblock the accept loop.
        let _ = TcpStream::connect(self.address);
    }
}

fn handle(mut stream: TcpStream, card: Arc<Mutex<Card>>) -> std::io::Result<()> {
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
    card.lock()
        .unwrap()
        .requests
        .push(format!("{method} {path}"));

    match method.as_str() {
        "GET" => serve_get(&mut stream, &path, &card),
        "POST" => serve_post(&mut stream, &path, &card),
        _ => respond(&mut stream, 405, "text/plain", b"method not allowed"),
    }
}

fn serve_get(stream: &mut TcpStream, path: &str, card: &Arc<Mutex<Card>>) -> std::io::Result<()> {
    if path == "/v1/ping" {
        return json(stream, 200, r#"{"errCode":200,"errMsg":"OK"}"#);
    }
    if path == "/v1/props" {
        let guard = card.lock().unwrap();
        let body = format!(
            r#"{{"errCode":200,"errMsg":"OK","model":"{}","battery":{},"firmwareVersion":"1.90","serialNo":"01234567"}}"#,
            guard.model, guard.battery
        );
        return json(stream, 200, &body);
    }
    if path == "/v1/photos" {
        let guard = card.lock().unwrap();
        let dirs: Vec<String> = guard
            .files
            .iter()
            .map(|(name, files)| {
                let listing: Vec<String> = files.keys().map(|f| format!("\"{f}\"")).collect();
                format!(r#"{{"name":"{name}","files":[{}]}}"#, listing.join(","))
            })
            .collect();
        return json(
            stream,
            200,
            &format!(
                r#"{{"errCode":200,"errMsg":"OK","dirs":[{}]}}"#,
                dirs.join(",")
            ),
        );
    }

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    if segments.len() == 5
        && segments[0] == "v1"
        && segments[1] == "photos"
        && segments[4] == "info"
    {
        let guard = card.lock().unwrap();
        return match guard
            .files
            .get(segments[2])
            .and_then(|f| f.get(segments[3]))
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
        return serve_photo(stream, segments[2], segments[3], card);
    }
    respond(stream, 404, "text/plain", b"not found")
}

fn serve_photo(
    stream: &mut TcpStream,
    directory: &str,
    filename: &str,
    card: &Arc<Mutex<Card>>,
) -> std::io::Result<()> {
    let guard = card.lock().unwrap();
    let Some(body) = guard.files.get(directory).and_then(|f| f.get(filename)) else {
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
        return stream.flush();
    }
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(&body)?;
    stream.flush()
}

fn serve_post(stream: &mut TcpStream, path: &str, card: &Arc<Mutex<Card>>) -> std::io::Result<()> {
    match path {
        "/v1/device/wlan/finish" => {
            card.lock().unwrap().wlan_finished = true;
            json(stream, 200, r#"{"errCode":200,"errMsg":"OK"}"#)
        }
        "/v1/device/finish" => {
            card.lock().unwrap().device_finished = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_of_pairs_has_two_files_per_shot() {
        let card = Card::with_pairs(3);
        assert_eq!(card.count(), 6);
        assert!(card.files["100RICOH"].contains_key("R0000001.JPG"));
        assert!(card.files["100RICOH"].contains_key("R0000001.DNG"));
    }

    #[test]
    fn binding_to_port_zero_reports_the_real_port() {
        let camera = HttpCamera::bind("127.0.0.1:0", Card::with_pairs(1)).unwrap();
        assert!(camera.port() > 0);
        assert!(camera.host().starts_with("127.0.0.1:"));
    }
}
