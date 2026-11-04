//! Owner-controlled HTTPS/WSS hosting. Configuration is private; remote clients cannot
//! change server settings or retrieve its pairing token. Each browser owns its own bridge.
#![allow(clippy::disallowed_methods)] // This entire module is native-only; std monotonic clocks never run on WASM.

use std::io::{Cursor, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gridcraft_mcp::control_auth;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tungstenite::{Message, protocol::WebSocketConfig};

const MAX_CONFIG: u64 = 16 * 1024;
const MAX_PEM: u64 = 1024 * 1024;
const MAX_HEADER: usize = 16 * 1024;
const MAX_CLIENTS: usize = 16;
const MAX_MESSAGE: usize = 4 * 1024 * 1024;
static ACTIVE_CLIENTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub listen: String,
    #[serde(deserialize_with = "deserialize_origin")]
    pub origin: String,
    pub static_dir: PathBuf,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    #[serde(default)]
    pub autostart: bool,
    token: String,
}

fn deserialize_origin<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    use serde::de::Error as _;
    let origin = String::deserialize(deserializer)?;
    let uri: tungstenite::http::Uri = origin.parse().map_err(|_| D::Error::custom("Invalid HTTPS origin"))?;
    if uri.scheme_str() != Some("https") || uri.path() != "/" || uri.query().is_some() {
        return Err(D::Error::custom("Expected an HTTPS origin without path or query"));
    }
    let authority = uri.authority().ok_or_else(|| D::Error::custom("HTTPS host missing"))?.as_str().to_ascii_lowercase();
    if authority.contains('@') {
        return Err(D::Error::custom("Credentials are forbidden in the origin"));
    }
    let authority = if uri.port_u16() == Some(443) { authority.strip_suffix(":443").unwrap_or(&authority) } else { &authority };
    Ok(format!("https://{authority}"))
}

impl Config {
    fn validate(&self) -> Result<(), String> {
        let address = self.listen.parse::<SocketAddr>().map_err(|_| "Listen must be an IP address and port.")?;
        if address.port() == 0 {
            return Err("HTTPS port must be from 1 to 65535.".into());
        }
        let uri: tungstenite::http::Uri = self.origin.parse().map_err(|_| "Invalid HTTPS origin.")?;
        if uri.scheme_str() != Some("https")
            || uri.authority().is_none()
            || uri.path() != "/"
            || uri.query().is_some()
            || uri.authority().is_some_and(|a| a.as_str().contains('@'))
            || uri.authority().is_some_and(|a| self.origin != format!("https://{}", a.as_str().to_ascii_lowercase()))
        {
            return Err("Origin must be https://hostname[:port], without credentials, path or query.".into());
        }
        if self.token.len() < control_auth::MIN_TOKEN_LEN || self.token.len() > 4096 || self.token.chars().any(char::is_control) {
            return Err("Invalid remote access token.".into());
        }
        if !self.static_dir.is_absolute() || !self.certificate.is_absolute() || !self.private_key.is_absolute() {
            return Err("Web directory and certificate paths must be absolute.".into());
        }
        if !self.static_dir.join("index.html").is_file() {
            return Err("Web directory does not contain index.html. Build the WASM distribution first.".into());
        }
        Ok(())
    }

    fn public(&self) -> Value {
        json!({"listen":self.listen, "origin":self.origin, "staticDir":self.static_dir,
            "certificate":self.certificate, "privateKey":self.private_key, "autostart":self.autostart,
            "wsUrl":format!("{}/agent/ws", self.origin.replacen("https://", "wss://", 1))})
    }
}

fn config_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("GRIDCRAFT_REMOTE_CONFIG_PATH") {
        return Ok(path.into());
    }
    let dir = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|v| PathBuf::from(v).join("Library/Application Support/GridCraft"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|v| PathBuf::from(v).join("GridCraft"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|v| PathBuf::from(v).join(".config")))
            .map(|v| v.join("gridcraft"))
    };
    dir.map(|v| v.join("remote.json")).ok_or_else(|| "Remote settings directory is unavailable.".into())
}

