//! Native UI bridge to the embedded agent backend or an explicitly selected external server.
//! The shared native backend stores provider keys privately and keeps them out of UI state.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use gridcraft_mcp::control_auth;
use gridcraft_ui_egui::{ControlRequest, Services};
use serde_json::{Value as Json, json};
use tungstenite::{Message, WebSocket};

const MAX_QUEUED_REQUESTS: usize = 8;
const QUEUED_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const UI_REPLY_TIMEOUT: Duration = Duration::from_secs(110);
type InboxJson = std::sync::Arc<std::sync::Mutex<Vec<Json>>>;

pub struct AgentBridge {
    outgoing: SyncSender<Json>,
    pub inbox: InboxJson,
    pub control_rx: Receiver<ControlRequest>,
    ctx: egui::Context,
}

impl AgentBridge {
    pub fn start_default(ctx: egui::Context) -> Result<Self, String> {
        if std::env::var_os("GRIDCRAFT_AGENT_USE_CONTROL_SERVER").is_some() {
            return Self::start_on(configured_token()?, SocketAddr::from(([127, 0, 0, 1], 8780)), ctx).map_err(|e| e.to_string());
        }
        // No external daemon or CLI binary is needed for the normal desktop configuration.
        let ws = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
        let tcp = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
        let address = ws.local_addr().map_err(|e| e.to_string())?;
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| "Cannot generate the private agent session token.")?;
        let token = control_auth::hex(&bytes);
        let server_token = token.clone();
        thread::Builder::new()
            .name("gridcraft-agent-backend".into())
            .spawn(move || {
                if let Err(error) = gridcraft_agent::web_control::serve(ws, tcp, server_token) {
                    log::warn!("local agent backend stopped: {error}");
                }
            })
            .map_err(|e| e.to_string())?;
        Self::start_on(token, address, ctx).map_err(|e| e.to_string())
    }

    fn start_on(token: String, address: SocketAddr, ctx: egui::Context) -> io::Result<Self> {
        let (outgoing, requests) = sync_channel(MAX_QUEUED_REQUESTS);
        let inbox = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (control_tx, control_rx) = std::sync::mpsc::channel();
        let worker_inbox = std::sync::Arc::clone(&inbox);
        let worker_ctx = ctx.clone();
        thread::Builder::new()
            .name("gridcraft-agent-control".into())
            .spawn(move || worker(requests, control_tx, worker_inbox, worker_ctx, token, address))?;
        Ok(Self { outgoing, inbox, control_rx, ctx })
    }

    pub fn install(self, services: &mut Services) -> Receiver<ControlRequest> {
        let outgoing = self.outgoing;
        let inbox = std::sync::Arc::clone(&self.inbox);
        let ctx = self.ctx.clone();
        services.agent_send = Some(Box::new(move |request| match outgoing.try_send(request) {
            Ok(()) => {}
            Err(std::sync::mpsc::TrySendError::Full(request)) => {
                notify_error(&inbox, &ctx, request.get("id").cloned(), "Agent queue is full. Wait for the current request to finish.".into());
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(request)) => {
                notify_error(&inbox, &ctx, request.get("id").cloned(), "Native agent connection stopped.".into());
            }
        }));
        services.agent_inbox = Some(self.inbox);
        self.control_rx
    }
}

/// Read only the bridge token for the optional external-server mode.
pub fn configured_token() -> Result<String, String> {
    if let Ok(token) = std::env::var("GRIDCRAFT_CONTROL_TOKEN") {
        return validate_token(token);
    }
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config")))
        .ok_or_else(|| "No user config directory is available for the agent token.".to_string())?;
    let path = config_dir.join("gridcraft/web-control.env");
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|_| "Agent token not found. Start gridcraft-web-control or set GRIDCRAFT_CONTROL_TOKEN.".to_string())?;
    if !metadata.file_type().is_file() {
        return Err("The web-control environment file must be a regular file.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("The web-control environment file must be private (mode 0600).".into());
        }
    }
    let contents = std::fs::read_to_string(path).map_err(|_| "Cannot read the private web-control environment file.".to_string())?;
    for line in contents.lines() {
        if let Some(value) = assignment_value(line, "GRIDCRAFT_CONTROL_TOKEN") {
            return validate_token(value);
        }
    }
    Err("GRIDCRAFT_CONTROL_TOKEN is missing from web-control.env.".into())
}

fn assignment_value(line: &str, name: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, raw) = line.split_once('=')?;
    if key.trim() != name {
        return None;
    }
    let raw = raw.trim();
    let value = if raw.len() >= 2 && ((raw.starts_with('"') && raw.ends_with('"')) || (raw.starts_with('\'') && raw.ends_with('\''))) {
        &raw[1..raw.len() - 1]
    } else {
        raw
    };
    Some(value.to_string())
}

fn validate_token(token: String) -> Result<String, String> {
    if token.len() < control_auth::MIN_TOKEN_LEN || token.chars().any(char::is_control) {
        return Err("GRIDCRAFT_CONTROL_TOKEN is invalid or too short.".into());
    }
    Ok(token)
}

