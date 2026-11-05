use std::io::{Read, Write};
#[cfg(test)]
use std::net::TcpStream;
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

mod pollinations;
mod settings;

pub fn handle<S: Read + Write>(ws: &mut WebSocket<S>, request: &Value) -> Result<bool, String> {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    if !matches!(
        method,
        "agent.run" | "agent.apply" | "agent.undo" | "agent.settings" | "agent.configure" | "agent.models" | "agent.test" | "agent.copyKey"
    ) {
        return Ok(false);
    }
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method {
        "agent.run" => propose(ws, &params),
        "agent.settings" | "agent.configure" | "agent.models" | "agent.test" => settings::handle(method, &params),
        "agent.copyKey" => copy_provider_key(&params),
        "agent.apply" => apply(ws, params),
        "agent.undo" => call_browser(ws, 9003, "agent.undoReviewed", json!({})),
        _ => return Ok(false),
    };
    let response = match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": error}}),
    };
    ws.send(Message::text(response.to_string())).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Return one explicitly requested provider credential to the authenticated settings UI.
/// This method is never called by model requests and its response must not be logged.
fn copy_provider_key(params: &Value) -> Result<Value, String> {
    let provider = params.get("provider").and_then(Value::as_str).ok_or("Provider is required.")?;
    let key = if provider == "pollinations" {
        settings::Settings::load()?.key()?
    } else {
        let profile = ProviderProfile::for_name(provider)?;
        if !matches!(provider, "openai" | "kimi" | "deepseek") {
            return Err("This provider key cannot be copied here.".into());
        }
        env(profile.api_key_env)?
    };
    Ok(json!({"copiedKey":key,"provider":provider}))
}

fn apply<S: Read + Write>(ws: &mut WebSocket<S>, mut action: Value) -> Result<Value, String> {
    validate_action(&json!({"action": action}))?;
    required(&action, "command")?;
    if !action.get("context").is_some_and(Value::is_object) {
        return Err("The proposal has no workbook context. Request a new proposal.".into());
    }
    if let Some(params) = action.get_mut("params").and_then(Value::as_object_mut)
        && params.get("range").and_then(Value::as_str).is_some_and(|range| range.trim().is_empty())
    {
        params.remove("range");
    }
    call_browser(ws, 9002, "agent.applyReviewed", action)
}

fn required<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params.get(key).and_then(Value::as_str).filter(|v| !v.trim().is_empty()).ok_or_else(|| format!("missing `{key}`"))
}

