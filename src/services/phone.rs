//! Phone ↔ PC transfer over the local network, like the .NET build: a small HTTP/1.1 server (std::net, a
//! thread per connection) serves a browser page the phone opens by scanning a QR code on the notch.
//! Uploads land in the receive folder (and on the Shelf); the PC offers files, folders and text through an
//! outbox the page shows live (Server-Sent Events). Every request needs the key from the QR code, which is
//! kept DPAPI-protected in phone-key.bin and forgotten when Phone is turned off. Off until turned on.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::{settings, shelf, store};

const PAGE: &str = include_str!("../../assets/phone.html");
const PORTS: [i32; 2] = [53318, 53328];
const KEY_FILE: &str = "phone-key.bin";
const KEY_ENTROPY: &[u8] = b"WinNotch.PhoneKey";
const MAX_TEXT: usize = 1 << 20;

// ---------------------------------------------------------------- state

#[derive(Clone)]
pub struct Transfer {
    pub id: u64,
    pub title: String,
    pub peer: String,
    pub incoming: bool,
    pub total: u64,
    bytes: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    pub state: String,
    pub active: bool,
    pub failed: bool,
    /// Text the phone sent (clicking the row copies it).
    pub text: Option<String>,
    /// Where a received file was saved.
    pub saved_path: Option<String>,
    pub at: String,
}

impl Transfer {
    pub fn progress(&self) -> f32 {
        if !self.active {
            return if self.failed { 0.0 } else { 1.0 };
        }
        if self.total == 0 {
            return 0.0;
        }
        (self.bytes.load(Ordering::Relaxed) as f64 / self.total as f64).clamp(0.0, 1.0) as f32
    }

    pub fn direction(&self) -> String {
        if self.incoming {
            format!("From {}", self.peer)
        } else {
            format!("To {}", self.peer)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OutKind {
    File,
    Folder,
    Text,
}

#[derive(Clone)]
pub struct OutItem {
    pub id: String,
    pub kind: OutKind,
    pub name: String,
    pub path: Option<String>,
    pub text: Option<String>,
    pub size: u64,
    pub sent_at: String,
    pub state: String,
}

impl OutItem {
    pub fn detail(&self) -> String {
        match self.kind {
            OutKind::Text => "Text".into(),
            OutKind::Folder => "Folder · .zip".into(),
            OutKind::File => format_size(self.size),
        }
    }
}

#[derive(Default)]
struct Phone {
    running: bool,
    starting: bool,
    status: String,
    web_url: Option<String>,
    key: String,
    /// Bumped on every start/stop; the accept loop of an older server exits.
    generation: u64,
    transfers: Vec<Transfer>,
    outbox: Vec<OutItem>,
    events: Vec<(u64, Sender<String>)>,
    next_id: u64,
}

fn cell() -> &'static Mutex<Phone> {
    static CELL: OnceLock<Mutex<Phone>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(Phone::default()))
}

fn with<R>(f: impl FnOnce(&mut Phone) -> R) -> R {
    f(&mut cell().lock().unwrap_or_else(|e| e.into_inner()))
}

/// What the Phone view shows.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub running: bool,
    pub starting: bool,
    pub status: String,
    pub web_url: Option<String>,
    pub connected: bool,
    pub transfers: Vec<Transfer>,
    pub outbox: Vec<OutItem>,
}

pub fn snapshot() -> Snapshot {
    with(|p| Snapshot {
        running: p.running,
        starting: p.starting,
        status: p.status.clone(),
        web_url: p.web_url.clone(),
        connected: p.running && !p.events.is_empty(),
        transfers: p.transfers.clone(),
        outbox: p.outbox.clone(),
    })
}

/// The app's colors for the page (JSON): the scheme's surfaces and text (opaque, as the panel without glass)
/// and the accent, so the page looks like the app. Read from Settings → Appearance each time: an open page
/// follows a change live (see `events`).
pub fn page_theme() -> String {
    let s = settings::shared();
    let sc = crate::theme::scheme(&s.theme_scheme);
    let t = crate::theme::current();
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let hex = |c: crate::gfx::Color| format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b));
    let rgb = |v: u32| format!("#{:06x}", v & 0xFF_FFFF);
    serde_json::json!({
        "accent": hex(t.accent),
        "onAccent": hex(t.on_accent),
        "bg": rgb(sc.pill),
        "card": rgb(sc.surface),
        "card2": rgb(sc.surface2),
        "line": rgb(sc.hover),
        "text": rgb(sc.text),
        "sub": rgb(sc.subtext),
        "light": sc.light,
    })
    .to_string()
}

pub fn receive_folder() -> PathBuf {
    let s = settings::get();
    if !s.phone_receive_folder.trim().is_empty() {
        return PathBuf::from(s.phone_receive_folder.trim());
    }
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Downloads").join("FathNotch")
}

/// How the page names this PC (FATHNOTCH_PC_NAME overrides it, for screenshots).
fn pc_name() -> String {
    std::env::var("FATHNOTCH_PC_NAME")
        .ok()
        .filter(|n| !n.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "PC".into())
}

