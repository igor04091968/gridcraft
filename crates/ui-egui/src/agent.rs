use std::collections::HashSet;

use serde_json::{Value as Json, json};

use crate::SheetApp;

#[cfg(test)]
mod feedback_tests;
mod remote;
mod review;

const POLLINATIONS_DEFAULT_MODEL: &str = "openai/gpt-6-luna";
const POLLINATIONS_LEGACY_DEFAULT_MODEL: &str = "stealth/glyph-cluster";

pub struct AgentModel {
    name: String,
    label: String,
}

impl AgentModel {
    fn from_catalog(model: &Json) -> Option<Self> {
        let name = model.get("name")?.as_str().filter(|name| valid_model(name))?.to_string();
        let title = model.get("title").and_then(Json::as_str).unwrap_or(&name).chars().take(160).collect::<String>();
        let cost = match model.get("cost").and_then(Json::as_str) {
            Some("free") => "Free",
            Some("paid") => "Платная",
            _ => "Цена неизвестна",
        };
        let specialized = if model.get("specialized") == Some(&Json::Bool(true)) { " · специализированная" } else { "" };
        Some(Self { label: format!("{title} · {cost}{specialized} ({name})"), name })
    }
}

#[derive(Default)]
pub struct AgentUi {
    pub open: bool,
    pub task: String,
    pub provider: String,
    pub model: String,
    pub range: String,
    pub max_steps: u32,
    pub status: String,
    pub proposal: Option<Json>,
    pub inspections: Vec<String>,
    pub log: Vec<String>,
    pub api_key_input: String,
    pub models: Vec<AgentModel>,
    pub configured: bool,
    pub connection_status: String,
    /// Present only after the user explicitly requests a saved key; never persisted or logged.
    revealed_key: String,
    revealed_provider: String,
    pub control_url: String,
    pub control_token_input: String,
    pub remember_control_token: bool,
    pub forget_control_token: bool,
    remote: remote::RemoteUi,
    web_connected: bool,
    settings_requested: bool,
    next_id: u64,
    pending: HashSet<String>,
    last_applied: Option<review::AppliedAction>,
    applying: Option<(String, Json)>,
    last_change: Option<Json>,
}

impl AgentUi {
    /// Non-secret preferences shared by desktop and browser persistence.
    pub fn preferences(&self) -> Json {
        json!({"provider":self.provider, "model":self.model})
    }

    pub fn restore_preferences(&mut self, value: &Json) {
        if let Some(provider) = value.get("provider").and_then(Json::as_str)
            && matches!(provider, "openai" | "kimi" | "deepseek" | "mistral" | "pollinations" | "kilo")
        {
            self.provider = provider.into();
            if provider == "pollinations" {
                self.model = POLLINATIONS_DEFAULT_MODEL.into();
            } else if provider == "kilo" {
                self.model = "stealth/glyph-cluster".into();
            }
        }
        if let Some(model) = value.get("model").and_then(Json::as_str).filter(|model| valid_model(model)) {
            self.model = if self.provider == "pollinations" && model == POLLINATIONS_LEGACY_DEFAULT_MODEL {
                POLLINATIONS_DEFAULT_MODEL.into()
            } else {
                model.into()
            };
        }
    }

    pub fn reset_connection(&mut self) {
        self.web_connected = false;
        if !self.pending.is_empty() {
            self.log
                .push("Связь прервалась во время запроса. Проверьте книгу перед повторной отправкой; запросы автоматически не повторяются.".into());
        }
        self.pending.clear();
        self.settings_requested = false;
        self.proposal = None;
        self.inspections.clear();
        self.applying = None;
        self.last_change = None;
        self.configured = false;
        self.connection_status.clear();
        self.revealed_key.clear();
        self.revealed_provider.clear();
    }

    pub fn new() -> Self {
        Self { provider: "openai".into(), model: "gpt-6.1-sol".into(), max_steps: 4, control_url: "ws://127.0.0.1:8780".into(), ..Default::default() }
    }
}