fn propose<S: Read + Write>(ws: &mut WebSocket<S>, params: &Value) -> Result<Value, String> {
    let task = required(params, "task")?;
    if task.len() > 16_000 {
        return Err("task is too long (16000 characters maximum)".into());
    }
    let provider = params.get("provider").and_then(Value::as_str).unwrap_or("legacy");
    let profile = ProviderProfile::for_name(provider)?;
    let saved = if provider == "pollinations" { Some(settings::Settings::load()?) } else { None };
    let requested_model = params.get("model").and_then(Value::as_str).filter(|m| !m.trim().is_empty() && *m != "default");
    let model = requested_model.map(str::to_string).unwrap_or_else(|| {
        if let Some(saved) = &saved {
            saved.model.clone()
        } else if provider == "legacy" {
            std::env::var("GRIDCRAFT_AGENT_MODEL").ok().filter(|m| !m.trim().is_empty()).unwrap_or_else(|| profile.default_model.into())
        } else {
            profile.default_model.into()
        }
    });
    if !settings::valid_model(&model) {
        return Err("Invalid model name (maximum 128 characters).".into());
    }
    let key = match &saved {
        Some(saved) => saved.key()?,
        None => env(profile.api_key_env)?,
    };
    let steps = params.get("maxSteps").and_then(Value::as_u64).unwrap_or(4);
    if !(1..=20).contains(&steps) {
        return Err("maxSteps must be from 1 to 20".into());
    }
    let mut read_params = json!({});
    if let Some(range) = params.get("range").and_then(Value::as_str).filter(|range| !range.trim().is_empty()) {
        read_params["range"] = json!(range);
    }
    let snapshot = call_browser(ws, 9000, "agent.snapshot", read_params)?;
    let expected = snapshot.get("context").filter(|value| value.is_object()).ok_or("workbook snapshot has no context")?.clone();
    let base = std::env::var(profile.base_url_env).unwrap_or_else(|_| profile.base_url.into());
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let system = "You are GridCraft's spreadsheet agent. Return exactly one JSON object per turn: {\"summary\":string,\"inspect\":null|{\"range\":\"A1:B8\"},\"action\":null|{\"command\":string,\"params\":object},\"undo\":string}. Use inspect to request a bounded read of the ACTIVE sheet before deciding; the next turn will contain that range's values. You may inspect multiple ranges, one per turn, within the supplied step budget. Never inspect whole rows/columns or more than 5000 cells at once. When ready, set inspect:null and return either one action or action:null. Propose at most one safe engine command. Use cell.set for one cell or range.setValues for a rectangular 2-D array. To merge cells, use home.mergeCells with an A1 range; omit range only to use the current selection. To move whole columns, use sheet.moveColumns with cols as an A1 whole-column range and before as a zero-based boundary. Never send an empty range. Never propose file.save, file.open, app.quit, macros or external links. The user reviews every action before execution. Never claim an action was applied or verified; GridCraft will apply and verify it separately.";
    let initial = serde_json::to_string(&json!({"task": task, "workbook": snapshot.get("workbook"), "range": snapshot.get("range")}))
        .map_err(|e| e.to_string())?;
    let initial: String = initial.chars().take(60_000).collect();
    let mut messages = vec![json!({"role":"system","content":system}), json!({"role":"user","content":initial})];
    let mut inspections = Vec::new();
    let mut inspected_chars = 0usize;
    let mut repair_used = false;

    for step in 0..steps {
        let body = json!({"model":model,"max_completion_tokens":1800,"messages":messages.clone()});
        let value = complete(provider, &url, &key, body)?;
        let content = message_content(&value);
        let content = content.as_deref().unwrap_or("");
        let mut proposal = match parse_proposal(content) {
            Ok(proposal) => proposal,
            Err(error) if !repair_used && step + 1 < steps => {
                repair_used = true;
                messages.push(json!({"role":"assistant","content":content}));
                messages.push(json!({"role":"user","content":format!("Your previous response was unusable ({error}). Return a complete, non-empty JSON object matching the required schema. Do not use tools or markdown fences.")}));
                continue;
            }
            Err(_) => return Err("Модель не вернула корректный ответ после повторного запроса. Книга не изменена; отправьте задачу ещё раз или выберите другую модель.".into()),
        };
        validate_proposal(&proposal)?;

        if let Some(inspect) = proposal.get("inspect").filter(|value| !value.is_null()) {
            if step + 1 >= steps {
                return Err("Агенту не хватило шагов для анализа. Увеличьте лимит шагов и повторите задачу.".into());
            }
            let range = match inspection_range(inspect) {
                Ok(range) => range,
                Err(error) if step + 1 < steps => {
                    messages.push(json!({"role":"assistant","content":content}));
                    messages.push(json!({"role":"user","content":format!("Inspection request rejected: {error}. Choose a smaller valid A1 range on the active sheet.")}));
                    continue;
                }
                Err(error) => return Err(error),
            };
            let observed = match call_browser(ws, 9000, "agent.snapshot", json!({"range": range})) {
                Ok(observed) => observed,
                Err(error) if step + 1 < steps => {
                    messages.push(json!({"role":"assistant","content":content}));
                    messages.push(json!({"role":"user","content":format!("Read-only inspection failed: {error}. Try a smaller range or finish with the evidence already available.")}));
                    continue;
                }
                Err(error) => return Err(error),
            };
            if observed.get("context") != Some(&expected) {
                return Err("Открытая книга или выделение изменились во время анализа. Запустите задачу заново.".into());
            }
            inspections.push(json!({"range":range,"summary":proposal.get("summary").cloned().unwrap_or(Value::Null)}));
            let observation = serde_json::to_string(&json!({"range":range,"data":observed.get("range")})).map_err(|e| e.to_string())?;
            let remaining = 60_000usize.saturating_sub(inspected_chars);
            if remaining == 0 {
                return Err("Агент исчерпал лимит данных для анализа. Сузьте диапазоны и повторите задачу.".into());
            }
            let bounded_observation: String = observation.chars().take(remaining.min(24_000)).collect();
            inspected_chars = inspected_chars.saturating_add(bounded_observation.chars().count());
            messages.push(json!({"role":"assistant","content":content}));
            messages.push(json!({"role":"user","content":format!("Read-only inspection result for {range}: {bounded_observation}{} Continue using this evidence.", if bounded_observation.len() < observation.len() { " [data truncated]" } else { "" })}));
            continue;
        }

        validate_action(&proposal)?;
        // The model cannot supply or replace the context captured before its request.
        if let Some(action) = proposal.get_mut("action").and_then(Value::as_object_mut) {
            action.insert("context".into(), expected);
        }
        return Ok(
            json!({"proposal":proposal,"provider":provider,"model":model,"maxSteps":steps,"stepsUsed":step + 1,"inspections":inspections,"applied":false}),
        );
    }
    Err("Агент достиг лимита шагов до подготовки предложения. Увеличьте лимит и повторите задачу.".into())
}

