//! Private Pollinations credentials. Public RPC replies never contain the key.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

pub const DEFAULT_MODEL: &str = "stealth/glyph-cluster";
const MAX_CONFIG: u64 = 16 * 1024;

#[derive(Default)]
pub(super) struct Settings {
    key: String,
    pub model: String,
}

pub fn valid_model(model: &str) -> bool {
    !model.trim().is_empty() && model.len() <= 128 && !model.chars().any(char::is_control)
}

fn config_path() -> Result<PathBuf, String> {
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
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self { model: DEFAULT_MODEL.into(), ..Self::default() }),
            Err(_) => return Err("Cannot read agent settings.".into()),
        };
        if !metadata.is_file() || metadata.len() > MAX_CONFIG {
            return Err("Agent settings must be a regular file smaller than 16 KiB.".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err("Agent settings must be private (mode 0600).".into());
            }
        }
        let file = std::fs::File::open(path).map_err(|_| "Cannot read agent settings.")?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::Read::take(file, MAX_CONFIG + 1), &mut bytes).map_err(|_| "Cannot read agent settings.")?;
        if bytes.len() as u64 > MAX_CONFIG {
            return Err("Agent settings are too large.".into());
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid agent settings file.")?;
        let model = value.pointer("/pollinations/model").and_then(Value::as_str).unwrap_or(DEFAULT_MODEL);
        let key = value.pointer("/pollinations/apiKey").and_then(Value::as_str).unwrap_or("");
        if !valid_model(model) || !valid_key(key) {
            return Err("Invalid Pollinations settings.".into());
        }
        Ok(Self { key: key.into(), model: model.into() })
    }

    pub fn key(&self) -> Result<String, String> {
        let key = if self.key.is_empty() { std::env::var("GRIDCRAFT_AGENT_POLLINATIONS_API_KEY").unwrap_or_default() } else { self.key.clone() };
        if key.is_empty() || !valid_key(&key) {
            return Err("Enter a Pollinations API key in the agent settings.".into());
        }
        Ok(key)
    }

    pub fn public(&self) -> Value {
        json!({"provider": "pollinations", "model": self.model, "configured": self.key().is_ok(), "storedKey": !self.key.is_empty()})
    }

    fn updated(mut self, params: &Value) -> Result<Self, String> {
        if let Some(model) = params.get("model") {
            let model = model.as_str().filter(|m| valid_model(m)).ok_or("Invalid model name (maximum 128 characters).")?;
            self.model = model.to_string();
        }
        if let Some(key) = params.get("apiKey") {
            let key = key.as_str().filter(|key| valid_key(key)).ok_or("Invalid API key.")?;
            // Empty entry means keep the saved key; removal is an explicit separate action.
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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| "Cannot create agent settings directory.")?;
        }
        let bytes = serde_json::to_vec_pretty(&json!({"version": 1, "pollinations": {"apiKey": self.key, "model": self.model}}))
            .map_err(|_| "Cannot serialize agent settings.")?;
        let path = path.to_str().ok_or("Agent settings path is not valid UTF-8.")?;
        gridcraft_engine::io::write_file(path, &bytes).map_err(|_| "Cannot save private agent settings.".into())
    }
}

fn valid_key(key: &str) -> bool {
    key.len() <= 4096 && !key.chars().any(char::is_whitespace) && !key.chars().any(char::is_control)
}

pub(super) fn handle(method: &str, params: &Value) -> Result<Value, String> {
    if params.get("provider").and_then(Value::as_str) != Some("pollinations") {
        return Err("These connection settings are for Pollinations.".into());
    }
    let settings = Settings::load()?;
    match method {
        "agent.settings" => Ok(settings.public()),
        "agent.configure" => {
            let settings = settings.updated(params)?;
            settings.write(&config_path()?)?;
            Ok(settings.public())
        }
        "agent.models" => {
            let value = get_json("/text/models", None)?;
            let models = model_list(&value);
            if models.is_empty() {
                return Err("Pollinations returned no compatible text models.".into());
            }
            Ok(json!({"models": models}))
        }
        "agent.test" => {
            // Authentication check without sending workbook data or generating billed text.
            let using_draft = params.get("apiKey").and_then(Value::as_str).is_some_and(|key| !key.is_empty());
            let settings = settings.updated(params)?;
            get_json("/account/key", Some(&settings.key()?))?;
            Ok(json!({"connectionVerified": true, "usingDraftKey":using_draft}))
        }
        _ => Err("Unknown agent settings method.".into()),
    }
}

fn model_list(value: &Value) -> Vec<Value> {
    let mut models: Vec<Value> = value
        .as_array()
        .into_iter()
        .flatten()
        .take(1000)
        .filter_map(|model| {
            let name = model.get("name")?.as_str().filter(|name| valid_model(name))?;
            let compatible = model
                .get("supported_endpoints")
                .and_then(Value::as_array)
                .is_some_and(|endpoints| endpoints.iter().any(|endpoint| endpoint.as_str() == Some("/v1/chat/completions")));
            compatible.then(|| {
                json!({"name": name,
                    "title": model.get("title").and_then(Value::as_str).unwrap_or(name).chars().take(160).collect::<String>(),
                    "cost": model_cost(model),
                    "specialized": model.get("is_specialized").and_then(Value::as_bool).unwrap_or(false)})
            })
        })
        .collect();
    // Free general-purpose models first; keep every compatible paid/specialized entry.
    models.sort_by_key(|model| (model["cost"] != "free", model["specialized"] == true));
    models
}