impl SheetApp {
    pub fn poll_agent(&mut self) {
        let responses = self
            .services
            .agent_inbox
            .as_ref()
            .map(|inbox| std::mem::take(&mut *inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner)))
            .unwrap_or_default();
        for response in responses {
            if let Some(id) = response.get("id").and_then(Json::as_str) {
                self.agent.pending.remove(id);
            }
            let applied_action = self
                .agent
                .applying
                .as_ref()
                .filter(|(id, _)| response.get("id").and_then(Json::as_str) == Some(id.as_str()))
                .map(|(_, action)| action.clone());
            if applied_action.is_some() {
                self.agent.applying = None;
            }
            if let Some(error) = response.pointer("/error/message").and_then(Json::as_str) {
                self.agent.status =
                    if applied_action.is_some() { format!("Не удалось применить: {error}") } else { error.to_string() };
                self.agent.log.push(format!("Error: {error}"));
            } else if let Some(result) = response.get("result") {
                if let (Some(key), Some(provider)) = (result.get("copiedKey").and_then(Json::as_str), result.get("provider").and_then(Json::as_str)) {
                    self.agent.revealed_key.clear();
                    self.agent.revealed_key.push_str(key);
                    self.agent.revealed_provider = provider.to_owned();
                    self.agent.status = "Ключ загружен в поле настроек. Нажмите «Копировать» или «Скрыть» после проверки.".into();
                } else if result.get("remoteServer") == Some(&Json::Bool(true)) || result.get("pairingToken").is_some() {
                    self.agent.remote.receive(result);
                    self.agent.status = "Настройки удалённого доступа обновлены.".into();
                } else if result.get("bridgeConnected") == Some(&Json::Bool(true)) {
                    self.agent.web_connected = true;
                    self.agent.settings_requested = false;
                    self.agent.status = "Веб-интерфейс подключён к агенту.".into();
                } else if result.get("bridgeDisconnected") == Some(&Json::Bool(true)) {
                    self.agent.reset_connection();
                    self.agent.status =
                        "Соединение прервано. Отправленные команды не повторяются; подключение восстанавливается автоматически.".into();
                } else if matches!(result.get("provider").and_then(Json::as_str), Some("pollinations" | "mistral" | "kilo"))
                    && result.get("configured").and_then(Json::as_bool).is_some()
                    && result.get("provider").and_then(Json::as_str) == Some(self.agent.provider.as_str())
                {
                    self.agent.configured = result.get("configured").and_then(Json::as_bool).unwrap_or(false);
                    if let Some(model) = result.get("model").and_then(Json::as_str).filter(|model| valid_model(model)) {
                        self.agent.model = model.into();
                    }
                    self.agent.connection_status = if self.agent.configured {
                        "Ключ настроен."
                    } else if self.agent.provider == "mistral" {
                        "Введите API-ключ Mistral."
                    } else if self.agent.provider == "kilo" {
                        "Введите API-ключ Kilo Gateway."
                    } else {
                        "Введите API-ключ Pollinations."
                    }
                    .into();
                    self.agent.status = self.agent.connection_status.clone();
                } else if let Some(models) = result.get("models").and_then(Json::as_array) {
                    self.agent.models = models.iter().filter_map(AgentModel::from_catalog).take(1000).collect();
                    self.agent.status = "Список моделей обновлён.".into();
                } else if result.get("connectionVerified") == Some(&Json::Bool(true)) {
                    let provider = result.get("provider").and_then(Json::as_str).unwrap_or(&self.agent.provider);
                    self.agent.connection_status = if result.get("usingDraftKey") == Some(&Json::Bool(true)) {
                        "Ключ проверен. Сохраните настройки, чтобы использовать его.".into()
                    } else if provider == "mistral" {
                        "Подключение к Mistral проверено.".into()
                    } else if provider == "kilo" {
                        "Подключение к Kilo Gateway проверено.".into()
                    } else {
                        "Подключение к Pollinations проверено.".into()
                    };
                    self.agent.status = self.agent.connection_status.clone();
                } else if let Some(proposal) = result.get("proposal") {
                    self.agent.proposal = Some(proposal.clone());
                    self.agent.last_change = None;
                    self.agent.inspections = result
                        .get("inspections")
                        .and_then(Json::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|item| {
                            let range = item.get("range").and_then(Json::as_str)?;
                            let summary = item.get("summary").and_then(Json::as_str).unwrap_or("проверка данных");
                            Some(format!("{range}: {summary}"))
                        })
                        .take(20)
                        .collect();
                    self.agent.status = if self.agent.inspections.is_empty() {
                        "Предложение готово. Книга не изменена.".into()
                    } else {
                        format!("Предложение готово после {} проверок диапазонов. Книга не изменена.", self.agent.inspections.len())
                    };
                } else if let Some(applied) = result.get("applied").and_then(Json::as_bool) {
                    if applied {
                        let address = result
                            .pointer("/result/cell")
                            .or_else(|| result.pointer("/result/range"))
                            .and_then(Json::as_str)
                            .or_else(|| applied_action.as_ref().and_then(action_range))
                            .filter(|address| address.len() <= 256);
                        self.agent.status = match address {
                            Some(address) => format!("Изменения применены: {address}."),
                            None => "Изменения применены к книге.".into(),
                        };
                        if let Some(action) = applied_action {
                            self.agent.last_change = Some(action);
                            self.agent.proposal = None;
                        }
                    } else {
                        self.agent.status = "Книга не изменилась. Проверьте команду и её параметры в предложении.".into();
                    }
                    self.agent.log.push(self.agent.status.clone());
                } else if result.get("undone") == Some(&Json::Bool(true)) {
                    self.agent.last_change = None;
                    self.agent.status = "Изменения агента отменены.".into();
                    self.agent.log.push(self.agent.status.clone());
                } else {
                    self.agent.status = "Command completed.".into();
                    self.agent.log.push("Command completed.".into());
                }
            }
        }
    }

    fn agent_send(&mut self, method: &str, params: Json) {
        self.agent.next_id = self.agent.next_id.saturating_add(1);
        let id = format!("agent-{}", self.agent.next_id);
        let request = json!({"id": id, "method": method, "params": params});
        if let Some(send) = &self.services.agent_send {
            self.agent.pending.insert(id);
            send(request);
            self.agent.status = "Агент анализирует книгу…".into();
        } else {
            self.agent.status = "Подключение агента недоступно. Проверьте запуск локальной службы или подключение веб-интерфейса.".into();
        }
    }

    fn apply_agent_proposal(&mut self, action: Json) {
        self.agent_send("agent.apply", action.clone());
        if self.services.agent_send.is_some() {
            self.agent.applying = Some((format!("agent-{}", self.agent.next_id), action));
        }
    }

    fn agent_operation_pending(&self) -> bool {
        self.session.active().is_some_and(|doc| self.session.in_group(doc.uid))
    }

    fn start_agent_task(&mut self) {
        if self.agent.task.trim().is_empty() || !self.agent.pending.is_empty() {
            return;
        }
        if self.agent_operation_pending() {
            self.agent.status = "Дождитесь завершения текущей операции с книгой перед запуском агента.".into();
            return;
        }
        if self.services.agent_send.is_none() || (self.services.agent_connect.is_some() && !self.agent.web_connected) {
            self.agent.status = "Подключение агента недоступно. Подключитесь перед отправкой задачи.".into();
            return;
        }
        // Only the explicitly labeled UI action saves a draft. Snapshot/Apply/Undo
        // RPCs continue to reject unfinished edits without writing anything.
        if !self.commit_edit(0, 0, false, false) {
            let reason = self.message.as_ref().map(|(_, message)| message.as_str()).unwrap_or("Проверьте ввод в ячейке.");
            self.agent.status = format!("Не удалось сохранить ввод ячейки: {reason} Задача агенту не отправлена.");
            return;
        }
        let task = self.agent.task.clone();
        self.agent.proposal = None;
        self.agent.inspections.clear();
        self.agent.log.push(format!("Задача: {task}"));
        self.agent_send(
            "agent.run",
            json!({"task":task, "provider":self.agent.provider, "model":self.agent.model, "range":self.agent.range, "maxSteps":self.agent.max_steps}),
        );
    }

    fn show_agent_change(&mut self) -> Result<(), String> {
        let action = self.agent.last_change.clone().ok_or("Нет применённых изменений для просмотра.")?;
        let doc = self.session.doc().map_err(|e| e.to_string())?;
        if action.pointer("/context/doc").and_then(Json::as_u64) != Some(doc.uid) {
            return Err("Изменения относятся к другой книге. Сначала откройте её.".into());
        }
        if let Some(range) = action_range(&action) {
            let mut params = json!({"range": range});
            if let Some(sheet) = action.pointer("/params/sheet").or_else(|| if range.contains('!') { None } else { action.pointer("/context/sheet") })
            {
                params["sheet"] = sheet.clone();
            }
            self.run("selection.set", params)?;
        }
        self.grid.ensure_visible = true;
        self.agent.open = false;
        Ok(())
    }
}