#[allow(clippy::disallowed_methods)] // Native desktop bridge; gridcraft-web never runs it.
fn worker(
    requests: Receiver<Json>,
    control_tx: std::sync::mpsc::Sender<ControlRequest>,
    inbox: InboxJson,
    ctx: egui::Context,
    token: String,
    address: SocketAddr,
) {
    let mut queued: VecDeque<(Json, Instant)> = VecDeque::new();
    loop {
        drain_requests(&requests, &mut queued, &inbox, &ctx);
        match connect(&token, address) {
            Ok(mut socket) => {
                log::info!("native AI agent connected to local web-control");
                serve_connection(&mut socket, &requests, &control_tx, &mut queued, &inbox, &ctx);
            }
            Err(error) => {
                expire_queued(&mut queued, &inbox, &ctx);
                log::debug!("native AI agent waiting for local web-control: {error}");
                match requests.recv_timeout(Duration::from_millis(750)) {
                    Ok(request) => queued.push_back((request, Instant::now())),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    }
}

fn connect(token: &str, address: SocketAddr) -> Result<WebSocket<TcpStream>, String> {
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    let (mut socket, _) = tungstenite::client(format!("ws://{address}"), stream).map_err(|e| e.to_string())?;
    socket.send(Message::Text(control_auth::auth_line(0, token).into())).map_err(|e| e.to_string())?;
    let response = socket.read().map_err(|e| e.to_string())?;
    let Message::Text(response) = response else {
        return Err("web-control did not return an authentication response".into());
    };
    let response: Json = serde_json::from_str(&response).map_err(|e| e.to_string())?;
    if response.get("ok") != Some(&Json::Bool(true)) {
        return Err("web-control rejected the native app token".into());
    }
    socket.get_ref().set_read_timeout(Some(Duration::from_millis(100))).map_err(|e| e.to_string())?;
    socket.get_ref().set_write_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    Ok(socket)
}

#[allow(clippy::disallowed_methods)] // Native desktop bridge; gridcraft-web never runs it.
fn serve_connection(
    socket: &mut WebSocket<TcpStream>,
    requests: &Receiver<Json>,
    control_tx: &std::sync::mpsc::Sender<ControlRequest>,
    queued: &mut VecDeque<(Json, Instant)>,
    inbox: &InboxJson,
    ctx: &egui::Context,
) {
    let mut pending = HashSet::new();
    loop {
        drain_requests(requests, queued, inbox, ctx);
        expire_queued(queued, inbox, ctx);
        while let Some((request, _)) = queued.pop_front() {
            let id = request.get("id").cloned();
            if let Err(error) = socket.send(Message::Text(request.to_string().into())) {
                notify_error(inbox, ctx, id.clone(), format!("Agent request could not be sent: {error}"));
                notify_pending(
                    inbox,
                    ctx,
                    &mut pending,
                    "The local agent connection ended; request status is unknown. Inspect the workbook before retrying.",
                );
                return;
            }
            if let Some(id) = id.and_then(|id| id.as_str().map(str::to_string)) {
                pending.insert(id);
            }
        }
        match socket.read() {
            Ok(Message::Text(text)) => match serde_json::from_str::<Json>(&text) {
                Ok(value) if is_workbook_request(&value) => {
                    if let Err(error) = forward_workbook_request(socket, control_tx, ctx, value) {
                        log::warn!("native agent workbook request failed: {error}");
                    }
                }
                Ok(value) => {
                    if let Some(id) = value.get("id").and_then(Json::as_str).filter(|id| id.starts_with("agent-")) {
                        pending.remove(id);
                        push_inbox(inbox, value, ctx);
                    }
                }
                Err(error) => log::warn!("invalid native agent response: {error}"),
            },
            Ok(Message::Ping(data)) => {
                if socket.send(Message::Pong(data)).is_err() {
                    notify_pending(
                        inbox,
                        ctx,
                        &mut pending,
                        "The local agent connection ended; request status is unknown. Inspect the workbook before retrying.",
                    );
                    return;
                }
            }
            Ok(Message::Close(_)) => {
                notify_pending(
                    inbox,
                    ctx,
                    &mut pending,
                    "The local agent connection closed; request status is unknown. Inspect the workbook before retrying.",
                );
                return;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error)) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(error) => {
                notify_pending(
                    inbox,
                    ctx,
                    &mut pending,
                    "The local agent connection ended; request status is unknown. Inspect the workbook before retrying.",
                );
                log::debug!("native AI agent disconnected: {error}");
                return;
            }
        }
        match requests.try_recv() {
            Ok(request) => queued.push_back((request, Instant::now())),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return,
        }
    }
}

#[allow(clippy::disallowed_methods)] // Native desktop bridge; gridcraft-web never runs it.
fn drain_requests(requests: &Receiver<Json>, queued: &mut VecDeque<(Json, Instant)>, inbox: &InboxJson, ctx: &egui::Context) {
    loop {
        match requests.try_recv() {
            Ok(request) if queued.len() < MAX_QUEUED_REQUESTS => queued.push_back((request, Instant::now())),
            Ok(request) => {
                notify_error(inbox, ctx, request.get("id").cloned(), "Agent queue is full. Wait for the current request to finish.".into())
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
        }
    }
}

#[allow(clippy::disallowed_methods)] // Native desktop bridge; gridcraft-web never runs it.
fn expire_queued(queued: &mut VecDeque<(Json, Instant)>, inbox: &InboxJson, ctx: &egui::Context) {
    let now = Instant::now();
    while queued.front().is_some_and(|(_, created)| now.duration_since(*created) >= QUEUED_REQUEST_TIMEOUT) {
        if let Some((request, _)) = queued.pop_front() {
            notify_error(
                inbox,
                ctx,
                request.get("id").cloned(),
                "Local web-control did not connect. Check that gridcraft-web-control.service is running, then submit again.".into(),
            );
        }
    }
}

fn notify_pending(inbox: &InboxJson, ctx: &egui::Context, pending: &mut HashSet<String>, message: &str) {
    for id in pending.drain() {
        notify_error(inbox, ctx, Some(Json::String(id)), message.into());
    }
}

fn notify_error(inbox: &InboxJson, ctx: &egui::Context, id: Option<Json>, message: String) {
    let value = json!({"id": id, "error": {"message": message}});
    push_inbox(inbox, value, ctx);
}

fn push_inbox(inbox: &InboxJson, value: Json, ctx: &egui::Context) {
    inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(value);
    ctx.request_repaint();
}

fn is_workbook_request(value: &Json) -> bool {
    matches!(
        value.get("method").and_then(Json::as_str),
        Some("document.inspect" | "sheet.read" | "cell.get" | "engine.execute" | "agent.snapshot" | "agent.applyReviewed" | "agent.undoReviewed")
    )
}

fn forward_workbook_request(
    socket: &mut WebSocket<TcpStream>,
    control_tx: &std::sync::mpsc::Sender<ControlRequest>,
    ctx: &egui::Context,
    value: Json,
) -> Result<(), String> {
    let id = value.get("id").cloned().ok_or_else(|| "workbook request has no id".to_string())?;
    let method = value.get("method").and_then(Json::as_str).ok_or_else(|| "workbook request has no method".to_string())?;
    let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
    let (request, reply) = ControlRequest::new(method, params);
    control_tx.send(request).map_err(|e| e.to_string())?;
    ctx.request_repaint();
    let mut response = reply.recv_timeout(UI_REPLY_TIMEOUT).unwrap_or_else(|_| json!({"ok": false, "error": "native workbook UI timed out"}));
    if let Some(object) = response.as_object_mut() {
        object.insert("id".into(), id);
    }
    socket.send(Message::Text(response.to_string().into())).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_the_named_env_assignment_and_removes_matching_quotes() {
        assert_eq!(assignment_value("# GRIDCRAFT_CONTROL_TOKEN=hidden", "GRIDCRAFT_CONTROL_TOKEN"), None);
        assert_eq!(assignment_value("OTHER=value", "GRIDCRAFT_CONTROL_TOKEN"), None);
        assert_eq!(assignment_value(" GRIDCRAFT_CONTROL_TOKEN = 'abc1234567890123' ", "GRIDCRAFT_CONTROL_TOKEN"), Some("abc1234567890123".into()));
    }

    #[test]
    fn accepts_only_server_workbook_methods() {
        assert!(is_workbook_request(&json!({"method":"sheet.read"})));
        assert!(is_workbook_request(&json!({"method":"engine.execute"})));
        assert!(is_workbook_request(&json!({"method":"agent.snapshot"})));
        assert!(is_workbook_request(&json!({"method":"agent.applyReviewed"})));
        assert!(is_workbook_request(&json!({"method":"agent.undoReviewed"})));
        assert!(!is_workbook_request(&json!({"method":"agent.run"})));
        assert!(!is_workbook_request(&json!({"method":"app.quit"})));
    }

    #[test]
    fn forwards_workbook_request_and_preserves_rpc_id() {
        use std::net::TcpListener;
        use std::sync::mpsc::channel;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            tungstenite::accept(stream).unwrap()
        });
        let stream = TcpStream::connect(address).unwrap();
        let (mut peer, _) = tungstenite::client(format!("ws://{address}"), stream).unwrap();
        let mut socket = acceptor.join().unwrap();
        let (control_tx, control_rx) = channel::<ControlRequest>();
        let ui = thread::spawn(move || {
            let request = control_rx.recv().unwrap();
            assert_eq!(request.method, "sheet.read");
            assert_eq!(request.params, json!({"range":"A1:B2"}));
            request.reply.send(json!({"ok":true,"result":{"range":"A1:B2"}})).unwrap();
        });
        let ctx = egui::Context::default();
        forward_workbook_request(&mut socket, &control_tx, &ctx, json!({"id":9001,"method":"sheet.read","params":{"range":"A1:B2"}})).unwrap();
        let Message::Text(response) = peer.read().unwrap() else { panic!("expected a text response") };
        let response: Json = serde_json::from_str(&response).unwrap();
        assert_eq!(response, json!({"id":9001,"ok":true,"result":{"range":"A1:B2"}}));
        ui.join().unwrap();
    }
}