pub fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.0} KB", b / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        trim_zero(format!("{:.1}", b / (1024.0 * 1024.0))) + " MB"
    } else {
        trim_zero(format!("{:.2}", b / (1024.0 * 1024.0 * 1024.0))) + " GB"
    }
}

fn trim_zero(s: String) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

// ---------------------------------------------------------------- start / stop

/// Starts the server or turns it off to match the `phone_enabled` setting. Blocking (call in background).
pub fn sync() {
    if settings::get().phone_enabled {
        start();
    } else {
        turn_off();
    }
}

/// Turns Phone on (saves the setting). Blocking.
pub fn turn_on() {
    settings::update_quiet(|s| s.phone_enabled = true);
    start();
    if !with(|p| p.running) {
        settings::update_quiet(|s| s.phone_enabled = false);
    }
}

/// Turns Phone off and forgets the key, so the old link stops working for good.
pub fn turn_off() {
    settings::update_quiet(|s| s.phone_enabled = false);
    stop(false);
    let _ = std::fs::remove_file(store::path(KEY_FILE));
}

fn start() {
    let gen = {
        let mut p = cell().lock().unwrap_or_else(|e| e.into_inner());
        if p.running || p.starting {
            return;
        }
        p.starting = true;
        p.status.clear();
        p.generation += 1;
        p.generation
    };
    crate::bg::refresh();
    let key = load_or_create_key();
    let saved = settings::get().phone_port;
    let mut choices: Vec<i32> = vec![saved];
    choices.extend(PORTS);
    choices.dedup();
    let mut bound = None;
    for port in choices.into_iter().filter(|p| *p > 0 && *p < 65536) {
        if let Ok(l) = TcpListener::bind(("0.0.0.0", port as u16)) {
            bound = Some((l, port));
            break;
        }
    }
    let Some((listener, port)) = bound else {
        with(|p| {
            p.starting = false;
            p.status = "Couldn't open a network port.".into();
        });
        return;
    };
    if saved != port {
        settings::update_quiet(|s| s.phone_port = port);
    }
    let ip = best_local_address();
    with(|p| {
        p.key = key.clone();
        p.running = true;
        p.starting = false;
        p.web_url = ip.map(|ip| format!("http://{ip}:{port}/?k={key}"));
        if p.web_url.is_none() {
            p.status = "Not connected to a network. Join the same Wi-Fi as your phone.".into();
        }
    });
    let _ = listener.set_nonblocking(true);
    std::thread::spawn(move || accept_loop(listener, gen));
}

fn stop(keep_status: bool) {
    with(|p| {
        p.generation += 1;
        p.running = false;
        p.web_url = None;
        p.key.clear();
        for t in p.transfers.iter().filter(|t| t.active) {
            t.cancel.store(true, Ordering::Relaxed);
        }
        p.events.clear(); // ends every live connection
        if !keep_status {
            p.status.clear();
        }
    });
    crate::bg::refresh();
}