fn model_cost(model: &Value) -> &'static str {
    let Some(pricing) = model.get("pricing").and_then(Value::as_object) else { return "unknown" };
    if pricing.get("currency").and_then(Value::as_str) != Some("pollen") {
        return "unknown";
    }
    // Pollinations Free entries have currency-only pricing. paid_only=false also
    // occurs on metered Quest models and must never mean free by itself.
    if pricing.len() == 1 && model.get("paid_only").and_then(Value::as_bool) == Some(false) {
        return "free";
    }
    if pricing.iter().any(|(key, value)| {
        key != "currency"
            && value.as_f64().or_else(|| value.as_str().and_then(|v| v.parse::<f64>().ok())).is_some_and(|rate| rate.is_finite() && rate > 0.0)
    }) {
        "paid"
    } else {
        "unknown"
    }
}

fn get_json(path: &str, key: Option<&str>) -> Result<Value, String> {
    let client = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).max_redirects(0).build().new_agent();
    let url = format!("https://gen.pollinations.ai{path}");
    let mut request = client.get(&url);
    if let Some(key) = key {
        request = request.header("Authorization", &format!("Bearer {key}"));
    }
    let mut response = request.call().map_err(http_error)?;
    response.body_mut().with_config().limit(1024 * 1024).read_json().map_err(|_| "Invalid Pollinations response.".into())
}

pub(super) fn http_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(401 | 403) => "Pollinations rejected the API key or its permissions. Check the key in connection settings.".into(),
        ureq::Error::StatusCode(402) => "Pollinations balance is insufficient for this model.".into(),
        ureq::Error::StatusCode(429) => "Pollinations request limit reached. Try again later.".into(),
        ureq::Error::StatusCode(status) => format!("Pollinations request failed (HTTP {status})."),
        _ => "Cannot connect to Pollinations. Check the network and try again.".into(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn configuration_replies_never_reveal_key_and_blank_input_keeps_it() {
        let config = Settings::default().updated(&json!({"apiKey": "test-private-key", "model": "openai"})).unwrap();
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
        let list = model_list(&json!([
            {"name":"image", "supported_endpoints":["/image"]},
            {"name":"text", "title":"Text model", "supported_endpoints":["/v1/chat/completions"]}
        ]));
        assert_eq!(list, vec![json!({"name":"text", "title":"Text model", "cost":"unknown", "specialized":false})]);
        assert!(!http_error(ureq::Error::StatusCode(401)).contains("Bearer"));
    }

    #[test]
    fn free_models_sort_first_without_mistaking_quest_models_for_free() {
        let endpoint = json!(["/v1/chat/completions"]);
        let models = model_list(&json!([
            {"name":"quest", "supported_endpoints":endpoint, "paid_only":false, "pricing":{"currency":"pollen","promptTextTokens":"0.00000015"}},
            {"name":"guard", "supported_endpoints":endpoint, "paid_only":false, "is_specialized":true, "pricing":{"currency":"pollen"}},
            {"name":DEFAULT_MODEL, "supported_endpoints":endpoint, "paid_only":false, "pricing":{"currency":"pollen"}},
            {"name":"missing-pricing", "supported_endpoints":endpoint, "paid_only":false},
            {"name":"invalid-pricing", "supported_endpoints":endpoint, "pricing":{"currency":"pollen","promptTextTokens":"invalid"}},
            {"name":"paid-only", "supported_endpoints":endpoint, "paid_only":true, "pricing":{"currency":"pollen"}}
        ]));
        assert_eq!(models.len(), 6);
        assert_eq!(models[0]["name"], DEFAULT_MODEL);
        assert_eq!(models[0]["cost"], "free");
        assert_eq!(models[1]["name"], "guard");
        assert_eq!(models[1]["specialized"], true);
        assert_eq!(models[2]["cost"], "paid");
        for model in &models[3..] {
            assert_eq!(model["cost"], "unknown");
        }
    }

    #[test]
    fn fresh_settings_use_free_default_and_preserve_explicit_paid_selection() {
        let dir = std::env::temp_dir().join(format!("gridcraft-agent-free-default-{}", std::process::id()));
        let path = dir.join("agent.json");
        let _ = std::fs::remove_dir_all(&dir);
        let fresh = Settings::read(&path).unwrap();
        assert_eq!(fresh.model, DEFAULT_MODEL);
        fresh.updated(&json!({"model":"openai/gpt-5.4-nano", "apiKey":"private-test-key"})).unwrap().write(&path).unwrap();
        let reloaded = Settings::read(&path).unwrap().updated(&json!({"apiKey":""})).unwrap();
        assert_eq!(reloaded.model, "openai/gpt-5.4-nano");
        assert_eq!(reloaded.key, "private-test-key");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn private_settings_survive_reload_and_removal() {
        let dir = std::env::temp_dir().join(format!("gridcraft-agent-settings-{}", std::process::id()));
        let path = dir.join("agent.json");
        let config = Settings::default().updated(&json!({"apiKey":"private-test-key", "model":"openai"})).unwrap();
        config.write(&path).unwrap();
        let loaded = Settings::read(&path).unwrap();
        assert_eq!(loaded.key, "private-test-key");
        assert_eq!(loaded.model, "openai");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        }
        loaded.updated(&json!({"clearKey":true})).unwrap().write(&path).unwrap();
        assert!(Settings::read(&path).unwrap().key.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