fn complete(provider: &str, url: &str, key: &str, body: Value) -> Result<Value, String> {
    if provider == "pollinations" {
        return pollinations::completion(url, key, &body);
    }
    let client = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(90))).build().new_agent();
    let mut response =
        client.post(url).header("Authorization", &format!("Bearer {key}")).send_json(body).map_err(|e| format!("agent request failed: {e}"))?;
    response.body_mut().with_config().limit(1024 * 1024).read_json().map_err(|e| format!("invalid agent response: {e}"))
}

fn message_content(response: &Value) -> Option<String> {
    let content = response.pointer("/choices/0/message/content")?;
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text = parts.iter().filter_map(|part| part.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn validate_proposal(proposal: &Value) -> Result<(), String> {
    if proposal.get("summary").and_then(Value::as_str).is_none() {
        return Err("agent proposal has no summary".into());
    }
    if proposal.get("inspect").is_some_and(|inspect| !inspect.is_null()) && proposal.get("action").is_some_and(|action| !action.is_null()) {
        return Err("agent response cannot inspect and propose a write in the same step".into());
    }
    Ok(())
}

fn inspection_range(inspect: &Value) -> Result<&str, String> {
    let range = inspect.get("range").and_then(Value::as_str).filter(|range| !range.trim().is_empty()).ok_or("inspection needs an A1 range")?;
    if range.len() > 128 || range.contains('!') {
        return Err("inspection range must be an A1 range on the active sheet (128 characters maximum)".into());
    }
    let parsed = gridcraft_engine::core::RangeRef::parse(range).ok_or("inspection range is not a valid A1 range")?;
    if parsed.is_full_rows() || parsed.is_full_cols() || parsed.count() > 5_000 {
        return Err("inspection is limited to 5000 cells; request a smaller range".into());
    }
    Ok(range)
}

struct ProviderProfile {
    base_url: &'static str,
    base_url_env: &'static str,
    api_key_env: &'static str,
    default_model: &'static str,
}

impl ProviderProfile {
    fn for_name(provider: &str) -> Result<Self, String> {
        match provider {
            "openai" => Ok(Self {
                base_url: "https://api.openai.com/v1",
                base_url_env: "GRIDCRAFT_AGENT_OPENAI_BASE_URL",
                api_key_env: "GRIDCRAFT_AGENT_OPENAI_API_KEY",
                default_model: "gpt-6.1-sol",
            }),
            "kimi" => Ok(Self {
                base_url: "https://api.moonshot.ai/v1",
                base_url_env: "GRIDCRAFT_AGENT_KIMI_BASE_URL",
                api_key_env: "GRIDCRAFT_AGENT_KIMI_API_KEY",
                default_model: "kimi-k3.1-pro",
            }),
            "deepseek" => Ok(Self {
                base_url: "https://plusvibeapi.ru/v1",
                base_url_env: "GRIDCRAFT_AGENT_PLUSVIBE_BASE_URL",
                api_key_env: "GRIDCRAFT_AGENT_PLUSVIBE_API_KEY",
                default_model: "deepseek-v4-flash-0731:free",
            }),
            "pollinations" => Ok(Self {
                base_url: "https://gen.pollinations.ai/v1",
                base_url_env: "GRIDCRAFT_AGENT_POLLINATIONS_BASE_URL",
                api_key_env: "GRIDCRAFT_AGENT_POLLINATIONS_API_KEY",
                default_model: settings::DEFAULT_MODEL,
            }),
            "legacy" => Ok(Self {
                base_url: "http://127.0.0.1:11434/v1",
                base_url_env: "GRIDCRAFT_AGENT_BASE_URL",
                api_key_env: "GRIDCRAFT_AGENT_API_KEY",
                default_model: "default",
            }),
            _ => Err(format!("unsupported agent provider: {provider}")),
        }
    }
}

fn env(key: &str) -> Result<String, String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty()).ok_or_else(|| format!("server is not configured: set {key}"))
}