fn accept_loop(listener: TcpListener, gen: u64) {
    loop {
        if with(|p| p.generation) != gen {
            return; // dropping the listener closes the port
        }
        match listener.accept() {
            Ok((stream, addr)) => {
                let _ = stream.set_nonblocking(false);
                std::thread::spawn(move || {
                    let _ = handle(stream, addr);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(60)),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

// ---------------------------------------------------------------- key

fn load_or_create_key() -> String {
    let path = store::path(KEY_FILE);
    if let Ok(bytes) = std::fs::read(&path) {
        if let Some(plain) = store::unprotect(&bytes, KEY_ENTROPY) {
            if let Ok(k) = String::from_utf8(plain) {
                if k.len() >= 12 {
                    return k;
                }
            }
        }
    }
    let key = random_id();
    if let Some(c) = store::protect(key.as_bytes(), KEY_ENTROPY) {
        let _ = std::fs::write(&path, c);
    }
    key
}

/// Unguessable, URL-safe and easy to read out (no 0/o, 1/l).
fn random_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    super::cliphistory::random_bytes(12).iter().map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char).collect()
}

/// The address a phone on the same Wi-Fi most likely reaches: the one the default route uses.
/// (A UDP "connect" sends nothing; it only picks the outgoing interface.)
fn best_local_address() -> Option<std::net::Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    for target in ["192.168.0.1:9", "10.0.0.1:9", "8.8.8.8:53"] {
        if s.connect(target).is_ok() {
            if let Ok(SocketAddr::V4(a)) = s.local_addr() {
                let ip = *a.ip();
                if !ip.is_unspecified() && !ip.is_loopback() && !ip.is_link_local() {
                    return Some(ip);
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------- PC side

/// Offers files and folders to the phone (already offered paths are skipped).
pub fn send(paths: &[String]) {
    let mut added = false;
    with(|p| {
        for raw in paths {
            let path = std::fs::canonicalize(raw)
                .map(|x| x.to_string_lossy().trim_start_matches(r"\\?\").to_string())
                .unwrap_or_else(|_| raw.clone());
            if p.outbox.iter().any(|o| o.path.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(&path))) {
                continue;
            }
            let pp = Path::new(&path);
            let name = pp.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Folder".into());
            let item = if pp.is_file() {
                OutItem {
                    id: random_id(),
                    kind: OutKind::File,
                    name,
                    size: std::fs::metadata(pp).map(|m| m.len()).unwrap_or(0),
                    path: Some(path.clone()),
                    text: None,
                    sent_at: shelf::now_string(),
                    state: "Waiting for phone".into(),
                }
            } else if pp.is_dir() {
                OutItem {
                    id: random_id(),
                    kind: OutKind::Folder,
                    name,
                    size: 0,
                    path: Some(path.clone()),
                    text: None,
                    sent_at: shelf::now_string(),
                    state: "Waiting for phone".into(),
                }
            } else {
                continue;
            };
            p.outbox.insert(0, item);
            added = true;
        }
    });
    if added {
        push("outbox");
    }
}

pub fn send_text(text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let line = text.trim().lines().next().unwrap_or("").trim();
    let name = if line.chars().count() > 60 { line.chars().take(60).collect::<String>() + "…" } else { line.to_string() };
    with(|p| {
        p.outbox.insert(
            0,
            OutItem {
                id: random_id(),
                kind: OutKind::Text,
                name,
                path: None,
                text: Some(text.to_string()),
                size: 0,
                sent_at: shelf::now_string(),
                state: "Waiting for phone".into(),
            },
        )
    });
    push("outbox");
}

/// Sends whatever is on the clipboard (files or text). False when there's nothing usable.
pub fn send_clipboard() -> bool {
    if let Some(files) = clipboard_files() {
        send(&files);
        return true;
    }
    if let Some(t) = crate::clipboard::get_text() {
        if !t.trim().is_empty() {
            send_text(&t);
            return true;
        }
    }
    false
}

fn clipboard_files() -> Option<Vec<String>> {
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    unsafe {
        IsClipboardFormatAvailable(15).ok()?;
        OpenClipboard(None).ok()?;
        let mut out = Vec::new();
        if let Ok(h) = GetClipboardData(15) {
            let d = HDROP(h.0);
            let n = DragQueryFileW(d, u32::MAX, None);
            for i in 0..n {
                let len = DragQueryFileW(d, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(d, i, Some(&mut buf));
                out.push(String::from_utf16_lossy(&buf[..len]));
            }
        }
        let _ = CloseClipboard();
        (!out.is_empty()).then_some(out)
    }
}

pub fn remove_outbox(id: &str) {
    let removed = with(|p| {
        let n = p.outbox.len();
        p.outbox.retain(|o| o.id != id);
        n != p.outbox.len()
    });
    if removed {
        push("outbox");
    }
}

pub fn clear_outbox() {
    let had = with(|p| !std::mem::take(&mut p.outbox).is_empty());
    if had {
        push("outbox");
    }
}

pub fn cancel(id: u64) {
    with(|p| {
        if let Some(t) = p.transfers.iter().find(|t| t.id == id && t.active) {
            t.cancel.store(true, Ordering::Relaxed);
        }
    });
}

pub fn set_transfer_state(id: u64, state: &str) {
    with(|p| {
        if let Some(t) = p.transfers.iter_mut().find(|t| t.id == id) {
            t.state = state.to_string();
        }
    });
    crate::bg::refresh();
}

/// Tells every open page that something changed ("outbox", "shelf", "received"). Any thread.
pub fn push(name: &str) {
    with(|p| p.events.retain(|(_, tx)| tx.send(name.to_string()).is_ok()));
    crate::bg::refresh();
}

fn track(title: String, peer: String, incoming: bool, total: u64, state: &str) -> Transfer {
    let t = with(|p| {
        p.next_id += 1;
        let t = Transfer {
            id: p.next_id,
            title,
            peer,
            incoming,
            total,
            bytes: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            state: state.to_string(),
            active: true,
            failed: false,
            text: None,
            saved_path: None,
            at: shelf::now_string(),
        };
        p.transfers.insert(0, t.clone());
        while p.transfers.len() > 20 {
            match p.transfers.iter().rposition(|t| !t.active) {
                Some(i) => {
                    p.transfers.remove(i);
                }
                None => break,
            }
        }
        t
    });
    crate::bg::refresh();
    t
}

/// An alert on the notch for a finished transfer (a burst of files shows as one updating alert);
/// clicking it opens the Phone tool.
fn notify(key: &str, icon: &'static str, title: &str, sub: String) {
    crate::model::push_alert(crate::notch::Alert {
        key: format!("phone:{key}"),
        kind: crate::notch::AlertKind::Full { icon, title: title.to_string(), sub, open: Some("phone") },
        duration: std::time::Duration::from_millis(3200),
    });
}

fn finish(id: u64, state: &str, failed: bool, f: impl FnOnce(&mut Transfer)) {
    with(|p| {
        if let Some(t) = p.transfers.iter_mut().find(|t| t.id == id) {
            t.active = false;
            t.failed = failed;
            t.state = state.to_string();
            f(t);
        }
    });
    crate::bg::refresh();
}

// ---------------------------------------------------------------- HTTP

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    peer: String,
}

impl Request {
    fn q(&self, k: &str) -> &str {
        self.query.get(k).map(String::as_str).unwrap_or("")
    }
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.get(&k.to_ascii_lowercase()).map(String::as_str)
    }
    fn content_length(&self) -> Option<u64> {
        self.header("content-length").and_then(|v| v.trim().parse().ok())
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read_request(reader: &mut BufReader<TcpStream>, addr: SocketAddr) -> Option<Request> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = HashMap::new();
    let mut total = 0;
    loop {
        let mut h = String::new();
        let n = reader.read_line(&mut h).ok()?;
        total += n;
        if n == 0 || total > 64 * 1024 {
            return None;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let (path, qs) = target.split_once('?').unwrap_or((&target, ""));
    let query = qs
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect();
    let ip = match addr {
        SocketAddr::V4(a) => a.ip().to_string(),
        SocketAddr::V6(a) => a.ip().to_ipv4_mapped().map_or_else(|| a.ip().to_string(), |v| v.to_string()),
    };
    Some(Request { method, path: percent_decode(path), query, headers, peer: format!("Browser ({ip})") })
}

fn respond(w: &mut TcpStream, status: u16, ctype: &str, body: &[u8], extra: &[(&str, String)]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        416 => "Range Not Satisfiable",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    w.write_all(body)?;
    w.flush()
}

fn status_only(w: &mut TcpStream, status: u16) -> std::io::Result<()> {
    respond(w, status, "text/plain; charset=utf-8", b"", &[])
}

fn json(w: &mut TcpStream, v: &serde_json::Value) -> std::io::Result<()> {
    respond(w, 200, "application/json; charset=utf-8", v.to_string().as_bytes(), &[])
}

fn allowed(r: &Request) -> bool {
    let key = with(|p| p.key.clone());
    let given = r.q("k").as_bytes();
    if key.is_empty() || given.len() != key.len() {
        return false;
    }
    given.iter().zip(key.as_bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn handle(stream: TcpStream, addr: SocketAddr) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
    let mut reader = BufReader::with_capacity(1 << 16, stream.try_clone()?);
    let mut w = stream;
    let Some(req) = read_request(&mut reader, addr) else { return Ok(()) };
    if !allowed(&req) {
        // Drain a small body so the browser sees the 403 rather than a reset.
        if req.method == "POST" && req.content_length().unwrap_or(0) < 1 << 20 {
            let mut sink = vec![0u8; req.content_length().unwrap_or(0) as usize];
            let _ = reader.read_exact(&mut sink);
        }
        return if req.path == "/" {
            respond(&mut w, 403, "text/html; charset=utf-8", DENIED.as_bytes(), &[])
        } else {
            status_only(&mut w, 403)
        };
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => {
            let (theme, name) = (page_theme(), pc_name());
            // The theme is JSON of hex colors and a bool: safe inside the page's <script>.
            let page = PAGE.replace("{{NAME}}", &html_encode(&name)).replace("{{THEME}}", &theme);
            respond(&mut w, 200, "text/html; charset=utf-8", page.as_bytes(), &[])
        }
        ("GET", "/web/files") => {
            let list: Vec<serde_json::Value> = shelf_files()
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    serde_json::json!({
                        "id": i,
                        "name": file_name(f),
                        "size": std::fs::metadata(f).map(|m| m.len()).unwrap_or(0),
                        "image": shelf::is_image_path(f),
                    })
                })
                .collect();
            json(&mut w, &serde_json::Value::Array(list))
        }
        ("GET", "/web/received") => {
            let list: Vec<serde_json::Value> = with(|p| {
                p.transfers
                    .iter()
                    .filter(|t| t.incoming && !t.active && !t.failed)
                    .map(|t| {
                        serde_json::json!({
                            "kind": if t.text.is_some() { "text" } else { "file" },
                            "name": t.title,
                            "size": t.total,
                            "text": t.text,
                            "at": t.at,
                        })
                    })
                    .collect()
            });
            json(&mut w, &serde_json::Value::Array(list))
        }
        ("GET", "/web/file") => {
            let files = shelf_files();
            match req.q("id").parse::<usize>().ok().and_then(|i| files.get(i)) {
                Some(f) => serve_file(&mut w, &req, f, Some(&file_name(f))),
                None => status_only(&mut w, 404),
            }
        }
        ("POST", "/web/upload") => upload(&mut reader, &mut w, &req),
        ("POST", "/web/text") => text(&mut reader, &mut w, &req),
        ("GET", "/web/outbox") => {
            let list: Vec<serde_json::Value> = with(|p| {
                // Text has nothing to download: the page showing it is delivery.
                for o in p.outbox.iter_mut().filter(|o| o.kind == OutKind::Text) {
                    o.state = "Shown on phone".into();
                }
                p.outbox
                    .iter()
                    .map(|o| {
                        serde_json::json!({
                            "id": o.id,
                            "kind": match o.kind { OutKind::File => "file", OutKind::Folder => "folder", OutKind::Text => "text" },
                            "name": o.name,
                            "size": o.size,
                            "text": o.text,
                            "image": o.kind == OutKind::File && shelf::is_image_path(&o.name),
                            "at": o.sent_at,
                            "state": o.state,
                        })
                    })
                    .collect()
            });
            crate::bg::refresh();
            json(&mut w, &serde_json::Value::Array(list))
        }
        ("GET", "/web/send") => send_item(&mut w, &req),
        ("GET", "/web/send-all") => {
            let items: Vec<OutItem> = with(|p| p.outbox.iter().filter(|o| o.kind != OutKind::Text).rev().cloned().collect());
            if items.is_empty() {
                return status_only(&mut w, 404);
            }
            zip_items(&mut w, &req, &format!("From {}.zip", pc_name()), &items)
        }
        ("GET", "/web/events") => events(&mut w),
        _ => status_only(&mut w, 404),
    }
}

fn html_encode(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn file_name(p: &str) -> String {
    Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string())
}

/// Shelf files (folders are skipped); ids index this list.
fn shelf_files() -> Vec<String> {
    shelf::all().into_iter().filter(|i| Path::new(&i.path).is_file()).map(|i| i.path).collect()
}

fn content_type(name: &str) -> &'static str {
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        "txt" | "log" | "md" => "text/plain; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "json" => "application/json",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

/// RFC 6266 attachment header with an ASCII fallback and a UTF-8 filename*.
fn attachment(name: &str) -> String {
    let ascii: String = name.chars().map(|c| if c.is_ascii() && c != '"' && c != '\\' && !c.is_control() { c } else { '_' }).collect();
    let enc: String = name
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{enc}")
}

/// Serves a file, honouring a single "Range: bytes=a-b" (resumed downloads, video seeking). No tracking.
fn serve_file(w: &mut TcpStream, req: &Request, path: &str, download_name: Option<&str>) -> std::io::Result<()> {
    let Ok(mut f) = std::fs::File::open(path) else { return status_only(w, 404) };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut start = 0u64;
    let mut end = len.saturating_sub(1);
    let mut partial = false;
    if let Some(r) = req.header("range").and_then(|r| r.strip_prefix("bytes=")) {
        if let Some((a, b)) = r.split(',').next().and_then(|r| r.split_once('-')) {
            let (a, b) = (a.trim(), b.trim());
            if a.is_empty() {
                if let Ok(n) = b.parse::<u64>() {
                    start = len.saturating_sub(n);
                }
            } else if let Ok(s) = a.parse::<u64>() {
                start = s;
                if let Ok(e) = b.parse::<u64>() {
                    end = e.min(end);
                }
            }
            if start >= len || start > end {
                return respond(w, 416, "text/plain", b"", &[("Content-Range", format!("bytes */{len}"))]);
            }
            partial = true;
        }
    }
    let count = if len == 0 { 0 } else { end - start + 1 };
    let mut head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {count}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nConnection: close\r\n",
        if partial { "206 Partial Content" } else { "200 OK" },
        content_type(path)
    );
    if partial {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{len}\r\n"));
    }
    if let Some(n) = download_name {
        head.push_str(&format!("Content-Disposition: {}\r\n", attachment(n)));
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    use std::io::Seek;
    f.seek(std::io::SeekFrom::Start(start))?;
    std::io::copy(&mut f.take(count), w)?;
    w.flush()
}

/// Where a received file goes: the name is cleaned of path tricks and gets "name (2).ext" when taken.
fn target_path(root: &Path, name: &str) -> PathBuf {
    let base = name.replace('\\', "/");
    let base = base.rsplit('/').next().unwrap_or("");
    let mut file: String = base.chars().map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c }).collect();
    file = file.trim().trim_end_matches('.').to_string();
    if file.is_empty() || file == "." || file == ".." {
        file = "file".into();
    }
    if file.chars().count() > 120 {
        let ext = Path::new(&file).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        file = file.chars().take(100).collect::<String>() + &ext;
    }
    let _ = std::fs::create_dir_all(root);
    let stem = Path::new(&file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = Path::new(&file).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut path = root.join(&file);
    let mut n = 2;
    while path.exists() || part_of(&path).exists() {
        path = root.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    path
}

fn part_of(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

fn upload(reader: &mut BufReader<TcpStream>, w: &mut TcpStream, req: &Request) -> std::io::Result<()> {
    let name = req.q("name").to_string();
    if name.is_empty() {
        return status_only(w, 400);
    }
    let Some(total) = req.content_length() else { return status_only(w, 400) };
    let title = file_name(&name.replace('\\', "/"));
    let t = track(title.clone(), req.peer.clone(), true, total, "Receiving");
    let folder = receive_folder();
    let path = target_path(&folder, &name);
    let part = part_of(&path);
    let result = (|| -> std::io::Result<()> {
        let mut out = std::io::BufWriter::with_capacity(1 << 16, std::fs::File::create(&part)?);
        let mut left = total;
        let mut buf = vec![0u8; 1 << 16];
        while left > 0 {
            if t.cancel.load(Ordering::Relaxed) {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "canceled"));
            }
            let want = left.min(buf.len() as u64) as usize;
            let n = reader.read(&mut buf[..want])?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            out.write_all(&buf[..n])?;
            left -= n as u64;
            t.bytes.fetch_add(n as u64, Ordering::Relaxed);
        }
        out.flush()?;
        drop(out);
        std::fs::rename(&part, &path)
    })();
    match result {
        Ok(()) => {
            let saved = path.to_string_lossy().into_owned();
            let folder_name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            finish(t.id, &format!("Saved to {folder_name}"), false, |x| x.saved_path = Some(saved.clone()));
            shelf::add(&[saved]);
            push("received");
            notify("recv", "ArrowDown", "File received", format!("{title} · saved to {folder_name}"));
            status_only(w, 200)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            let canceled = t.cancel.load(Ordering::Relaxed);
            finish(t.id, if canceled { "Canceled" } else { "Stopped" }, true, |_| {});
            if canceled {
                let _ = status_only(w, 500);
            }
            Err(e)
        }
    }
}

fn text(reader: &mut BufReader<TcpStream>, w: &mut TcpStream, req: &Request) -> std::io::Result<()> {
    let len = req.content_length().unwrap_or(0) as usize;
    if len > MAX_TEXT {
        return status_only(w, 413);
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let text = String::from_utf8_lossy(&body).into_owned();
    if text.trim().is_empty() {
        return status_only(w, 400);
    }
    let first = text.trim().lines().next().unwrap_or("").trim();
    let title = if first.chars().count() > 60 { first.chars().take(57).collect::<String>() + "…" } else { first.to_string() };
    let t = track(title.clone(), req.peer.clone(), true, len as u64, "");
    finish(t.id, "Text · click to copy", false, |x| x.text = Some(text.clone()));
    push("received");
    notify("recv", "ArrowDown", "Text received", title.clone());
    status_only(w, 200)
}

fn send_item(w: &mut TcpStream, req: &Request) -> std::io::Result<()> {
    let id = req.q("id");
    let Some(item) = with(|p| p.outbox.iter().find(|o| o.id == id).cloned()) else { return status_only(w, 404) };
    match item.kind {
        OutKind::Text => {
            let r = respond(w, 200, "text/plain; charset=utf-8", item.text.clone().unwrap_or_default().as_bytes(), &[]);
            if r.is_ok() {
                notify("send", "ArrowUp", "Text sent to phone", item.name.clone());
            }
            r
        }
        OutKind::Folder => {
            if !item.path.as_deref().is_some_and(|p| Path::new(p).is_dir()) {
                return status_only(w, 404);
            }
            let name = format!("{}.zip", item.name);
            zip_items(w, req, &name, &[item])
        }
        OutKind::File => {
            let path = item.path.clone().unwrap_or_default();
            if !Path::new(&path).is_file() {
                return status_only(w, 404);
            }
            // A thumbnail on the page: served inline and not counted as a download.
            if req.q("preview") == "1" {
                return serve_file(w, req, &path, None);
            }
            // A resumed download: serve the range (no progress for it).
            if req.header("range").is_some() {
                return serve_file(w, req, &path, Some(&item.name));
            }
            let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nContent-Disposition: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                content_type(&path),
                attachment(&item.name)
            );
            outgoing(w, req, &item.name, len, &[item.id.clone()], |w, t| {
                w.write_all(head.as_bytes())?;
                let mut f = std::fs::File::open(&path)?;
                copy_counting(&mut f, w, t)
            })
        }
    }
}

fn copy_counting(r: &mut impl Read, w: &mut impl Write, t: &Transfer) -> std::io::Result<()> {
    let mut buf = vec![0u8; 1 << 16];
    loop {
        if t.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "canceled"));
        }
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        w.write_all(&buf[..n])?;
        t.bytes.fetch_add(n as u64, Ordering::Relaxed);
    }
}

/// Runs a download as a tracked transfer (progress, cancel from the PC); marks the items downloaded.
fn outgoing(
    w: &mut TcpStream,
    req: &Request,
    title: &str,
    total: u64,
    ids: &[String],
    body: impl FnOnce(&mut TcpStream, &Transfer) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let t = track(title.to_string(), req.peer.clone(), false, total, "Sending");
    match body(w, &t).and_then(|_| w.flush()) {
        Ok(()) => {
            finish(t.id, "Downloaded", false, |_| {});
            notify("send", "ArrowUp", "Sent to phone", title.to_string());
            with(|p| {
                for o in p.outbox.iter_mut().filter(|o| ids.contains(&o.id)) {
                    o.state = "Downloaded".into();
                }
            });
            push("outbox");
            Ok(())
        }
        Err(e) => {
            let canceled = t.cancel.load(Ordering::Relaxed);
            finish(t.id, if canceled { "Canceled" } else { "Stopped" }, true, |_| {});
            let _ = w.shutdown(std::net::Shutdown::Both); // a cut-short body must not look like a complete file
            Err(e)
        }
    }
}

// ---------------------------------------------------------------- zip (stored, streamed)

fn crc_table() -> &'static [u32; 256] {
    static T: OnceLock<[u32; 256]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    })
}

fn crc32_file(path: &Path, cancel: &AtomicBool) -> Option<u32> {
    let mut f = std::fs::File::open(path).ok()?;
    let t = crc_table();
    let mut c = 0xFFFF_FFFFu32;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    Some(c ^ 0xFFFF_FFFF)
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<(PathBuf, String)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let r = format!("{rel}/{name}");
        if p.is_dir() {
            walk(&p, &r, out);
        } else if p.is_file() {
            out.push((p, r));
        }
    }
}

fn unique_entry(names: &mut Vec<String>, name: &str) -> String {
    let mut u = name.to_string();
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut n = 2;
    while names.iter().any(|x| x.eq_ignore_ascii_case(&u)) {
        u = format!("{stem} ({n}){ext}");
        n += 1;
    }
    names.push(u.clone());
    u
}

/// Streams files and folders as one .zip (stored, no compression), with a known Content-Length.
fn zip_items(w: &mut TcpStream, req: &Request, zip_name: &str, items: &[OutItem]) -> std::io::Result<()> {
    let mut entries: Vec<(PathBuf, String)> = Vec::new();
    let mut names = Vec::new();
    for item in items {
        let Some(p) = item.path.as_deref() else { continue };
        let p = Path::new(p);
        if item.kind == OutKind::File && p.is_file() {
            entries.push((p.to_path_buf(), unique_entry(&mut names, &item.name)));
        } else if item.kind == OutKind::Folder && p.is_dir() {
            let root = unique_entry(&mut names, &item.name);
            walk(p, &root, &mut entries);
        }
    }
    // Sizes up front (stored entries ≤ 4 GB; bigger files are left out).
    let mut sized: Vec<(PathBuf, String, u64)> = entries
        .into_iter()
        .filter_map(|(p, n)| {
            let len = std::fs::metadata(&p).ok()?.len();
            (len < 0xFFFF_FFFF).then_some((p, n, len))
        })
        .collect();
    sized.truncate(65_000);
    let total_src: u64 = sized.iter().map(|e| e.2).sum();
    let central_size: u64 = sized.iter().map(|e| 46 + e.1.len() as u64).sum();
    let local_size: u64 = sized.iter().map(|e| 30 + e.1.len() as u64 + e.2).sum();
    if local_size + central_size > 0xFFFF_FFFF {
        return status_only(w, 413);
    }
    let length = local_size + central_size + 22;
    let (time, date) = dos_now();
    let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {length}\r\nContent-Disposition: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        attachment(zip_name)
    );
    outgoing(w, req, zip_name, total_src, &ids, |w, t| {
        w.write_all(head.as_bytes())?;
        let mut central = Vec::new();
        let mut offset: u32 = 0;
        for (path, name, len) in &sized {
            // CRC first (a second read), so the local header is complete and no data descriptor is needed.
            let crc = crc32_file(path, &t.cancel).ok_or(std::io::ErrorKind::Interrupted)?;
            let nb = name.as_bytes();
            let mut h = Vec::with_capacity(30 + nb.len());
            h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            h.extend_from_slice(&20u16.to_le_bytes());
            h.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
            h.extend_from_slice(&0u16.to_le_bytes()); // stored
            h.extend_from_slice(&time.to_le_bytes());
            h.extend_from_slice(&date.to_le_bytes());
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&(*len as u32).to_le_bytes());
            h.extend_from_slice(&(*len as u32).to_le_bytes());
            h.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(nb);
            w.write_all(&h)?;
            let f = std::fs::File::open(path)?;
            // Exactly `len` bytes, even if the file changed since it was measured.
            let mut limited = f.take(*len);
            let before = t.bytes.load(Ordering::Relaxed);
            copy_counting(&mut limited, w, t)?;
            let wrote = t.bytes.load(Ordering::Relaxed) - before;
            if wrote < *len {
                w.write_all(&vec![0u8; (*len - wrote) as usize])?;
            }

            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0x0800u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&time.to_le_bytes());
            central.extend_from_slice(&date.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(*len as u32).to_le_bytes());
            central.extend_from_slice(&(*len as u32).to_le_bytes());
            central.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0u8; 8]); // extra, comment, disk, internal attrs
            central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(nb);
            offset += 30 + nb.len() as u32 + *len as u32;
        }
        let n = sized.len() as u16;
        let mut end = Vec::with_capacity(22);
        end.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        end.extend_from_slice(&[0u8; 4]);
        end.extend_from_slice(&n.to_le_bytes());
        end.extend_from_slice(&n.to_le_bytes());
        end.extend_from_slice(&(central.len() as u32).to_le_bytes());
        end.extend_from_slice(&offset.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        w.write_all(&central)?;
        w.write_all(&end)
    })
}