fn action_range(action: &Json) -> Option<&str> {
    action.pointer("/params/cell").or_else(|| action.pointer("/params/range")).and_then(Json::as_str)
}

pub fn show(app: &mut SheetApp, ctx: &egui::Context) {
    if !app.agent.open {
        return;
    }
    if app.agent.provider == "pollinations"
        && !app.agent.settings_requested
        && app.services.agent_send.is_some()
        && (app.services.agent_connect.is_none() || app.agent.web_connected)
    {
        app.agent.settings_requested = true;
        app.agent_send("agent.settings", json!({"provider":"pollinations"}));
        app.agent_send("agent.models", json!({"provider":"pollinations"}));
    }
    if app.agent.provider == "mistral"
        && !app.agent.settings_requested
        && app.services.agent_send.is_some()
        && (app.services.agent_connect.is_none() || app.agent.web_connected)
    {
        app.agent.settings_requested = true;
        app.agent_send("agent.settings", json!({"provider":"mistral"}));
    }
    if app.agent.provider == "kilo"
        && !app.agent.settings_requested
        && app.services.agent_send.is_some()
        && (app.services.agent_connect.is_none() || app.agent.web_connected)
    {
        app.agent.settings_requested = true;
        app.agent_send("agent.settings", json!({"provider":"kilo"}));
        app.agent_send("agent.models", json!({"provider":"kilo"}));
    }
    let mut open = app.agent.open;
    let mut close_after_change = false;
    egui::Window::new("AI-агент").id(egui::Id::unique("gridcraft-agent")).open(&mut open).resizable(true).default_width(480.0).show(ctx, |ui| {
        ui.label("Изменения агента требуют подтверждения");
        if app.services.agent_connect.is_none() && app.services.agent_send.is_some() {
            remote::show(app, ui);
        }
        if app.services.agent_connect.is_some() {
            egui::CollapsingHeader::new("Подключение веб-интерфейса").default_open(app.services.agent_send.is_none()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let label = ui.label("Адрес сервера");
                    ui.text_edit_singleline(&mut app.agent.control_url).labelled_by(label.id);
                    if ui.button("Копировать").clicked() {
                        ui.ctx().copy_text(app.agent.control_url.clone());
                        app.agent.status = "Адрес сервера скопирован.".into();
                    }
                });
                ui.horizontal(|ui| {
                    let label = ui.label("Токен подключения");
                    ui.add(egui::TextEdit::singleline(&mut app.agent.control_token_input).password(true)).labelled_by(label.id);
                    if ui.add_enabled(!app.agent.control_token_input.is_empty(), egui::Button::new("Копировать")).clicked() {
                        ui.ctx().copy_text(app.agent.control_token_input.clone());
                        app.agent.status = "Токен подключения скопирован.".into();
                    }
                });
                ui.checkbox(&mut app.agent.remember_control_token, "Запомнить на этом устройстве")
                    .on_hover_text("Сохранить токен в этом браузере и подключаться после открытия страницы. «Забыть токен» удаляет сохранение.");
                if ui.button("Забыть токен").clicked() {
                    app.agent.remember_control_token = false;
                    app.agent.forget_control_token = true;
                    app.agent.control_token_input.clear();
                }
                if ui.button("Подключиться").clicked() {
                    let token = std::mem::take(&mut app.agent.control_token_input);
                    if let Some(connect) = &app.services.agent_connect {
                        connect(app.agent.control_url.clone(), token);
                        app.agent.status = "Подключение…".into();
                    }
                }
                if ui.button("Отключиться").clicked() {
                    if let Some(connect) = &app.services.agent_connect {
                        connect(String::new(), String::new());
                    }
                    app.agent.reset_connection();
                    app.agent.status = "Отключено.".into();
                }
                ui.small("Для локального сервера используйте ws://127.0.0.1:8780; для удалённого — wss://.");
            });
        }
        ui.add_space(8.0);
        let mut provider_changed = false;
        ui.horizontal(|ui| {
            let label = ui.label("Провайдер");
            craft_ui::ComboBox::from_id_salt("gridcraft-agent-provider")
                .selected_text(match app.agent.provider.as_str() {
                    "kimi" => "Kimi",
                    "deepseek" => "DeepSeek (PlusVibe)",
                    "mistral" => "Mistral API",
                    "pollinations" => "Pollinations",
                    "kilo" => "Kilo Gateway (Glyph Cluster, free)",
                    _ => "Codex (OpenAI API)",
                })
                .show_ui(ui, |ui| {
                    provider_changed |= ui.selectable_value(&mut app.agent.provider, "openai".into(), "Codex (OpenAI API)").changed();
                    provider_changed |= ui.selectable_value(&mut app.agent.provider, "kimi".into(), "Kimi").changed();
                    provider_changed |= ui.selectable_value(&mut app.agent.provider, "deepseek".into(), "DeepSeek (PlusVibe)").changed();
                    provider_changed |= ui.selectable_value(&mut app.agent.provider, "mistral".into(), "Mistral API").changed();
                    provider_changed |= ui.selectable_value(&mut app.agent.provider, "pollinations".into(), "Pollinations").changed();
                    provider_changed |= ui.selectable_value(&mut app.agent.provider, "kilo".into(), "Kilo Gateway (Glyph Cluster, free)").changed();
                })
                .response
                .labelled_by(label.id);
        });
        if provider_changed {
            app.agent.settings_requested = false;
            app.agent.api_key_input.clear();
            app.agent.revealed_key.clear();
            app.agent.revealed_provider.clear();
            app.agent.models.clear();
            app.agent.connection_status.clear();
            app.agent.proposal = None;
            app.agent.inspections.clear();
            app.agent.model = match app.agent.provider.as_str() {
                "kimi" => "kimi-k3.1-pro".into(),
                "deepseek" => "deepseek-v4-flash-0731:free".into(),
                "mistral" => "mistral-small-latest".into(),
                "pollinations" => POLLINATIONS_DEFAULT_MODEL.into(),
                "kilo" => "stealth/glyph-cluster".into(),
                _ => "gpt-6.1-sol".into(),
            };
        }
        ui.horizontal(|ui| {
            let label = ui.label("Модель");
            ui.text_edit_singleline(&mut app.agent.model).labelled_by(label.id);
            ui.add(egui::DragValue::new(&mut app.agent.max_steps).range(1..=20).prefix("Лимит шагов "));
        });
        if app.agent.provider == "pollinations" {
            show_pollinations_settings(app, ui);
        } else if app.agent.provider == "mistral" {
            show_mistral_settings(app, ui);
        } else if app.agent.provider == "kilo" {
            show_kilo_settings(app, ui);
        } else {
            let key_label = match app.agent.provider.as_str() {
                "kimi" => "Ключ на сервере: GRIDCRAFT_AGENT_KIMI_API_KEY",
                "deepseek" => "PlusVibe API · ключ только на сервере: GRIDCRAFT_AGENT_PLUSVIBE_API_KEY",
                _ => "Ключ на сервере: GRIDCRAFT_AGENT_OPENAI_API_KEY",
            };
            ui.small(key_label);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Показать сохранённый ключ").clicked() {
                    app.agent_send("agent.copyKey", json!({"provider":app.agent.provider}));
                }
                if ui.button("Копировать сведения").clicked() {
                    ui.ctx().copy_text(format!("Провайдер: {}; модель: {}; {}", app.agent.provider, app.agent.model, key_label));
                    app.agent.status = "Сведения скопированы.".into();
                }
            });
            show_revealed_key(app, ui, &app.agent.provider.clone());
        }
        ui.horizontal(|ui| {
            let label = ui.label("Диапазон (необязательно)");
            ui.text_edit_singleline(&mut app.agent.range).labelled_by(label.id);
        });
        let label = ui.label("Задача");
        ui.add(egui::TextEdit::multiline(&mut app.agent.task).desired_rows(5).desired_width(f32::INFINITY)).labelled_by(label.id);
        let operation_pending = app.agent_operation_pending();
        if operation_pending {
            ui.label("Выполняется операция с книгой. Дождитесь её завершения перед запуском агента.");
        } else if let Some(editor) = &app.editor {
            ui.label(format!("В ячейке {} не завершён ввод. «Сохранить ввод и предложить» сохранит ячейку перед чтением таблицы.", editor.cell.a1()));
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Отменить ввод ячейки")).clicked() {
                app.cancel_edit();
                app.agent.status = "Ввод ячейки отменён. Текст задачи сохранён.".into();
            }
        }
        let run_label = if app.editor.is_some() { "Сохранить ввод и предложить" } else { "Предложить" };
        if ui.add_enabled(app.agent.pending.is_empty() && !operation_pending, egui::Button::new(run_label)).clicked() {
            app.start_agent_task();
        }
        ui.separator();
        ui.label(&app.agent.status);
        if let Some(proposal) = app.agent.proposal.clone() {
            for inspection in &app.agent.inspections {
                ui.small(format!("Проверено: {inspection}"));
            }
            if let Some(summary) = proposal.get("summary").and_then(Json::as_str) {
                ui.label(summary);
            }
            if let Some(action) = proposal.get("action").filter(|v| !v.is_null()) {
                ui.monospace(
                    serde_json::to_string_pretty(&json!({"command": action.get("command"), "params": action.get("params")})).unwrap_or_default(),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(app.agent.pending.is_empty() && app.editor.is_none() && !operation_pending, egui::Button::new("Выполнить"))
                        .clicked()
                    {
                        app.apply_agent_proposal(action.clone());
                    }
                    if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Отклонить")).clicked() {
                        app.agent.proposal = None;
                        app.agent.status = "Предложение отклонено; ничего не изменено.".into();
                    }
                });
            }
        }
        if app.agent.last_change.is_some() && ui.button("Показать изменения").clicked() {
            if let Err(error) = app.show_agent_change() {
                app.agent.status = error;
            } else {
                close_after_change = true;
            }
        }
        if ui
            .add_enabled(
                app.agent.pending.is_empty() && app.editor.is_none() && !operation_pending,
                egui::Button::new("Отменить последнее изменение"),
            )
            .clicked()
        {
            app.agent_send("agent.undo", json!({}));
        }
        ui.separator();
        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
            for line in &app.agent.log {
                ui.label(line);
            }
        });
    });
    if close_after_change {
        open = false;
    }
    app.agent.open = open;
    if !open {
        app.agent.remote.hide_token();
        app.agent.revealed_key.clear();
        app.agent.revealed_provider.clear();
        app.agent.api_key_input.clear();
    }
}

