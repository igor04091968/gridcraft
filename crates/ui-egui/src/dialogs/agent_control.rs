use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};

use super::Dialog;

const SERVER: &str = "gridcraft-app";

#[derive(Clone, Copy, PartialEq)]
enum Client {
    Kimi,
    Codex,
}

impl Client {
    fn parse(value: Option<&Json>) -> Self {
        match value.and_then(Json::as_str) {
            Some("codex") => Self::Codex,
            _ => Self::Kimi,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Kimi => "kimi",
            Self::Codex => "codex",
        }
    }
}

pub(super) fn show(ui: &mut egui::Ui, d: &mut Dialog, lang: crate::i18n::Language) {
    ui.label(lang.tr("Connect an AI agent to this desktop app. The agent starts GridCraft automatically when needed."));
    ui.add_space(8.0);
    egui::Grid::new("agent-control-settings").num_columns(2).show(ui, |ui| {
        ui.label(lang.tr("Agent:"));
        let client = Client::parse(d.values.get("client"));
        craft_ui::ComboBox::from_id_salt("agent-control-client")
            .selected_text(match client {
                Client::Kimi => "Kimi Code",
                Client::Codex => "Codex",
            })
            .show_ui(ui, |ui| {
                for (id, label) in [("kimi", "Kimi Code"), ("codex", "Codex")] {
                    if ui.selectable_label(client.id() == id, label).clicked() {
                        d.values.insert("client".into(), json!(id));
                        d.values.remove("status");
                    }
                }
            });
        ui.end_row();
        ui.label(lang.tr("Scope:"));
        let global = d.values.get("global").and_then(Json::as_bool).unwrap_or(false);
        craft_ui::ComboBox::from_id_salt("agent-control-scope").selected_text(lang.tr(if global { "All projects" } else { "This project" })).show_ui(
            ui,
            |ui| {
                for (value, label) in [(false, "This project"), (true, "All projects")] {
                    if ui.selectable_label(global == value, lang.tr(label)).clicked() {
                        d.values.insert("global".into(), json!(value));
                        d.values.remove("status");
                    }
                }
            },
        );
        ui.end_row();
        ui.label(lang.tr("Control port:"));
        let mut port = d.values.get("port").and_then(Json::as_str).unwrap_or("7979").to_string();
        if ui.text_edit_singleline(&mut port).changed() {
            d.values.insert("port".into(), json!(port));
        }
        ui.end_row();
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(lang.tr("Install integration")).clicked() {
            let result = install_from_dialog(d);
            d.values.insert("status".into(), json!(message(result)));
        }
        if ui.button(lang.tr("Remove integration")).clicked() {
            let result = remove_from_dialog(d);
            d.values.insert("status".into(), json!(message(result)));
        }
    });
    if let Some(status) = d.values.get("status").and_then(Json::as_str) {
        ui.add_space(6.0);
        ui.label(status);
    }
    ui.add_space(8.0);
    ui.label(lang.tr("After installation, start a new agent session. Keep other MCP servers configured in the same file: they are preserved."));
    ui.add_space(4.0);
    ui.monospace("gridcraft-cli mcp --connect 7979 --launch gridcraft");
}

fn message(result: Result<PathBuf, String>) -> String {
    match result {
        Ok(path) => format!("Updated {}. Start a new agent session to connect.", path.display()),
        Err(error) => error,
    }
}

fn install_from_dialog(d: &Dialog) -> Result<PathBuf, String> {
    let client = Client::parse(d.values.get("client"));
    let global = d.values.get("global").and_then(Json::as_bool).unwrap_or(false);
    let port = d.values.get("port").and_then(Json::as_str).unwrap_or("7979");
    validate_port(port)?;
    let app = std::env::current_exe().map_err(|e| format!("Cannot locate the GridCraft app: {e}"))?;
    let cli = executable_sibling(&app, "gridcraft-cli");
    if !cli.is_file() {
        return Err(format!("gridcraft-cli was not found beside the app: {}. Install both binaries first.", cli.display()));
    }
    let path = config_path(client, global)?;
    install(&path, client, &cli, &app, port)?;
    Ok(path)
}

fn remove_from_dialog(d: &Dialog) -> Result<PathBuf, String> {
    let client = Client::parse(d.values.get("client"));
    let global = d.values.get("global").and_then(Json::as_bool).unwrap_or(false);
    let path = config_path(client, global)?;
    remove(&path, client)?;
    Ok(path)
}

fn validate_port(port: &str) -> Result<(), String> {
    match port.parse::<u16>() {
        Ok(0) | Err(_) => Err("Control port must be a number from 1 to 65535.".into()),
        Ok(_) => Ok(()),
    }
}

fn executable_sibling(app: &Path, name: &str) -> PathBuf {
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    app.parent().unwrap_or_else(|| Path::new(".")).join(file)
}

fn config_path(client: Client, global: bool) -> Result<PathBuf, String> {
    let root = if global { home_dir()? } else { std::env::current_dir().map_err(|e| format!("Cannot locate the project directory: {e}"))? };
    Ok(match client {
        Client::Kimi => root.join(".kimi-code/mcp.json"),
        Client::Codex => root.join(".codex/config.toml"),
    })
}

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| "Cannot locate your home directory.".into())
}