fn parse_proposal(content: &str) -> Result<Value, String> {
    let trimmed = content.trim();
    let json_text =
        trimmed.strip_prefix("```json").or_else(|| trimmed.strip_prefix("```")).and_then(|v| v.strip_suffix("```")).unwrap_or(trimmed).trim();
    let value: Value = serde_json::from_str(json_text).map_err(|e| format!("agent did not return valid JSON: {e}"))?;
    if value.get("summary").and_then(Value::as_str).is_none() {
        return Err("agent proposal has no summary".into());
    }
    Ok(value)
}

fn validate_action(proposal: &Value) -> Result<(), String> {
    let Some(action) = proposal.get("action") else { return Ok(()) };
    if action.is_null() {
        return Ok(());
    }
    let command = action.get("command").and_then(Value::as_str).ok_or_else(|| "agent action has no command".to_string())?;
    if !matches!(command, "cell.set" | "range.setValues" | "home.bold" | "home.italic" | "home.underline" | "home.mergeCells" | "sheet.moveColumns") {
        return Err(format!("agent proposed a command not allowed by the web review policy: {command}"));
    }
    let params = action.get("params").filter(|value| value.is_object()).ok_or_else(|| "agent action params must be an object".to_string())?;
    if command == "sheet.moveColumns" {
        if params.get("cols").and_then(Value::as_str).is_none_or(|value| value.trim().is_empty()) {
            return Err("sheet.moveColumns needs a whole-column A1 range in cols".into());
        }
        if params.get("before").and_then(Value::as_u64).is_none() {
            return Err("sheet.moveColumns needs a zero-based integer before".into());
        }
    }
    if command == "home.mergeCells" && params.get("range").is_some_and(|value| !value.is_string()) {
        return Err("home.mergeCells range must be an A1 range string".into());
    }
    Ok(())
}