fn valid_model(model: &str) -> bool {
    !model.trim().is_empty() && model.len() <= 128 && !model.chars().any(char::is_control)
}

fn show_kilo_settings(app: &mut SheetApp, ui: &mut egui::Ui) {
    egui::CollapsingHeader::new("Настройка Kilo Gateway").default_open(true).show(ui, |ui| {
        ui.small("Kilo Gateway · модель по умолчанию: Glyph Cluster (stealth/glyph-cluster, free).");
        ui.horizontal_wrapped(|ui| {
            let label = ui.label("API-ключ");
            ui.add(egui::TextEdit::singleline(&mut app.agent.api_key_input).password(true).hint_text(if app.agent.configured {
                "Ключ сохранён · оставьте пустым, чтобы сохранить его"
            } else {
                "Введите API-ключ Kilo Gateway"
            }))
            .labelled_by(label.id);
            if ui.add_enabled(!app.agent.api_key_input.is_empty(), egui::Button::new("Копировать введённый ключ")).clicked() {
                ui.ctx().copy_text(app.agent.api_key_input.clone());
                app.agent.status = "Введённый ключ скопирован.".into();
            }
            if ui
                .add_enabled(app.services.paste_text.is_some(), egui::Button::new("Вставить"))
                .on_hover_text("Вставить API-ключ из системного буфера обмена")
                .clicked()
            {
                paste_api_key_from_clipboard(app, "Kilo Gateway");
            }
        });
        ui.horizontal_wrapped(|ui| {
            if app.agent.configured && ui.button("Показать сохранённый ключ").clicked() {
                app.agent_send("agent.copyKey", json!({"provider":"kilo"}));
            }
            if ui.button("Копировать сведения").clicked() {
                ui.ctx().copy_text(format!("Провайдер: Kilo Gateway; модель: {}; API: https://api.kilo.ai/v1", app.agent.model));
                app.agent.status = "Сведения скопированы.".into();
            }
        });
        show_revealed_key(app, ui, "kilo");
        ui.small("Ключ хранится в приватном файле настроек приложения/сервера. В веб-интерфейсе сохраняется только выбор модели.");
        if !app.agent.models.is_empty() {
            craft_ui::ComboBox::from_id_salt("kilo-models").selected_text("Выбрать из каталога моделей").show_ui(ui, |ui| {
                for model in &app.agent.models {
                    ui.selectable_value(&mut app.agent.model, model.name.clone(), &model.label);
                }
            });
        }
        if app.agent.model == "stealth/glyph-cluster" {
            ui.small("Glyph Cluster (free) — модель по умолчанию для Kilo Gateway.");
        }
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Сохранить настройки")).clicked() {
                let key = std::mem::take(&mut app.agent.api_key_input);
                app.agent_send("agent.configure", json!({"provider":"kilo", "model":app.agent.model, "apiKey":key}));
            }
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Проверить подключение")).clicked() {
                app.agent_send("agent.test", json!({"provider":"kilo", "apiKey":app.agent.api_key_input}));
            }
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Обновить модели")).clicked() {
                app.agent_send("agent.models", json!({"provider":"kilo"}));
            }
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Удалить сохранённый ключ")).clicked() {
                app.agent.api_key_input.clear();
                app.agent_send("agent.configure", json!({"provider":"kilo", "clearKey":true}));
            }
        });
        ui.label(&app.agent.connection_status);
    });
}

