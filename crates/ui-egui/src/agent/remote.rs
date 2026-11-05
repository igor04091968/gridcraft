use crate::SheetApp;
use serde_json::{Value, json};

pub(super) struct RemoteUi {
    requested: bool,
    listen: String,
    origin: String,
    directory: String,
    certificate: String,
    private_key: String,
    autostart: bool,
    running: bool,
    clients: u64,
    error: String,
    ws_url: String,
    token: String,
}

impl Default for RemoteUi {
    fn default() -> Self {
        Self {
            requested: false,
            listen: "0.0.0.0:8443".into(),
            origin: "https://localhost:8443".into(),
            directory: String::new(),
            certificate: String::new(),
            private_key: String::new(),
            autostart: false,
            running: false,
            clients: 0,
            error: String::new(),
            ws_url: String::new(),
            token: String::new(),
        }
    }
}

impl RemoteUi {
    pub(super) fn hide_token(&mut self) {
        self.token.clear();
    }
    pub(super) fn receive(&mut self, result: &Value) {
        if let Some(token) = result.get("pairingToken").and_then(Value::as_str) {
            self.token = token.into();
            return;
        }
        for (key, field) in [
            ("listen", &mut self.listen),
            ("origin", &mut self.origin),
            ("staticDir", &mut self.directory),
            ("certificate", &mut self.certificate),
            ("privateKey", &mut self.private_key),
            ("wsUrl", &mut self.ws_url),
        ] {
            if let Some(value) = result.get(key).and_then(Value::as_str) {
                *field = value.into();
            }
        }
        self.autostart = result.get("autostart").and_then(Value::as_bool).unwrap_or(false);
        self.running = result.get("running").and_then(Value::as_bool).unwrap_or(false);
        self.clients = result.get("clients").and_then(Value::as_u64).unwrap_or(0);
        self.error = result.get("serverError").and_then(Value::as_str).unwrap_or_default().into();
        self.token.clear();
    }
}

pub(super) fn show(app: &mut SheetApp, ui: &mut egui::Ui) {
    egui::CollapsingHeader::new("Удалённый доступ HTTPS / WSS").show(ui, |ui| {
        if !app.agent.remote.requested {
            app.agent.remote.requested = true;
            app.agent_send("server.status", json!({}));
        }
        let remote = &mut app.agent.remote;
        ui.label(if remote.running { "Сервер работает" } else { "Сервер остановлен" });
        ui.label(format!("Подключений: {}", remote.clients));
        ui.add_enabled_ui(!remote.running, |ui| {
            for (label_text, field, secret) in [("Слушать IP:порт", &mut remote.listen, false), ("Адрес HTTPS", &mut remote.origin, false),
                ("Папка веб-приложения", &mut remote.directory, false), ("Сертификат PEM", &mut remote.certificate, false),
                ("Закрытый ключ PEM", &mut remote.private_key, true)] {
                ui.horizontal(|ui| {
                    let label = ui.label(label_text);
                    ui.add(egui::TextEdit::singleline(field).password(secret)).labelled_by(label.id);
                    if ui.add_enabled(!field.is_empty(), egui::Button::new("Копировать")).clicked() {
                        ui.ctx().copy_text(field.clone());
                        app.agent.status = "Значение скопировано в буфер обмена.".into();
                    }
                });
            }
            ui.checkbox(&mut remote.autostart, "Запускать при открытии GridCraft");
        });
        if !remote.error.is_empty() { ui.label(&remote.error); }
        if !remote.ws_url.is_empty() { ui.label(format!("Агент: {}", remote.ws_url)); }
        let mut action = None;
        ui.horizontal(|ui| {
            if ui.add_enabled(!remote.running, egui::Button::new("Сохранить сервер")).clicked() {
                action = Some(("server.configure", json!({"listen":remote.listen,"origin":remote.origin,"staticDir":remote.directory,
                    "certificate":remote.certificate,"privateKey":remote.private_key,"autostart":remote.autostart})));
            }
            if ui.add_enabled(!remote.running, egui::Button::new("Запустить сервер")).clicked() { action = Some(("server.start",json!({}))); }
            if ui.add_enabled(remote.running, egui::Button::new("Остановить сервер")).clicked() { action = Some(("server.stop",json!({}))); }
            if ui.button("Обновить статус").clicked() { action = Some(("server.status",json!({}))); }
        });
        if remote.token.is_empty() {
            if ui.button("Показать токен подключения").clicked() { action = Some(("server.pair",json!({}))); }
        } else {
            ui.add(egui::TextEdit::singleline(&mut remote.token).password(true).interactive(false));
            if ui.button("Копировать токен").clicked() { ui.ctx().copy_text(remote.token.clone()); }
            if ui.button("Скрыть токен").clicked() { remote.token.clear(); }
        }
        if ui.button("Сменить токен и отключить клиентов").clicked() {
            remote.token.clear();
            action = Some(("server.rotateToken", json!({})));
        }
        ui.small("Нужен доверенный сертификат для указанного домена и абсолютные пути к файлам. После обновления сертификата новые подключения используют его автоматически.");
        if let Some((method, params)) = action { app.agent_send(method, params); }
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use egui_kittest::kittest::Queryable as _;
    use std::sync::{Arc, Mutex};

    #[test]
    fn remote_panel_saves_configuration_and_never_persists_pairing_token() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let sink = requests.clone();
        let inbox = Arc::new(Mutex::new(Vec::new()));
        let responses = inbox.clone();
        let mut app = SheetApp::new(
            gridcraft_engine::Session::new(),
            crate::Services {
                agent_inbox: Some(inbox),
                agent_send: Some(Box::new(move |request| {
                    let result = if request["method"] == "server.pair" {
                        json!({"pairingToken":"synthetic-private-token"})
                    } else {
                        json!({"remoteServer":true,"running":request["method"] == "server.start"})
                    };
                    responses.lock().unwrap().push(json!({"id":request["id"],"result":result}));
                    sink.lock().unwrap().push(request);
                })),
                ..Default::default()
            },
        );
        app.agent.open = true;
        app.agent.remote.directory = "/test/web".into();
        app.agent.remote.certificate = "/test/cert.pem".into();
        app.agent.remote.private_key = "/test/key.pem".into();
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1300.0, 1100.0)).build_ui_state(
            |ui, app| {
                app.poll_agent();
                super::super::show(app, &ui.ctx().clone());
            },
            app,
        );
        harness.run_steps(2);
        harness.get_by_label("Удалённый доступ HTTPS / WSS").click();
        harness.run_steps(3);
        harness.get_by_label("Сохранить сервер").click();
        harness.run_steps(3);
        let sent = requests.lock().unwrap();
        let saved = sent.iter().find(|request| request["method"] == "server.configure").unwrap();
        assert_eq!(saved["params"]["staticDir"], "/test/web");
        assert!(saved["params"].get("token").is_none());
        drop(sent);
        harness.get_by_label("Запустить сервер").click();
        harness.run_steps(3);
        assert!(harness.state().agent.remote.running);
        harness.get_by_label("Показать токен подключения").click();
        harness.run_steps(3);
        assert_eq!(harness.state().agent.remote.token, "synthetic-private-token");
        assert!(!harness.state().agent.preferences().to_string().contains("token"));
        assert!(!harness.state().agent.log.iter().any(|line| line.contains("synthetic-private-token")));
        harness.get_by_label("Скрыть токен").click();
        harness.run_steps(2);
        assert!(harness.state().agent.remote.token.is_empty());
        harness.get_by_label("Остановить сервер").click();
        harness.run_steps(3);
        assert!(!harness.state().agent.remote.running);
    }
}