fn dos_now() -> (u16, u16) {
    let t = super::todos::local_now_secs();
    let (y, m, d) = super::todos::civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    let time = ((s / 3600) << 11 | (s % 3600 / 60) << 5 | (s % 60) / 2) as u16;
    let date = (((y - 1980).max(0) as u32) << 9 | m << 5 | d) as u16;
    (time, date)
}

// ---------------------------------------------------------------- live updates

fn events(w: &mut TcpStream) -> std::io::Result<()> {
    let (tx, rx) = channel::<String>();
    let id = with(|p| {
        p.next_id += 1;
        p.events.push((p.next_id, tx));
        p.next_id
    });
    crate::bg::refresh();
    let result = (|| -> std::io::Result<()> {
        w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n")?;
        let mut theme = page_theme();
        let hello = serde_json::json!({ "name": pc_name() }).to_string();
        w.write_all(format!("event: hello\ndata: {hello}\n\n").as_bytes())?;
        w.write_all(format!("event: theme\ndata: {theme}\n\n").as_bytes())?;
        w.flush()?;
        let mut idle = 0;
        loop {
            // Settings → Appearance changed: recolor the page live.
            let now = page_theme();
            if now != theme {
                theme = now;
                w.write_all(format!("event: theme\ndata: {theme}\n\n").as_bytes())?;
                w.flush()?;
            }
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(name) => {
                    w.write_all(format!("event: {name}\ndata: 1\n\n").as_bytes())?;
                    w.flush()?;
                    idle = 0;
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Notice a closed page within a second (so "Phone connected" goes away promptly).
                    w.set_nonblocking(true)?;
                    let mut b = [0u8; 1];
                    let closed = matches!(w.peek(&mut b), Ok(0)) || matches!(w.peek(&mut b), Err(ref e) if e.kind() != std::io::ErrorKind::WouldBlock);
                    w.set_nonblocking(false)?;
                    if closed {
                        return Ok(());
                    }
                    idle += 1;
                    // Keeps phones and routers from dropping an idle connection.
                    if idle >= 20 {
                        idle = 0;
                        w.write_all(b": ping\n\n")?;
                        w.flush()?;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(()), // server stopping
            }
        }
    })();
    with(|p| p.events.retain(|(i, _)| *i != id));
    crate::bg::refresh();
    result
}

const DENIED: &str = "<!doctype html><html lang=en><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'>\
<meta name=color-scheme content='dark light'><title>FathNotch · Link turned off</title>\
<style>:root{--bg:#0a0a0c;--text:#f4f4f6;--sub:#8b8b93;--card:#151518;--line:#26262b}\
@media(prefers-color-scheme:light){:root{--bg:#f3f3f6;--text:#141417;--sub:#6b6b73;--card:#fff;--line:#e4e4ea}}\
body{margin:0;min-height:100vh;display:grid;place-items:center;background:var(--bg);color:var(--text);\
font:15px/1.5 system-ui,-apple-system,'Segoe UI',Roboto,sans-serif;padding:24px;box-sizing:border-box}\
div{max-width:360px;text-align:center;background:var(--card);border:1px solid var(--line);border-radius:22px;padding:32px 24px}\
i{display:block;width:12px;height:12px;border-radius:50%;background:#ff453a;margin:0 auto 18px;box-shadow:0 0 0 6px #ff453a26}\
h1{font-size:20px;margin:0 0 8px}p{margin:0;color:var(--sub)}</style>\
<div><i></i><h1>Link turned off</h1><p>Phone transfer was turned off on the PC. Turn on Phone in FathNotch and scan the new code.</p></div>";

#[cfg(test)]
mod tests {
    use super::*;

    fn get(port: i32, path: &str) -> (String, Vec<u8>) {
        let mut s = TcpStream::connect(("127.0.0.1", port as u16)).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).unwrap();
        let split = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (String::from_utf8_lossy(&all[..split]).into_owned(), all[split + 4..].to_vec())
    }

    /// End to end against a scratch data folder: FATHNOTCH_DATA_DIR and PHONE_TEST_DIR (files to offer) must be set.
    #[test]
    #[ignore]
    fn outbox_and_zip() {
        let dir = PathBuf::from(std::env::var("PHONE_TEST_DIR").unwrap());
        turn_on();
        let snap = snapshot();
        assert!(snap.running, "status: {}", snap.status);
        let key = with(|p| p.key.clone());
        let port = settings::get().phone_port;
        send(&[dir.join("Project folder").to_string_lossy().into_owned(), dir.join("notes.txt").to_string_lossy().into_owned()]);
        send_text("Text for the phone");
        let (h, body) = get(port, &format!("/web/outbox?k={key}"));
        println!("{h}\n{}", String::from_utf8_lossy(&body));
        let (h, body) = get(port, &format!("/web/send-all?k={key}"));
        println!("{h}");
        std::fs::write(dir.join("all.zip"), &body).unwrap();
        let folder_id = snapshot().outbox.iter().find(|o| o.kind == OutKind::Folder).unwrap().id.clone();
        let (h, body) = get(port, &format!("/web/send?k={key}&id={folder_id}"));
        println!("{h}");
        std::fs::write(dir.join("folder.zip"), &body).unwrap();
        let file_id = snapshot().outbox.iter().find(|o| o.kind == OutKind::File).unwrap().id.clone();
        let (h, body) = get(port, &format!("/web/send?k={key}&id={file_id}"));
        println!("{h}\n{}", String::from_utf8_lossy(&body));
        for t in snapshot().transfers {
            println!("transfer: {} | {} | {} | {}", t.title, t.direction(), t.state, t.progress());
        }
        for o in snapshot().outbox {
            println!("outbox: {} | {} | {}", o.name, o.detail(), o.state);
        }
        turn_off();
        assert!(!store::path(KEY_FILE).exists());
        let (h, _) = std::panic::catch_unwind(|| get(port, "/")).unwrap_or_default();
        println!("after off: {}", h.lines().next().unwrap_or("(refused)"));
    }
}