fn show_pollinations_settings(app: &mut SheetApp, ui: &mut egui::Ui) {
    egui::CollapsingHeader::new("Настройка Pollinations").default_open(true).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            let label = ui.label("API-ключ");
            ui.add(egui::TextEdit::singleline(&mut app.agent.api_key_input).password(true).hint_text(if app.agent.configured {
                "Ключ сохранён · оставьте пустым, чтобы сохранить его"
            } else {
                "Введите ключ Pollinations"
            }))
            .labelled_by(label.id);
            if ui.add_enabled(!app.agent.api_key_input.is_empty(), egui::Button::new("Копировать введённый ключ")).clicked() {
                ui.ctx().copy_text(app.agent.api_key_input.clone());
                app.agent.status = "Введённый ключ скопирован.".into();
            }
            if ui
                .add_enabled(app.services.paste_text.is_some(), egui::Button::new("Вставить"))
                .on_hover_text("Вставить API-ключ из системного буфера обмена")
                .clicked()
            {
                paste_api_key_from_clipboard(app, "Pollinations");
            }
        });
        ui.horizontal_wrapped(|ui| {
            if app.agent.configured && ui.button("Показать сохранённый ключ").clicked() {
                app.agent_send("agent.copyKey", json!({"provider":"pollinations"}));
            }
            if ui.button("Копировать сведения").clicked() {
                ui.ctx().copy_text(format!("Провайдер: Pollinations; модель: {}; API: https://gen.pollinations.ai/v1", app.agent.model));
                app.agent.status = "Сведения скопированы.".into();
            }
        });
        show_revealed_key(app, ui, "pollinations");
        ui.hyperlink_to("Получить API-ключ", "https://enter.pollinations.ai");
        ui.small("Ключ сохраняется на стороне приложения или сервера. В браузере сохраняется только выбор модели.");
        if !app.agent.models.is_empty() {
            craft_ui::ComboBox::from_id_salt("pollinations-models").selected_text("Выбрать из каталога моделей").show_ui(ui, |ui| {
                for model in &app.agent.models {
                    ui.selectable_value(&mut app.agent.model, model.name.clone(), &model.label);
                }
            });
        }
        if app.agent.model == POLLINATIONS_DEFAULT_MODEL {
            ui.small("GPT-6 Luna · reasoning для задач общего назначения.");
        } else if matches!(app.agent.model.as_str(), "qwen/qwen3guard-gen-8b" | "qwen3guard-gen-8b" | "qwen-safety") {
            ui.small("Qwen3Guard · Free · модерация текста. Для работы с таблицей выберите модель общего назначения.");
        }
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Сохранить настройки")).clicked() {
                let key = std::mem::take(&mut app.agent.api_key_input);
                app.agent_send("agent.configure", json!({"provider":"pollinations", "model":app.agent.model, "apiKey":key}));
            }
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Проверить подключение")).clicked() {
                app.agent_send("agent.test", json!({"provider":"pollinations", "apiKey":app.agent.api_key_input}));
            }
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Обновить модели")).clicked() {
                app.agent_send("agent.models", json!({"provider":"pollinations"}));
            }
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Удалить сохранённый ключ")).clicked() {
                app.agent.api_key_input.clear();
                app.agent_send("agent.configure", json!({"provider":"pollinations", "clearKey":true}));
            }
        });
        ui.label(&app.agent.connection_status);
    });
}