fn install(path: &Path, client: Client, cli: &Path, app: &Path, port: &str) -> Result<(), String> {
    backup(path)?;
    match client {
        Client::Kimi => install_kimi(path, cli, app, port),
        Client::Codex => install_codex(path, cli, app, port),
    }
}

fn install_kimi(path: &Path, cli: &Path, app: &Path, port: &str) -> Result<(), String> {
    let mut config = read_json(path)?;
    let root = config.as_object_mut().ok_or_else(|| format!("{} must contain a JSON object.", path.display()))?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{}: mcpServers must be an object.", path.display()))?;
    servers.insert(
        SERVER.into(),
        json!({
            "command": cli,
            "args": ["mcp", "--connect", port, "--launch", app],
            "startupTimeoutMs": 30000,
            "toolTimeoutMs": 120000
        }),
    );
    write(path, serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?)
}

fn install_codex(path: &Path, cli: &Path, app: &Path, port: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut config = if text.trim().is_empty() {
        toml::value::Table::new()
    } else {
        toml::from_str::<toml::value::Table>(&text).map_err(|e| format!("Cannot parse {}: {e}", path.display()))?
    };
    let servers = config
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
        .as_table_mut()
        .ok_or_else(|| format!("{}: mcp_servers must be a table.", path.display()))?;
    let mut server = toml::value::Table::new();
    server.insert("command".into(), toml::Value::String(cli.to_string_lossy().into_owned()));
    server.insert(
        "args".into(),
        toml::Value::Array(
            ["mcp", "--connect", port, "--launch"]
                .into_iter()
                .map(|s| toml::Value::String(s.into()))
                .chain(std::iter::once(toml::Value::String(app.to_string_lossy().into_owned())))
                .collect(),
        ),
    );
    servers.insert(SERVER.into(), toml::Value::Table(server));
    write(path, toml::to_string_pretty(&config).map_err(|e| e.to_string())?)
}

fn remove(path: &Path, client: Client) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    backup(path)?;
    match client {
        Client::Kimi => {
            let mut config = read_json(path)?;
            if let Some(servers) = config.get_mut("mcpServers").and_then(Json::as_object_mut) {
                servers.remove(SERVER);
            }
            write(path, serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?)
        }
        Client::Codex => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
            let mut config = toml::from_str::<toml::value::Table>(&text).map_err(|e| format!("Cannot parse {}: {e}", path.display()))?;
            if let Some(servers) = config.get_mut("mcp_servers").and_then(toml::Value::as_table_mut) {
                servers.remove(SERVER);
            }
            write(path, toml::to_string_pretty(&config).map_err(|e| e.to_string())?)
        }
    }
}

fn read_json(path: &Path) -> Result<Json, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text).map_err(|e| format!("Cannot parse {}: {e}", path.display())),
        Ok(_) => Ok(json!({})),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("Cannot read {}: {e}", path.display())),
    }
}

fn backup(path: &Path) -> Result<(), String> {
    if path.exists() {
        let backup = path.with_extension(format!("{}.bak", path.extension().and_then(|s| s.to_str()).unwrap_or("config")));
        std::fs::copy(path, &backup).map_err(|e| format!("Cannot back up {}: {e}", path.display()))?;
    }
    Ok(())
}

fn write(path: &Path, text: String) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    }
    let temporary = path.with_extension("gridcraft-tmp");
    std::fs::write(&temporary, text).map_err(|e| format!("Cannot write {}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, path).map_err(|e| format!("Cannot replace {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("gridcraft-agent-control-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn kimi_install_preserves_other_servers_and_removes_only_gridcraft() {
        let dir = directory("kimi");
        let path = dir.join("mcp.json");
        std::fs::write(&path, r#"{"mcpServers":{"other":{"command":"other"}},"setting":true}"#).unwrap();
        install(&path, Client::Kimi, Path::new("/bin/gridcraft-cli"), Path::new("/bin/gridcraft"), "7979").unwrap();
        let value = read_json(&path).unwrap();
        assert_eq!(value["setting"], true);
        assert_eq!(value["mcpServers"]["other"]["command"], "other");
        assert_eq!(value["mcpServers"][SERVER]["args"][2], "7979");
        remove(&path, Client::Kimi).unwrap();
        let value = read_json(&path).unwrap();
        assert!(value["mcpServers"].get(SERVER).is_none());
        assert_eq!(value["mcpServers"]["other"]["command"], "other");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn codex_install_preserves_existing_configuration() {
        let dir = directory("codex");
        let path = dir.join("config.toml");
        std::fs::write(&path, "model = \"test\"\n[mcp_servers.other]\ncommand = \"other\"\n").unwrap();
        install(&path, Client::Codex, Path::new("/bin/gridcraft-cli"), Path::new("/bin/gridcraft"), "7979").unwrap();
        let value: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["model"].as_str(), Some("test"));
        assert_eq!(value["mcp_servers"]["other"]["command"].as_str(), Some("other"));
        assert_eq!(value["mcp_servers"][SERVER]["args"][3].as_str(), Some("--launch"));
        remove(&path, Client::Codex).unwrap();
        let value: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(value["mcp_servers"].as_table().unwrap().get(SERVER).is_none());
        assert_eq!(value["mcp_servers"]["other"]["command"].as_str(), Some("other"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn validates_ports() {
        assert!(validate_port("7979").is_ok());
        for port in ["0", "65536", "http", ""] {
            assert!(validate_port(port).is_err());
        }
    }
}