fn private_file(path: &Path, limit: u64, private: bool) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "Cannot read server configuration or certificate file.")?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err("Server configuration and certificates must be bounded regular files (no symlinks).".into());
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Private settings and TLS key must have mode 0600.".into());
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "Cannot open server file.")?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read server file.")?;
    if bytes.len() as u64 > limit {
        return Err("Server file is too large.".into());
    }
    Ok(bytes)
}

fn load(path: &Path) -> Result<Config, String> {
    serde_json::from_slice(&private_file(path, MAX_CONFIG, true)?).map_err(|_| "Invalid remote settings file.".into())
}

fn tls(config: &Config) -> Result<Arc<ServerConfig>, String> {
    let certs = private_file(&config.certificate, MAX_PEM, false)?;
    let key = private_file(&config.private_key, MAX_PEM, true)?;
    let certs = rustls_pemfile::certs(&mut Cursor::new(certs)).collect::<Result<Vec<_>, _>>().map_err(|_| "Invalid certificate PEM.")?;
    let key = rustls_pemfile::private_key(&mut Cursor::new(key)).map_err(|_| "Invalid key PEM.")?.ok_or("No TLS private key found.")?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| "Cannot configure TLS.")?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|_| "Certificate and key are invalid or do not match.")?;
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(tls))
}

struct Running {
    stop: Arc<AtomicBool>,
    clients: Arc<Mutex<Vec<(u64, TcpStream)>>>,
    worker: std::thread::JoinHandle<()>,
    config: Config,
    error: Arc<Mutex<String>>,
}

#[derive(Default)]
struct Manager {
    running: Option<Running>,
}
static MANAGER: OnceLock<Mutex<Manager>> = OnceLock::new();

fn manager() -> std::sync::MutexGuard<'static, Manager> {
    MANAGER.get_or_init(Mutex::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Manager {
    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop.store(true, Ordering::Relaxed);
            for (_, client) in running.clients.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter() {
                let _ = client.shutdown(Shutdown::Both);
            }
            let _ = running.worker.join();
        }
    }

    fn start(&mut self, config: Config) -> Result<(), String> {
        config.validate()?;
        tls(&config)?;
        if self.running.as_ref().is_some_and(|r| !r.worker.is_finished()) {
            return Err("Remote server is already running. Stop it before changing its settings.".into());
        }
        self.stop();
        let listener = TcpListener::bind(&config.listen).map_err(|e| format!("Cannot start HTTPS listener: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let clients = Arc::new(Mutex::new(Vec::new()));
        let error = Arc::new(Mutex::new(String::new()));
        let runtime = Runtime { stop: stop.clone(), clients: clients.clone(), config: config.clone(), error: error.clone() };
        let worker = std::thread::Builder::new().name("gridcraft-https".into()).spawn(move || runtime.serve(listener)).map_err(|e| e.to_string())?;
        self.running = Some(Running { stop, clients, worker, config, error });
        Ok(())
    }

    fn status(&self, saved: Option<&Config>) -> Value {
        let config = self.running.as_ref().map(|r| &r.config).or(saved);
        let mut value = config.map(Config::public).unwrap_or_else(|| json!({}));
        value["remoteServer"] = json!(true);
        value["running"] = json!(self.running.as_ref().is_some_and(|r| !r.worker.is_finished()));
        value["clients"] =
            json!(self.running.as_ref().map(|r| r.clients.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()).unwrap_or(0));
        value["serverError"] =
            json!(self.running.as_ref().map(|r| r.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()).unwrap_or_default());
        value
    }
}

/// Called only from the private desktop/loopback bridge, never from the public endpoint.
pub fn handle_local(method: &str, params: &Value) -> Result<Value, String> {
    let path = config_path()?;
    let mut manager = manager();
    handle_at(&mut manager, &path, method, params)
}

fn handle_at(manager: &mut Manager, path: &Path, method: &str, params: &Value) -> Result<Value, String> {
    match method {
        "server.status" => {
            let saved = if path.exists() { Some(load(path)?) } else { None };
            Ok(manager.status(saved.as_ref()))
        }
        "server.configure" => {
            if manager.running.is_some() {
                return Err("Stop the remote server before saving settings.".into());
            }
            let mut value = Value::Object(params.as_object().cloned().ok_or("Remote settings must be an object.")?);
            // Tokens are generated locally; a browser cannot choose a weak credential.
            let old = if path.exists() { Some(load(path)?) } else { None };
            let token = match old {
                Some(config) => config.token,
                None => new_token()?,
            };
            value["token"] = json!(token);
            let config: Config = serde_json::from_value(value).map_err(|_| "Invalid remote settings.")?;
            config.validate()?;
            tls(&config)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|_| "Cannot create settings directory.")?;
            }
            let bytes = serde_json::to_vec_pretty(&config).map_err(|_| "Cannot serialize remote settings.")?;
            gridcraft_engine::io::write_file(path.to_str().ok_or("Settings path must be UTF-8.")?, &bytes)
                .map_err(|_| "Cannot save private remote settings.")?;
            Ok(manager.status(Some(&config)))
        }
        "server.start" => {
            let config = load(path)?;
            manager.start(config)?;
            Ok(manager.status(None))
        }
        "server.stop" => {
            manager.stop();
            Ok(manager.status(load(path).ok().as_ref()))
        }
        "server.pair" => {
            let config = load(path)?;
            let mut value = config.public();
            value["pairingToken"] = json!(config.token);
            Ok(value)
        }
        "server.rotateToken" => {
            let mut config = load(path)?;
            let was_running = manager.running.is_some();
            manager.stop();
            config.token = new_token()?;
            let bytes = serde_json::to_vec_pretty(&config).map_err(|_| "Cannot serialize remote settings.")?;
            gridcraft_engine::io::write_file(path.to_str().ok_or("Settings path must be UTF-8.")?, &bytes)
                .map_err(|_| "Cannot rotate private token.")?;
            if was_running {
                manager.start(config.clone())?;
            }
            Ok(manager.status(Some(&config)))
        }
        _ => Err("Unknown server method.".into()),
    }
}