fn show_mistral_settings(app: &mut SheetApp, ui: &mut egui::Ui) {
    egui::CollapsingHeader::new("Настройка Mistral API").default_open(true).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            let label = ui.label("API-ключ");
            ui.add(egui::TextEdit::singleline(&mut app.agent.api_key_input).password(true).hint_text(if app.agent.configured {
                "Ключ сохранён · оставьте пустым, чтобы сохранить его"
            } else {
                "Введите API-ключ Mistral"
            }))
            .labelled_by(label.id);
            if ui.add_enabled(!app.agent.api_key_input.is_empty(), egui::Button::new("Копировать введённый ключ")).clicked() {
                ui.ctx().copy_text(app.agent.api_key_input.clone());
                app.agent.status = "Введённый ключ скопирован.".into();
            }
            if ui
                .add_enabled(app.services.paste_text.is_some(), egui::Button::new("Вставить"))
                .on_hover_text("Вставить API-ключ из системного буфера обмена")
                .clicked()
            {
                paste_api_key_from_clipboard(app, "Mistral");
            }
        });
        ui.horizontal_wrapped(|ui| {
            if app.agent.configured && ui.button("Показать сохранённый ключ").clicked() {
                app.agent_send("agent.copyKey", json!({"provider":"mistral"}));
            }
            if ui.button("Копировать сведения").clicked() {
                ui.ctx().copy_text(format!("Провайдер: Mistral API; модель: {}; API: https://api.mistral.ai/v1", app.agent.model));
                app.agent.status = "Сведения скопированы.".into();
            }
        });
        show_revealed_key(app, ui, "mistral");
        ui.hyperlink_to("Панель Mistral API", "https://admin.mistral.ai/");
        ui.small("Ключ хранится в приватном файле настроек GridCraft. Проверка запрашивает список моделей без генерации текста.");
        if !app.agent.models.is_empty() {
            craft_ui::ComboBox::from_id_salt("mistral-models").selected_text("Выбрать доступную модель").show_ui(ui, |ui| {
                for model in &app.agent.models {
                    ui.selectable_value(&mut app.agent.model, model.name.clone(), &model.label);
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Сохранить настройки")).clicked() {
                let key = std::mem::take(&mut app.agent.api_key_input);
                app.agent_send("agent.configure", json!({"provider":"mistral", "model":app.agent.model, "apiKey":key}));
            }
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Проверить подключение")).clicked() {
                app.agent_send("agent.test", json!({"provider":"mistral", "apiKey":app.agent.api_key_input}));
            }
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Обновить модели")).clicked() {
                app.agent_send("agent.models", json!({"provider":"mistral"}));
            }
            if ui.add_enabled(app.agent.pending.is_empty(), egui::Button::new("Удалить сохранённый ключ")).clicked() {
                app.agent.api_key_input.clear();
                app.agent_send("agent.configure", json!({"provider":"mistral", "clearKey":true}));
            }
        });
        ui.label(&app.agent.connection_status);
    });
}

