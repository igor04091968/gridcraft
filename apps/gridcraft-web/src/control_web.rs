use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};

use gridcraft_ui_egui::{ControlRequest, SheetApp};
use serde_json::{Value as Json, json};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

pub struct BrowserControl {
    socket: web_sys::WebSocket,
    incoming: Rc<RefCell<Vec<String>>>,
    requests: Sender<ControlRequest>,
    pending: Vec<(Json, Receiver<Json>)>,
    auth_failed: Rc<Cell<bool>>,
    authenticated: Rc<Cell<bool>>,
    started: web_time::Instant,
    _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_open: Closure<dyn FnMut()>,
    _on_error: Closure<dyn FnMut(web_sys::Event)>,
    _on_close: Closure<dyn FnMut(web_sys::Event)>,
}

impl BrowserControl {
    pub fn connect(url: &str, token: &str, ctx: egui::Context, app: &mut SheetApp) -> Option<Self> {
        let parsed = match web_sys::Url::new(url) {
            Ok(parsed) => parsed,
            Err(_) => {
                app.agent.status = "Некорректный адрес сервера агента.".into();
                return None;
            }
        };
        let local = matches!(parsed.hostname().as_str(), "127.0.0.1" | "localhost" | "[::1]");
        if !(parsed.protocol() == "wss:" || (parsed.protocol() == "ws:" && local)) || !parsed.username().is_empty() || !parsed.password().is_empty() {
            app.agent.status = "Используйте wss:// для удалённого сервера или ws:// для localhost.".into();
            return None;
        }
        if token.len() < gridcraft_mcp::control_auth::MIN_TOKEN_LEN || token.chars().any(char::is_control) {
            app.agent.status = "Некорректный или слишком короткий токен подключения.".into();
            return None;
        }
        let socket = web_sys::WebSocket::new(url).map_err(|e| log::error!("control WebSocket: {e:?}")).ok()?;
        let auth_failed = Rc::new(Cell::new(false));
        let authenticated = Rc::new(Cell::new(false));
        let denied = auth_failed.clone();
        let accepted = authenticated.clone();
        let incoming = Rc::new(RefCell::new(Vec::new()));
        let messages = Rc::clone(&incoming);
        let agent_inbox = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent_messages = std::sync::Arc::clone(&agent_inbox);
        let status_ctx = ctx.clone();
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            if let Some(text) = event.data().as_string() {
                if let Ok(value) = serde_json::from_str::<Json>(&text)
                    && let Some(allowed) = crate::control_response::auth_outcome(&value, accepted.get())
                {
                    let response = if allowed {
                        accepted.set(true);
                        json!({"id":"agent-connection", "result":{"bridgeConnected":true}})
                    } else {
                        denied.set(true);
                        json!({"id":"agent-connection", "error":{"message":"Сервер отклонил токен подключения."}})
                    };
                    agent_messages.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(response);
                    ctx.request_repaint();
                    return;
                }
                let is_agent = serde_json::from_str::<Json>(&text)
                    .ok()
                    .and_then(|v| v.get("id").and_then(Json::as_str).map(str::to_string))
                    .is_some_and(|id| id.starts_with("agent-"));
                if is_agent {
                    if let Ok(value) = serde_json::from_str::<Json>(&text) {
                        agent_messages.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(value);
                    }
                } else {
                    messages.borrow_mut().push(text);
                }
                ctx.request_repaint();
            }
        });
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        let auth = gridcraft_mcp::control_auth::auth_line(0, token);
        let opened = socket.clone();
        let on_open = Closure::<dyn FnMut()>::new(move || {
            if let Err(e) = opened.send_with_str(&auth) {
                log::error!("control authentication failed to send: {e:?}");
            }
        });
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        let errors = std::sync::Arc::clone(&agent_inbox);
        let error_ctx = status_ctx.clone();
        let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(
                json!({"id":"agent-connection", "error":{"message":"Не удалось подключиться к серверу агента. Проверьте адрес и запуск службы."}}),
            );
            error_ctx.request_repaint();
        });
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        let closed = std::sync::Arc::clone(&agent_inbox);
        let rejected = auth_failed.clone();
        let on_close = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            if rejected.get() {
                return;
            }
            closed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(json!({"id":"agent-connection", "result":{"bridgeDisconnected":true}}));
            status_ctx.request_repaint();
        });
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        let (requests, rx) = channel();
        app.control_rx = Some(rx);
        app.services.agent_inbox = Some(std::sync::Arc::clone(&agent_inbox));
        let agent_socket = socket.clone();
        let failures = std::sync::Arc::clone(&agent_inbox);
        app.services.agent_send = Some(Box::new(move |request| {
            if let Err(e) = agent_socket.send_with_str(&request.to_string()) {
                log::debug!("agent request failed to send: {e:?}");
                failures.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(json!({"id":request.get("id"), "error":{"message":"Нет подключения к серверу агента. Подключитесь через настройки веб-интерфейса."}}));
            }
        }));
        Some(Self {
            socket,
            incoming,
            requests,
            pending: Vec::new(),
            auth_failed,
            authenticated,
            started: web_time::Instant::now(),
            _on_message: on_message,
            _on_open: on_open,
            _on_error: on_error,
            _on_close: on_close,
        })
    }

    pub fn auth_failed(&self) -> bool {
        self.auth_failed.get()
    }

    pub fn authenticated(&self) -> bool {
        self.authenticated.get()
    }

    pub fn needs_reconnect(&self) -> bool {
        self.socket.ready_state() == web_sys::WebSocket::CLOSED
            || (!self.authenticated.get() && self.started.elapsed() > std::time::Duration::from_secs(12))
    }

    pub fn close(self) {
        self.socket.set_onmessage(None);
        self.socket.set_onopen(None);
        self.socket.set_onerror(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close();
    }

    pub fn queue(&mut self) {
        for text in std::mem::take(&mut *self.incoming.borrow_mut()) {
            let request = serde_json::from_str::<Json>(&text).unwrap_or_else(|e| json!({"error": e.to_string()}));
            let id = request.get("id").cloned().unwrap_or(Json::Null);
            let Some(method) = request.get("method").and_then(Json::as_str).map(str::to_string) else {
                continue;
            };
            let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
            let (request, reply) = ControlRequest::new(method, params);
            if self.requests.send(request).is_ok() {
                self.pending.push((id, reply));
            }
        }
    }

    pub fn reply(&mut self) {
        let mut ready = Vec::new();
        self.pending.retain_mut(|(id, rx)| match rx.try_recv() {
            Ok(mut response) => {
                if let Some(object) = response.as_object_mut() {
                    object.insert("id".into(), id.clone());
                }
                ready.push(response);
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        });
        for response in ready {
            if let Err(e) = self.socket.send_with_str(&response.to_string()) {
                log::error!("control reply failed: {e:?}");
            }
        }
    }
}

/// Credentials live only in this tab's memory. Never replay an RPC after reconnecting.
pub struct ConnectionPlan {
    pub url: String,
    pub token: String,
    attempts: u32,
    next_attempt: Option<web_time::Instant>,
}

impl ConnectionPlan {
    pub fn new(url: String, token: String) -> Self {
        Self { url, token, attempts: 0, next_attempt: None }
    }

    pub fn update(&mut self, control: &mut Option<BrowserControl>, app: &mut SheetApp, ctx: &egui::Context) -> bool {
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        if control.as_ref().is_some_and(BrowserControl::auth_failed) {
            return false;
        }
        if control.as_ref().is_some_and(|c| c.authenticated() && !c.needs_reconnect()) {
            self.attempts = 0;
            self.next_attempt = None;
            return true;
        }
        if control.as_ref().is_some_and(|c| !c.needs_reconnect()) {
            return true;
        }
        let now = web_time::Instant::now();
        let deadline = self.next_attempt.get_or_insert_with(|| {
            let delay = 1u64.checked_shl(self.attempts.min(5)).unwrap_or(30).min(30);
            now + std::time::Duration::from_secs(delay)
        });
        if now < *deadline {
            return true;
        }
        if let Some(old) = control.take() {
            old.close();
        }
        app.services.agent_send = None;
        app.services.agent_inbox = None;
        app.agent.reset_connection();
        *control = BrowserControl::connect(&self.url, &self.token, ctx.clone(), app);
        self.attempts = self.attempts.saturating_add(1);
        self.next_attempt = None;
        control.is_some()
    }
}

pub fn default_url() -> Option<String> {
    let location = web_sys::window()?.location();
    if location.protocol().ok()?.as_str() == "https:" {
        Some(format!("{}/agent/ws", location.origin().ok()?.replacen("https://", "wss://", 1)))
    } else {
        None
    }
}