fn new_token() -> Result<String, String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| "Cannot generate remote pairing token.")?;
    Ok(control_auth::hex(&bytes))
}

pub fn autostart() -> Result<(), String> {
    let path = config_path()?;
    if path.exists() {
        let config = load(&path)?;
        if config.autostart {
            manager().start(config)?;
        }
    }
    Ok(())
}

/// CLI service mode; the saved configuration is also used by the desktop panel.
pub fn main(path: &Path) -> Result<(), String> {
    manager().start(load(path)?)?;
    eprintln!("gridcraft-cli: built-in HTTPS/WSS server started");
    loop {
        std::thread::sleep(Duration::from_millis(250));
        if manager().running.as_ref().is_none_or(|r| r.worker.is_finished()) {
            return Err("HTTPS listener stopped.".into());
        }
    }
}

struct Runtime {
    stop: Arc<AtomicBool>,
    clients: Arc<Mutex<Vec<(u64, TcpStream)>>>,
    config: Config,
    error: Arc<Mutex<String>>,
}

impl Runtime {
    fn serve(self, listener: TcpListener) {
        let mut id = 0u64;
        while !self.stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let mut clients = self.clients.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if clients.len() >= MAX_CLIENTS || self.stop.load(Ordering::Relaxed) {
                        continue;
                    }
                    let Ok(clone) = stream.try_clone() else {
                        continue;
                    };
                    if ACTIVE_CLIENTS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| (n < MAX_CLIENTS).then_some(n + 1)).is_err() {
                        continue;
                    }
                    id = id.wrapping_add(1);
                    clients.push((id, clone));
                    drop(clients);
                    let guard = ClientGuard { id, clients: self.clients.clone() };
                    let config = self.config.clone();
                    let error = self.error.clone();
                    let _ = std::thread::Builder::new().name("gridcraft-wss-client".into()).spawn(move || {
                        let _guard = guard;
                        // Reload on each handshake, allowing certificate renewal without a restart.
                        let tls = match tls(&config) {
                            Ok(tls) => {
                                error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
                                tls
                            }
                            Err(e) => {
                                *error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = e;
                                return;
                            }
                        };
                        // Peer errors and request data must not be written to the server log.
                        let _ = serve_client(stream, &config, tls);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(25)),
                Err(_) => {
                    *self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = "HTTPS listener failed.".into();
                    break;
                }
            }
        }
    }
}

