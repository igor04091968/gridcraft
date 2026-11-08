//! Private Cohere credentials. Public RPC replies never contain the key.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::settings;

pub const DEFAULT_MODEL: &str = "cohere/north-mini-code:free";

#[derive(Default)]
pub(super) struct Settings {
    key: String,
    pub model: String,
}

pub(super) fn config_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("GRIDCRAFT_AGENT_CONFIG_PATH") {
        return Ok(PathBuf::from(path));
    }
    let dir = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support/GridCraft"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("GridCraft"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|dir| dir.join("gridcraft"))
    };
    dir.map(|dir| dir.join("agent.json")).ok_or_else(|| "Agent settings directory is unavailable.".into())
}

impl Settings {
    pub fn load() -> Result<Self, String> {
        Self::read(&config_path()?)
    }

    fn read(path: &Path) -> Result<Self, String> {
        let value = settings::read_config(path)?;
        let model = value.pointer("/cohere/model").and_then(Value::as_str).unwrap_or(DEFAULT_MODEL);
        let key = value.pointer("/cohere/apiKey").and_then(Value::as_str).unwrap_or("");
        if !settings::valid_model(model) || !settings::valid_key(key) {
            return Err("Invalid Cohere settings.".into());
        }
        Ok(Self { key: key.to_string(), model: model.to_string() })
    }

    pub fn key(&self) -> Result<String, String> {
        if self.key.is_empty() {
            return Err("Cohere API key is missing.".into());
        }
        Ok(self.key.clone())
    }
}

fn valid_model(model: &str) -> bool {
    !model.trim().is_empty() && model.len() <= 128 && !model.chars().any(char::is_control)
}

fn valid_key(key: &str) -> bool {
    !key.trim().is_empty() && !key.chars().any(char::is_control)
}

/// Public validator used by other providers to check Cohere model names.
pub fn valid_model_public(model: &str) -> bool {
    valid_model(model)
}

/// Public completion function used by other providers to call Cohere.
pub fn completion(url: &str, key: &str, body: &Value) -> Result<Value, String> {
    let client = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(90)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut response = client.post(url).header("Authorization", &format!("Bearer {key}")).send_json(body).map_err(settings::http_error)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let error = response.body_mut().with_config().limit(64 * 1024).read_json::<Value>().unwrap_or(Value::Null);
        return Err(cohere_error(status, &error));
    }
    response.body_mut().with_config().limit(1024 * 1024).read_json().map_err(|_| "Некорректный ответ Cohere.".into())
}