#[allow(clippy::disallowed_methods)] // Native-only backend; the bounded wait never executes on WASM.
fn call_browser<S: Read + Write>(ws: &mut WebSocket<S>, id: u64, method: &str, params: Value) -> Result<Value, String> {
    let request = json!({"id": id, "method": method, "params": params});
    ws.send(Message::text(request.to_string())).map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + Duration::from_secs(110);
    for _ in 0..256 {
        if std::time::Instant::now() >= deadline {
            return Err("Browser reply timed out.".into());
        }
        match ws.read() {
            Ok(Message::Text(text)) => {
                let response: Value = serde_json::from_str(&text).map_err(|e| format!("invalid browser response: {e}"))?;
                if response.get("id").and_then(Value::as_u64) != Some(id) {
                    if response.get("method").is_some() {
                        ws.send(Message::text(json!({"id":response.get("id"),"error":{"code":-32000,"message":"Agent is busy; wait for the current request to finish."}}).to_string())).map_err(|e| e.to_string())?;
                    }
                    continue;
                }
                if response.get("ok") == Some(&Value::Bool(true)) {
                    return Ok(response.get("result").cloned().unwrap_or(Value::Null));
                }
                return Err(response.get("error").and_then(Value::as_str).unwrap_or("browser command failed").to_string());
            }
            Ok(Message::Ping(data)) => ws.send(Message::Pong(data)).map_err(|e| e.to_string())?,
            Ok(Message::Close(_)) => return Err("browser closed the control channel".into()),
            Ok(_) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("Too many unrelated messages while waiting for the browser.".into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn socket_pair() -> (WebSocket<TcpStream>, WebSocket<TcpStream>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            tungstenite::accept(stream).unwrap()
        });
        let stream = TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (client, _) = tungstenite::client(format!("ws://{address}"), stream).unwrap();
        (server.join().unwrap(), client)
    }

    fn read_json(ws: &mut WebSocket<TcpStream>) -> Value {
        let Message::Text(text) = ws.read().unwrap() else { panic!("expected text") };
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn invalid_apply_returns_rpc_error_and_keeps_connection_usable() {
        let (mut server, mut browser) = socket_pair();
        for params in [json!({"command": "app.quit", "params": {}}), json!({"command": "cell.set", "params": {"cell": "A1", "input": "42"}})] {
            assert!(handle(&mut server, &json!({"id": "agent-test", "method": "agent.apply", "params": params})).unwrap());
            let reply = read_json(&mut browser);
            assert_eq!(reply["id"], "agent-test");
            assert!(reply["error"]["message"].is_string());
        }
    }

    #[test]
    fn apply_forwards_context_to_atomic_ui_guard_and_returns_stale_error() {
        let (mut server, mut browser) = socket_pair();
        let context = json!({"doc": 1, "revision": 2, "sheet": 0, "selection": {}});
        let expected = context.clone();
        let peer = std::thread::spawn(move || {
            let request = read_json(&mut browser);
            assert_eq!(request["method"], "agent.applyReviewed");
            assert_eq!(request["params"]["context"], expected);
            assert!(request["params"]["params"].get("range").is_none());
            browser.send(Message::text(json!({"id": request["id"], "ok": false, "error": "workbook changed"}).to_string())).unwrap();
            let reply = read_json(&mut browser);
            assert_eq!(reply["id"], "agent-test");
            assert_eq!(reply["error"]["message"], "workbook changed");
        });
        handle(
            &mut server,
            &json!({"id": "agent-test", "method": "agent.apply", "params": {"command": "home.bold", "params": {"range": " "}, "context": context}}),
        )
        .unwrap();
        peer.join().unwrap();
    }

    #[test]
    fn undo_uses_only_guarded_agent_history() {
        let (mut server, mut browser) = socket_pair();
        let peer = std::thread::spawn(move || {
            let request = read_json(&mut browser);
            assert_eq!(request["method"], "agent.undoReviewed");
            assert_eq!(request["params"], json!({}));
            browser.send(Message::text(json!({"id": request["id"], "ok": true, "result": {"undone": true}}).to_string())).unwrap();
            assert_eq!(read_json(&mut browser)["result"]["undone"], true);
        });
        handle(&mut server, &json!({"id": "agent-test", "method": "agent.undo"})).unwrap();
        peer.join().unwrap();
    }

    #[test]
    fn provider_profiles_have_separate_server_credentials_and_defaults() {
        let openai = ProviderProfile::for_name("openai").unwrap();
        assert_eq!(openai.base_url, "https://api.openai.com/v1");
        assert_eq!(openai.api_key_env, "GRIDCRAFT_AGENT_OPENAI_API_KEY");
        assert_eq!(openai.default_model, "gpt-6.1-sol");
        let kimi = ProviderProfile::for_name("kimi").unwrap();
        assert_eq!(kimi.base_url, "https://api.moonshot.ai/v1");
        assert_eq!(kimi.api_key_env, "GRIDCRAFT_AGENT_KIMI_API_KEY");
        assert_eq!(kimi.default_model, "kimi-k3.1-pro");
        let deepseek = ProviderProfile::for_name("deepseek").unwrap();
        assert_eq!(deepseek.base_url, "https://plusvibeapi.ru/v1");
        assert_eq!(deepseek.base_url_env, "GRIDCRAFT_AGENT_PLUSVIBE_BASE_URL");
        assert_eq!(deepseek.api_key_env, "GRIDCRAFT_AGENT_PLUSVIBE_API_KEY");
        assert_eq!(deepseek.default_model, "deepseek-v4-flash-0731:free");
        let pollinations = ProviderProfile::for_name("pollinations").unwrap();
        assert_eq!(pollinations.base_url, "https://gen.pollinations.ai/v1");
        assert_eq!(pollinations.api_key_env, "GRIDCRAFT_AGENT_POLLINATIONS_API_KEY");
        assert_eq!(pollinations.default_model, "stealth/glyph-cluster");
        assert!(ProviderProfile::for_name("https://attacker.example").is_err());
    }

    #[test]
    fn copy_key_endpoint_refuses_legacy_provider() {
        assert_eq!(copy_provider_key(&json!({"provider":"legacy"})).unwrap_err(), "This provider key cannot be copied here.");
        assert!(copy_provider_key(&json!({"provider":"unknown"})).is_err());
    }

    #[test]
    fn parses_fenced_proposals_and_rejects_unreviewed_commands() {
        let value = parse_proposal("```json\n{\"summary\":\"Set A1\",\"action\":{\"command\":\"cell.set\",\"params\":{\"cell\":\"A1\",\"input\":\"42\"}},\"undo\":\"Undo\"}\n```").unwrap();
        validate_action(&value).unwrap();
        let bad = json!({"summary":"Quit", "action":{"command":"app.quit","params":{}}});
        assert!(validate_action(&bad).is_err());
    }
}