struct ClientGuard {
    id: u64,
    clients: Arc<Mutex<Vec<(u64, TcpStream)>>>,
}
impl Drop for ClientGuard {
    fn drop(&mut self) {
        ACTIVE_CLIENTS.fetch_sub(1, Ordering::Relaxed);
        self.clients.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|(id, _)| *id != self.id);
    }
}

struct Replay<S> {
    prefix: Cursor<Vec<u8>>,
    stream: S,
}
impl<S: Read> Read for Replay<S> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.prefix.position() < self.prefix.get_ref().len() as u64 { self.prefix.read(out) } else { self.stream.read(out) }
    }
}
impl<S: Write> Write for Replay<S> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

fn serve_client(stream: TcpStream, config: &Config, tls: Arc<ServerConfig>) -> Result<(), String> {
    stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    let socket = stream.try_clone().map_err(|e| e.to_string())?;
    let mut stream = StreamOwned::new(ServerConnection::new(tls).map_err(|e| e.to_string())?, stream);
    let mut header = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= MAX_HEADER || Instant::now() >= deadline {
            return Err("HTTP header limit exceeded.".into());
        }
        let mut byte = [0];
        stream.read_exact(&mut byte).map_err(|_| "HTTP handshake failed.")?;
        header.push(byte[0]);
    }
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut request = httparse::Request::new(&mut headers);
    request.parse(&header).map_err(|_| "Invalid HTTP request.")?;
    let uri: tungstenite::http::Uri = config.origin.parse().map_err(|_| "Invalid origin.")?;
    let host = request.headers.iter().find(|h| h.name.eq_ignore_ascii_case("host")).map(|h| h.value);
    if host != uri.authority().map(|a| a.as_str().as_bytes()) || request.method != Some("GET") {
        return http_error(&mut stream, "403 Forbidden");
    }
    let path = request.path.ok_or("HTTP path missing.")?;
    if path.split('?').next() != Some("/agent/ws") {
        return serve_static(&mut stream, &config.static_dir, path);
    }
    let origin = config.origin.clone();
    #[allow(clippy::result_large_err)] // tungstenite callback requires its concrete HTTP response error.
    let callback = move |request: &tungstenite::handshake::server::Request, response: tungstenite::handshake::server::Response| {
        if request.headers().get("Origin").and_then(|h| h.to_str().ok()) != Some(origin.as_str()) {
            let mut denied = tungstenite::http::Response::new(Some("Origin rejected".to_string()));
            *denied.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
            return Err(denied);
        }
        Ok(response)
    };
    let options = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE));
    let mut ws = tungstenite::accept_hdr_with_config(Replay { prefix: Cursor::new(header), stream }, callback, Some(options))
        .map_err(|_| "WebSocket handshake failed.")?;
    crate::web_control::authenticate_ws(&mut ws, &config.token)?;
    socket.set_read_timeout(Some(Duration::from_millis(100))).map_err(|e| e.to_string())?;
    let mut received = Instant::now();
    let mut ping = Instant::now();
    loop {
        match ws.read() {
            Ok(Message::Text(text)) => {
                let request: Value = serde_json::from_str(&text).map_err(|_| "Invalid agent JSON.")?;
                socket.set_read_timeout(Some(Duration::from_secs(110))).map_err(|e| e.to_string())?;
                // Remote clients have agent-only access. No server management or arbitrary
                // engine RPC, and no implicit routing to another browser's workbook.
                if !crate::web_agent::handle(&mut ws, &request)? {
                    ws.send(Message::text(
                        json!({"id":request.get("id"),"error":{"code":-32601,"message":"Unknown remote agent method"}}).to_string(),
                    ))
                    .map_err(|e| e.to_string())?;
                }
                socket.set_read_timeout(Some(Duration::from_millis(100))).map_err(|e| e.to_string())?;
                received = Instant::now();
                ping = Instant::now();
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {
                received = Instant::now();
            }
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return Err("Agent connection closed.".into()),
        }
        if received.elapsed() > Duration::from_secs(60) {
            return Err("Agent heartbeat timed out.".into());
        }
        if ping.elapsed() > Duration::from_secs(20) {
            ws.send(Message::Ping(Vec::new().into())).map_err(|_| "Heartbeat failed.")?;
            ping = Instant::now();
        }
    }
}