fn cohere_error(status: u16, error: &Value) -> String {
    if status != 400 {
        return settings::http_error(ureq::Error::StatusCode(status));
    }
    let details = ["/error/message", "/error/code", "/error/type", "/message", "/code", "/error"]
        .into_iter()
        .filter_map(|path| error.pointer(path).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if ["invalid model", "unknown model", "model not found", "unsupported model", "model_not_found", "invalid_model"]
        .iter()
        .any(|reason| details.contains(reason))
    {
        "Cohere не распознал выбранную модель (HTTP 400). Обновите список моделей и выберите модель из каталога.".into()
    } else if ["context_length_exceeded", "maximum context length", "context window", "too many tokens", "input is too long", "prompt is too long"]
        .iter()
        .any(|reason| details.contains(reason))
    {
        "Контекст превышает лимит модели Cohere (HTTP 400). Укажите меньший диапазон ячеек, сократите задачу или выберите модель с большим контекстом.".into()
    } else if ["unsupported parameter", "unsupported_parameter", "unknown parameter", "not supported", "unrecognized parameter"]
        .iter()
        .any(|reason| details.contains(reason))
    {
        "Выбранная модель Cohere не поддерживает параметры запроса (HTTP 400). Попробуйте другую модель из каталога.".into()
    } else {
        "Cohere отклонил запрос (HTTP 400). Проверьте выбранную модель и диапазон ячеек; точная причина не распознана. Книга не изменена.".into()
    }
}

pub(super) fn handle(method: &str, params: &Value) -> Result<Value, String> {
    if params.get("provider").and_then(Value::as_str) != Some("cohere") {
        return Err("These connection settings are for Cohere.".into());
    }
    let current = Settings::load()?;
    match method {
        "agent.settings" => Ok(current.public()),
        "agent.configure" => {
            let updated = current.updated(params)?;
            updated.write(&config_path()?)?;
            Ok(updated.public())
        }
        "agent.models" => {
            let value = get_models(&current.key()?)?;
            let models = model_list(&value);
            if models.is_empty() {
                return Err("Cohere returned no chat models available to this API key.".into());
            }
            Ok(json!({"models": models}))
        }
        "agent.test" => {
            let using_draft = params.get("apiKey").and_then(Value::as_str).is_some_and(|key| !key.is_empty());
            let updated = current.updated(params)?;
            let models = get_models(&updated.key()?)?;
            if model_list(&models).is_empty() {
                return Err("Cohere accepted the API key but returned no chat models for this account.".into());
            }
            Ok(json!({"provider": "cohere", "connectionVerified": true, "usingDraftKey": using_draft}))
        }
        _ => Err("Unknown agent settings method.".into()),
    }
}

impl Settings {
    fn updated(mut self, params: &Value) -> Result<Self, String> {
        if let Some(model) = params.get("model") {
            let model = model.as_str().filter(|model| valid_model(model)).ok_or("Invalid model name (maximum 128 characters).")?;
            self.model = model.to_string();
        }
        if let Some(key) = params.get("apiKey") {
            let key = key.as_str().unwrap_or("");
            if !key.is_empty() && !valid_key(key) {
                return Err("Invalid API key.".into());
            }
            if !key.is_empty() {
                self.key = key.to_string();
            }
        }
        if params.get("clearKey") == Some(&Value::Bool(true)) {
            self.key.clear();
        }
        Ok(self)
    }

    fn write(&self, path: &Path) -> Result<(), String> {
        let mut value = settings::read_config(path)?;
        value["version"] = json!(1);
        value["cohere"] = json!({"apiKey": self.key, "model": self.model});
        settings::write_config(path, &value)
    }

    pub fn public(&self) -> Value {
        json!({"provider": "cohere", "model": self.model, "configured": self.key().is_ok(), "storedKey": !self.key.is_empty()})
    }
}

fn get_models(key: &str) -> Result<Value, String> {
    let client = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(20))).max_redirects(0).build().new_agent();
    let mut response = client.get("https://api.kilo.ai/v1/models").header("Authorization", &format!("Bearer {key}")).call().map_err(http_error)?;
    response.body_mut().with_config().limit(1024 * 1024).read_json().map_err(|_| "Cohere returned an invalid model catalogue.".into())
}

fn model_list(value: &Value) -> Vec<Value> {
    let mut models: Vec<Value> = value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(500)
        .filter_map(|model| {
            let id = model.get("id")?.as_str().filter(|id| valid_model(id))?;
            (model.pointer("/capabilities/completion_chat").and_then(Value::as_bool) == Some(true))
                .then(|| json!({"name": id, "title": id, "cost": "unknown", "specialized": false}))
        })
        .collect();
    models.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    models
}

fn http_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(401 | 403) => "Cohere отклонил API-ключ или его права доступа.".into(),
        ureq::Error::StatusCode(429) => "Cohere ограничил частоту запросов. Попробуйте позже.".into(),
        ureq::Error::StatusCode(status) => format!("Проверка Cohere завершилась с HTTP {status}."),
        _ => "Не удалось подключиться к Cohere. Проверьте сеть.".into(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use serde_json::json;

    #[test]
    fn configuration_replies_never_reveal_key_and_blank_input_keeps_it() {
        let config = Settings::default().updated(&json!({"apiKey": "test-private-key", "model": "cohere/north-mini-code:free"})).unwrap();
        let config = config.updated(&json!({"apiKey": "", "model": "other-model"})).unwrap();
        assert_eq!(config.key, "test-private-key");
        assert!(!config.public().to_string().contains("test-private-key"));
        assert!(!config.public().to_string().contains("apiKey"));
        assert!(config.updated(&json!({"clearKey": true})).unwrap().key.is_empty());
    }

    #[test]
    fn rejects_hostile_settings_and_filters_model_catalog() {
        for params in [json!({"apiKey": "bad\nkey"}), json!({"model": ""}), json!({"model": "x".repeat(129)})] {
            assert!(Settings::default().updated(&params).is_err());
        }
        let list = model_list(&json!({"data": [
            {"id": "image", "capabilities": {"completion_chat": false}},
            {"id": "text", "capabilities": {"completion_chat": true}}
        ]}));
        assert_eq!(list, vec![json!({"name": "text", "title": "text", "cost": "unknown", "specialized": false})]);
        assert!(!http_error(ureq::Error::StatusCode(401)).contains("Bearer"));
    }
}