fn paste_api_key_from_clipboard(app: &mut SheetApp, provider: &str) {
    let Some(text) = app.system_clipboard_text() else {
        app.agent.status = "В буфере обмена нет текста.".into();
        return;
    };
    let key = text.trim();
    if key.is_empty() {
        app.agent.status = "В буфере обмена нет текста.".into();
    } else if key.len() > 4096 || key.chars().any(char::is_whitespace) || key.chars().any(char::is_control) {
        app.agent.status = format!("Текст в буфере не подходит как API-ключ {provider}.");
    } else {
        app.agent.api_key_input = key.to_string();
        app.agent.status = format!("API-ключ {provider} вставлен в скрытое поле.");
    }
}

fn show_revealed_key(app: &mut SheetApp, ui: &mut egui::Ui, provider: &str) {
    if app.agent.revealed_key.is_empty() || app.agent.revealed_provider != provider {
        return;
    }
    ui.horizontal(|ui| {
        let label = ui.label("Сохранённый ключ");
        ui.add(egui::TextEdit::singleline(&mut app.agent.revealed_key).password(true).interactive(false)).labelled_by(label.id);
        if ui.button("Копировать").clicked() {
            ui.ctx().copy_text(app.agent.revealed_key.clone());
            app.agent.status = "Сохранённый ключ скопирован в буфер обмена.".into();
        }
        if ui.button("Скрыть").clicked() {
            app.agent.revealed_key.clear();
            app.agent.revealed_provider.clear();
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::Services;

    #[test]
    fn pollinations_default_and_catalog_keep_paid_and_specialized_choices() {
        let mut agent = AgentUi::new();
        agent.restore_preferences(&json!({"provider":"pollinations"}));
        assert_eq!(agent.model, POLLINATIONS_DEFAULT_MODEL);
        agent.restore_preferences(&json!({"provider":"pollinations","model":"openai/gpt-5.4-nano"}));
        assert_eq!(agent.model, "openai/gpt-5.4-nano");
        for (cost, expected) in [("free", "Free"), ("paid", "Платная"), ("unknown", "Цена неизвестна")] {
            let model = AgentModel::from_catalog(&json!({"name":"guard", "title":"Guard", "cost":cost, "specialized":true})).unwrap();
            assert_eq!(model.name, "guard");
            assert!(model.label.contains(expected));
            assert!(model.label.contains("специализированная"));
        }
    }

    #[test]
    fn panel_controls_have_accessible_names() {
        let mut app = SheetApp::new(gridcraft_engine::Session::new(), Services::default());
        app.agent.open = true;
        // The harness's default accessibility check rejects any unnamed interactive control.
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(800.0, 600.0)).build_ui_state(
            |ui, app| {
                let ctx = ui.ctx().clone();
                show(app, &ctx);
            },
            app,
        );
        harness.run_steps(2);
        assert!(harness.state().agent.open);
    }

    #[test]
    fn pollinations_settings_ui_saves_and_removes_key_without_persisting_it_in_preferences() {
        use egui_kittest::kittest::Queryable as _;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&requests);
        let inbox = Arc::new(Mutex::new(Vec::new()));
        let response_sink = Arc::clone(&inbox);
        let mut app = SheetApp::new(
            gridcraft_engine::Session::new(),
            Services {
                agent_inbox: Some(Arc::clone(&inbox)),
                agent_send: Some(Box::new(move |value| {
                    response_sink
                        .lock()
                        .unwrap()
                        .push(json!({"id":value["id"], "result":{"provider":"pollinations", "model":"openai", "configured":true}}));
                    sink.lock().unwrap().push(value);
                })),
                ..Default::default()
            },
        );
        app.agent.open = true;
        app.agent.provider = "pollinations".into();
        app.agent.model = "openai".into();
        app.agent.api_key_input = "test-private-key".into();
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 900.0)).build_ui_state(
            |ui, app| {
                app.poll_agent();
                show(app, &ui.ctx().clone());
            },
            app,
        );
        harness.run_steps(3);
        assert_eq!(requests.lock().unwrap()[0]["method"], "agent.settings");
        harness.get_by_label("Сохранить настройки").click();
        harness.run_steps(3);
        let sent = requests.lock().unwrap();
        let saved = sent.iter().find(|request| request["method"] == "agent.configure").unwrap();
        assert_eq!(saved["params"]["apiKey"], "test-private-key");
        drop(sent);
        assert!(harness.state().agent.api_key_input.is_empty());
        assert_eq!(harness.state().agent.preferences(), json!({"provider":"pollinations", "model":"openai"}));
        harness.get_by_label("Удалить сохранённый ключ").click();
        harness.run_steps(3);
        assert!(requests.lock().unwrap().iter().any(|request| request["params"]["clearKey"] == true));
    }

    #[test]
    fn pollinations_run_reply_keeps_settings_and_shows_proposal() {
        let inbox = Arc::new(Mutex::new(Vec::new()));
        let mut app = SheetApp::new(gridcraft_engine::Session::new(), Services { agent_inbox: Some(Arc::clone(&inbox)), ..Default::default() });
        app.agent.provider = "pollinations".into();
        app.agent.configured = true;
        let proposal = json!({"summary":"Set A1", "action":{"command":"cell.set", "params":{"cell":"A1", "input":"42"}}});
        inbox
            .lock()
            .unwrap()
            .push(json!({"id":"agent-1", "result":{"provider":"pollinations", "model":"openai", "applied":false, "proposal":proposal}}));
        app.poll_agent();
        assert_eq!(app.agent.proposal, Some(proposal));
        assert!(app.agent.configured);
    }

    #[test]
    fn explicitly_requested_key_is_temporary_and_never_logged_or_persisted() {
        let inbox = Arc::new(Mutex::new(Vec::new()));
        let mut app = SheetApp::new(gridcraft_engine::Session::new(), Services { agent_inbox: Some(Arc::clone(&inbox)), ..Default::default() });
        app.agent.provider = "pollinations".into();
        inbox.lock().unwrap().push(json!({"id":"agent-1", "result":{"provider":"pollinations", "copiedKey":"synthetic-secret"}}));
        app.poll_agent();
        assert_eq!(app.agent.revealed_key, "synthetic-secret");
        assert_eq!(app.agent.revealed_provider, "pollinations");
        assert!(!app.agent.preferences().to_string().contains("synthetic-secret"));
        assert!(!app.agent.log.iter().any(|line| line.contains("synthetic-secret")));
        app.agent.reset_connection();
        assert!(app.agent.revealed_key.is_empty());
    }

    #[test]
    fn web_settings_wait_for_authentication_and_stop_after_disconnect() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&requests);
        let inbox = Arc::new(Mutex::new(Vec::new()));
        let mut app = SheetApp::new(
            gridcraft_engine::Session::new(),
            Services {
                agent_inbox: Some(Arc::clone(&inbox)),
                agent_connect: Some(Box::new(|_, _| {})),
                agent_send: Some(Box::new(move |value| sink.lock().unwrap().push(value))),
                ..Default::default()
            },
        );
        app.agent.open = true;
        app.agent.provider = "pollinations".into();
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 900.0)).build_ui_state(
            |ui, app| {
                app.poll_agent();
                show(app, &ui.ctx().clone());
            },
            app,
        );
        harness.run_steps(3);
        assert!(requests.lock().unwrap().is_empty());
        inbox.lock().unwrap().push(json!({"result":{"bridgeConnected":true}}));
        harness.run_steps(3);
        assert_eq!(requests.lock().unwrap().len(), 2);
        inbox.lock().unwrap().push(json!({"result":{"bridgeDisconnected":true}}));
        harness.run_steps(3);
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn pollinations_preferences_and_server_status_restore_without_credentials() {
        let inbox = Arc::new(Mutex::new(Vec::new()));
        let mut app = SheetApp::new(gridcraft_engine::Session::new(), Services { agent_inbox: Some(Arc::clone(&inbox)), ..Default::default() });
        app.agent.restore_preferences(&json!({"provider":"pollinations", "model":"openai", "apiKey":"must-not-restore"}));
        inbox.lock().unwrap().push(json!({"id":"agent-1", "result":{"provider":"pollinations", "model":"selected-model", "configured":true}}));
        app.poll_agent();
        assert_eq!(app.agent.model, "selected-model");
        assert!(app.agent.configured);
        assert!(app.agent.api_key_input.is_empty());
        assert!(!app.agent.preferences().to_string().contains("apiKey"));
    }

    #[test]
    fn starts_with_codex_api_settings() {
        let agent = AgentUi::new();
        assert_eq!(agent.provider, "openai");
        assert_eq!(agent.model, "gpt-6.1-sol");
    }

    #[test]
    fn proposal_is_inert_until_explicit_apply_and_can_be_undone() {
        let inbox = Arc::new(Mutex::new(Vec::new()));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let sent_from_ui = Arc::clone(&sent);
        let mut app = SheetApp::new(
            gridcraft_engine::Session::new(),
            Services {
                agent_inbox: Some(Arc::clone(&inbox)),
                agent_send: Some(Box::new(move |value| sent_from_ui.lock().unwrap().push(value))),
                ..Default::default()
            },
        );
        let action = json!({"command": "cell.set", "params": {"cell": "A1", "input": "42"}});
        inbox.lock().unwrap().push(json!({"id": "agent-1", "result": {"proposal": {"summary": "Set A1", "action": action}}}));
        app.poll_agent();
        assert!(sent.lock().unwrap().is_empty());
        assert!(app.agent.proposal.is_some());

        let action = app.agent.proposal.as_ref().unwrap().get("action").cloned().unwrap();
        app.apply_agent_proposal(action);
        assert!(app.agent.proposal.is_some());
        app.agent_send("agent.undo", json!({}));
        let requests = sent.lock().unwrap();
        assert_eq!(requests[0]["method"], "agent.apply");
        assert_eq!(requests[0]["params"]["command"], "cell.set");
        assert_eq!(requests[1]["method"], "agent.undo");
    }

    #[test]
    fn disconnected_panel_does_not_claim_to_have_sent_work() {
        let mut app = SheetApp::new(gridcraft_engine::Session::new(), Services::default());
        app.agent_send("agent.undo", json!({}));
        assert!(app.agent.status.contains("недоступно"));
    }
}