fn http_error(out: &mut impl Write, status: &str) -> Result<(), String> {
    write!(out, "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n").map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

fn asset(root: &Path, request: &str) -> Option<(PathBuf, &'static str)> {
    let path = request.split('?').next()?.strip_prefix('/')?;
    let path = if path.is_empty() { "index.html" } else { path };
    if path.contains('%')
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path.split('/').any(|part| part.starts_with('.') || part.is_empty())
        || Path::new(path).components().any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let mime = match Path::new(path).extension()?.to_str()? {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => return None,
    };
    let root = root.canonicalize().ok()?;
    let full = root.join(path).canonicalize().ok()?;
    if !full.starts_with(root) || !full.is_file() {
        return None;
    }
    Some((full, mime))
}

fn serve_static(out: &mut impl Write, root: &Path, path: &str) -> Result<(), String> {
    if path == "/health" {
        write!(out, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{{\"ok\":true}}").map_err(|e| e.to_string())?;
        return out.flush().map_err(|e| e.to_string());
    }
    let Some((path, mime)) = asset(root, path) else {
        return http_error(out, "404 Not Found");
    };
    let file = std::fs::File::open(path).map_err(|_| "Cannot open web asset.")?;
    let size = file.metadata().map_err(|_| "Cannot read web asset metadata.")?.len();
    if size > 128 * 1024 * 1024 {
        return http_error(out, "413 Content Too Large");
    }
    write!(out, "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {size}\r\nConnection: close\r\nCache-Control: no-cache\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nStrict-Transport-Security: max-age=86400\r\n\r\n").map_err(|e| e.to_string())?;
    std::io::copy(&mut file.take(size), out).map_err(|_| "Web asset transfer failed.")?;
    out.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use rustls::{ClientConfig, ClientConnection, RootCertStore};
    use tungstenite::client::IntoClientRequest;

    struct Fixture {
        dir: PathBuf,
        config: Config,
        cert: rustls::pki_types::CertificateDer<'static>,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!("gridcraft-wss-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("index.html"), "<html>GridCraft test</html>").unwrap();
            std::fs::write(dir.join("test.wasm"), [0, 97, 115, 109]).unwrap();
            let certificate = dir.join("cert.pem");
            let private_key = dir.join("key.pem");
            let cert = write_certificate(&certificate, &private_key);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let config = Config {
                listen: address.to_string(),
                origin: format!("https://localhost:{}", address.port()),
                static_dir: dir.clone(),
                certificate,
                private_key,
                autostart: false,
                token: "a".repeat(64),
            };
            Self { dir, config, cert }
        }
        fn client(&self) -> StreamOwned<ClientConnection, TcpStream> {
            client(&self.config, self.cert.clone())
        }
        fn websocket(&self, origin: &str) -> Result<tungstenite::WebSocket<StreamOwned<ClientConnection, TcpStream>>, String> {
            let mut request = format!("{}/agent/ws", self.config.origin.replacen("https://", "wss://", 1)).into_client_request().unwrap();
            request.headers_mut().insert("Origin", origin.parse().unwrap());
            tungstenite::client(request, self.client()).map(|(ws, _)| ws).map_err(|e| e.to_string())
        }
        fn authenticated(&self) -> tungstenite::WebSocket<StreamOwned<ClientConnection, TcpStream>> {
            let mut ws = self.websocket(&self.config.origin).unwrap();
            ws.send(Message::text(control_auth::auth_line(0, &self.config.token))).unwrap();
            assert_eq!(read_json(&mut ws)["ok"], true);
            ws
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    impl Drop for Manager {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn write_certificate(cert: &Path, key: &Path) -> rustls::pki_types::CertificateDer<'static> {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        std::fs::write(cert, generated.cert.pem()).unwrap();
        std::fs::write(key, generated.signing_key.serialize_pem()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        generated.cert.der().clone()
    }
    fn client(config: &Config, cert: rustls::pki_types::CertificateDer<'static>) -> StreamOwned<ClientConnection, TcpStream> {
        let mut roots = RootCertStore::empty();
        roots.add(cert).unwrap();
        let tls = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let socket = TcpStream::connect(&config.listen).unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let conn = ClientConnection::new(Arc::new(tls), rustls::pki_types::ServerName::try_from("localhost").unwrap()).unwrap();
        StreamOwned::new(conn, socket)
    }
    fn read_json<S: Read + Write>(ws: &mut tungstenite::WebSocket<S>) -> Value {
        let Message::Text(text) = ws.read().unwrap() else { panic!("expected JSON text") };
        serde_json::from_str(&text).unwrap()
    }
    fn get(f: &Fixture, path: &str) -> String {
        let mut stream = f.client();
        let uri: tungstenite::http::Uri = f.config.origin.parse().unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: {}\r\n\r\n", uri.authority().unwrap()).unwrap();
        stream.flush().unwrap();
        let mut bytes = Vec::new();
        // A server closing the underlying TLS connection after a complete Content-Length
        // response may produce UnexpectedEof on the client's next read.
        let _ = stream.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[test]
    fn https_serves_assets_and_refuses_paths_and_untrusted_origin() {
        let f = Fixture::new();
        let mut server = Manager::default();
        server.start(f.config.clone()).unwrap();
        assert!(get(&f, "/").contains("GridCraft test"));
        assert!(get(&f, "/test.wasm").contains("Content-Type: application/wasm"));
        assert!(get(&f, "/health").ends_with("{\"ok\":true}"));
        for path in ["/../index.html", "/%2e%2e/index.html", "/key.pem", "/remote.json", "/.env", "/a\\index.html"] {
            assert!(get(&f, path).starts_with("HTTP/1.1 404"), "{path}");
        }
        assert!(f.websocket("https://hostile.example").is_err());
        assert!(!server.status(None).to_string().contains(&f.config.token));
    }

    #[test]
    fn wss_authentication_and_remote_management_are_restricted() {
        let f = Fixture::new();
        let mut server = Manager::default();
        server.start(f.config.clone()).unwrap();
        let mut wrong = f.websocket(&f.config.origin).unwrap();
        wrong.send(Message::text(control_auth::auth_line(0, "wrong"))).unwrap();
        assert_eq!(read_json(&mut wrong)["ok"], false);
        let mut ws = f.authenticated();
        for method in ["server.configure", "server.pair", "engine.execute"] {
            ws.send(Message::text(json!({"id":"agent-test","method":method,"params":{}}).to_string())).unwrap();
            assert_eq!(read_json(&mut ws)["error"]["code"], -32601);
        }
        ws.send(Message::text(json!({"id":"agent-safe","method":"agent.apply","params":{}}).to_string())).unwrap();
        assert_eq!(read_json(&mut ws)["id"], "agent-safe");
        server.stop();
        assert!(ws.read().is_err());
        server.start(f.config.clone()).unwrap();
        let _ = f.authenticated();
    }

    #[test]
    fn simultaneous_browsers_have_independent_workbook_channels() {
        let f = Fixture::new();
        let mut server = Manager::default();
        server.start(f.config.clone()).unwrap();
        let mut first = f.authenticated();
        let mut second = f.authenticated();
        for (ws, label) in [(&mut first, "one"), (&mut second, "two")] {
            ws.send(Message::text(json!({"id":format!("agent-{label}"),"method":"agent.undo","params":{}}).to_string())).unwrap();
        }
        for (ws, label) in [(&mut second, "two"), (&mut first, "one")] {
            let request = read_json(ws);
            assert_eq!(request["method"], "agent.undoReviewed");
            ws.send(Message::text(json!({"id":request["id"],"ok":true,"result":{"workbook":label}}).to_string())).unwrap();
            let reply = read_json(ws);
            assert_eq!(reply["id"], format!("agent-{label}"));
            assert_eq!(reply["result"]["workbook"], label);
        }
    }

    #[test]
    fn renewed_certificates_are_loaded_without_interrupting_existing_clients() {
        let f = Fixture::new();
        let mut server = Manager::default();
        server.start(f.config.clone()).unwrap();
        let mut existing = f.authenticated();
        let renewed = write_certificate(&f.config.certificate, &f.config.private_key);
        let mut next = client(&f.config, renewed);
        let uri: tungstenite::http::Uri = f.config.origin.parse().unwrap();
        write!(next, "GET /health HTTP/1.1\r\nHost: {}\r\n\r\n", uri.authority().unwrap()).unwrap();
        next.flush().unwrap();
        let mut result = Vec::new();
        let _ = next.read_to_end(&mut result);
        assert!(String::from_utf8_lossy(&result).contains("200 OK"));
        existing.send(Message::text(json!({"id":"agent-existing","method":"agent.apply","params":{}}).to_string())).unwrap();
        assert_eq!(read_json(&mut existing)["id"], "agent-existing");
        let mut old_trust = f.client();
        assert!(old_trust.conn.complete_io(&mut old_trust.sock).is_err());
    }

    #[test]
    fn configuration_rejects_downgrade_mismatched_keys_and_unsafe_files() {
        let mut f = Fixture::new();
        let mut input = serde_json::to_value(&f.config).unwrap();
        input["origin"] = json!("https://LOCALHOST:443/");
        let canonical: Config = serde_json::from_value(input).unwrap();
        assert_eq!(canonical.origin, "https://localhost");
        assert!(canonical.validate().is_ok());
        f.config.origin = "http://localhost".into();
        assert!(f.config.validate().is_err());
        f.config.origin = "https://user@localhost".into();
        assert!(f.config.validate().is_err());
        f.config.origin = "https://localhost/path".into();
        assert!(f.config.validate().is_err());
        f.config.origin = "https://localhost".into();
        let other = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        std::fs::write(&f.config.private_key, other.signing_key.serialize_pem()).unwrap();
        assert!(tls(&f.config).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::{fs::PermissionsExt, fs::symlink};
            let link = f.dir.join("link.pem");
            symlink(&f.config.private_key, &link).unwrap();
            assert!(private_file(&link, MAX_PEM, true).is_err());
            std::fs::set_permissions(&f.config.private_key, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(private_file(&f.config.private_key, MAX_PEM, true).is_err());
            let outside = f.dir.with_extension("html");
            std::fs::write(&outside, "secret").unwrap();
            symlink(&outside, f.dir.join("leak.html")).unwrap();
            assert!(asset(&f.dir, "/leak.html").is_none());
            std::fs::remove_file(outside).unwrap();
        }
    }

    #[test]
    fn local_setup_generates_private_token_and_rotation_revokes_old_clients() {
        let mut f = Fixture::new();
        let path = f.dir.join("remote.json");
        let mut server = Manager::default();
        for params in [Value::Null, json!("bad"), json!([]), json!(123)] {
            assert!(handle_at(&mut server, &path, "server.configure", &params).is_err());
            assert!(!path.exists());
        }
        let mut params = f.config.public();
        params.as_object_mut().unwrap().remove("wsUrl");
        let reply = handle_at(&mut server, &path, "server.configure", &params).unwrap();
        assert!(reply.get("token").is_none());
        f.config = load(&path).unwrap();
        assert_eq!(f.config.token.len(), 64);
        assert_ne!(f.config.token, "a".repeat(64));
        handle_at(&mut server, &path, "server.start", &json!({})).unwrap();
        let mut old_client = f.authenticated();
        let old_token = f.config.token.clone();
        let reply = handle_at(&mut server, &path, "server.rotateToken", &json!({})).unwrap();
        assert_eq!(reply["running"], true);
        assert!(!reply.to_string().contains(&old_token));
        assert!(old_client.read().is_err());
        f.config = load(&path).unwrap();
        assert_ne!(f.config.token, old_token);
        let mut denied = f.websocket(&f.config.origin).unwrap();
        denied.send(Message::text(control_auth::auth_line(0, &old_token))).unwrap();
        assert_eq!(read_json(&mut denied)["ok"], false);
        let _ = f.authenticated();
        assert_eq!(handle_at(&mut server, &path, "server.pair", &json!({})).unwrap()["pairingToken"], f.config.token);
    }
}
