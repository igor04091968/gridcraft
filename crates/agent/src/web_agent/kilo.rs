//! Private Kilo Gateway credentials. Public RPC replies never contain the key.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::settings;

pub const DEFAULT_MODEL: &str = "stealth/glyph-cluster";

#[derive(Default)]
pub(super) struct Settings {
    key: String,
    pub model: String,
}

impl Settings {
    pub fn load() -> Result<Self, String> {
        Self::read(&settings::config_path()?)
    }

    fn read(path: &Path) -> Result<Self, String> {
        let value = settings::read_config(path)?;
        let model = value.pointer("/kilo/model").and_then(Value::as_str).unwrap_or(DEFAULT_MODEL);
        let key = value.pointer("/kilo/apiKey").and_then(Value::as_str).unwrap_or("");
        if !settings::valid_model(model) || !settings::valid_key(key) {
            return Err("Invalid Kilo Gateway settings.".into());
        }
        Ok(Self { key: key.into(), model: model.into() })
    }

    pub fn key(&self) -> Result<String, String> {
        let key = if self.key.is_empty() { std::env::var("GRIDCRAFT_AGENT_KILO_API_KEY").unwrap_or_default() } else { self.key.clone() };
        if key.is_empty() || !settings::valid_key(&key) {
            return Err("Введите API-ключ Kilo Gateway в настройках AI-агента.".into());
        }
        Ok(key)
    }

    pub fn public(&self) -> Value {
        json!({"provider":"kilo", "model":self.model, "configured":self.key().is_ok(), "storedKey":!self.key.is_empty()})
    }

    fn updated(mut self, params: &Value) -> Result<Self, String> {
        if let Some(model) = params.get("model") {
            let model = model.as_str().filter(|model| settings::valid_model(model)).ok_or("Invalid model name (maximum 128 characters).")?;
            self.model = model.into();
        }
        if let Some(key) = params.get("apiKey") {
            let key = key.as_str().filter(|key| settings::valid_key(key)).ok_or("Invalid API key.")?;
            // Empty input preserves the stored key; removal is a separate explicit command.
            if !key.is_empty() {
                self.key = key.into();
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
        value["kilo"] = json!({"apiKey":self.key, "model":self.model});
        settings::write_config(path, &value)
    }
}

pub(super) fn handle(method: &str, params: &Value) -> Result<Value, String> {
    if params.get("provider").and_then(Value::as_str) != Some("kilo") {
        return Err("These connection settings are for Kilo Gateway.".into());
    }
    let current = Settings::load()?;
    match method {
        "agent.settings" => Ok(current.public()),
        "agent.configure" => {
            let updated = current.updated(params)?;
            updated.write(&settings::config_path()?)?;
            Ok(updated.public())
        }
        "agent.models" => {
            let value = get_models(&current.key()?)?;
            let models = model_list(&value);
            if models.is_empty() {
                return Err("Kilo Gateway returned no chat models available to this API key.".into());
            }
            Ok(json!({"models":models}))
        }
        "agent.test" => {
            let using_draft = params.get("apiKey").and_then(Value::as_str).is_some_and(|key| !key.is_empty());
            let updated = current.updated(params)?;
            let models = get_models(&updated.key()?)?;
            if model_list(&models).is_empty() {
                return Err("Kilo Gateway accepted the API key but returned no chat models for this account.".into());
            }
            Ok(json!({"provider":"kilo", "connectionVerified":true, "usingDraftKey":using_draft}))
        }
        _ => Err("Unknown agent settings method.".into()),
    }
}

fn get_models(key: &str) -> Result<Value, String> {
    let client = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).max_redirects(0).build().new_agent();
    let mut response = client.get("https://api.kilo.ai/v1/models").header("Authorization", &format!("Bearer {key}")).call().map_err(http_error)?;
    response.body_mut().with_config().limit(1024 * 1024).read_json().map_err(|_| "Kilo Gateway returned an invalid model catalogue.".into())
}

fn model_list(value: &Value) -> Vec<Value> {
    let mut models: Vec<Value> = value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(500)
        .filter_map(|model| {
            let id = model.get("id")?.as_str().filter(|id| settings::valid_model(id))?;
            Some(json!({"name":id, "title":id, "cost":"unknown", "specialized":false}))
        })
        .collect();
    models.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    // Prefer the free Glyph Cluster default when present.
    models.sort_by_key(|model| model["name"].as_str() != Some(DEFAULT_MODEL));
    models
}

fn http_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(401 | 403) => "Kilo Gateway отклонил API-ключ или его права доступа.".into(),
        ureq::Error::StatusCode(429) => "Kilo Gateway ограничил частоту запросов. Попробуйте позже.".into(),
        ureq::Error::StatusCode(status) => format!("Проверка Kilo Gateway завершилась с HTTP {status}."),
        _ => "Не удалось подключиться к Kilo Gateway. Проверьте сеть.".into(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn configuration_replies_never_reveal_key_and_blank_input_keeps_it() {
        let config = Settings::default().updated(&json!({"apiKey": "test-private-key", "model": DEFAULT_MODEL})).unwrap();
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
        let list = model_list(&json!({"data":[{"id":DEFAULT_MODEL},{"id":"z-model"}]}));
        assert_eq!(list[0]["name"], DEFAULT_MODEL);
        assert!(!http_error(ureq::Error::StatusCode(401)).contains("Bearer"));
    }
}
