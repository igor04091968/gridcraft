//! Fill missing Russian interface translations through Google Translate's public endpoint.
//! Existing entries are never changed; machine-translated additions need native-speaker review.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use crate::i18n_report::catalog_keys;

const ENDPOINT: &str = "https://translate.googleapis.com/translate_a/single";

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    if args.iter().any(|arg| !matches!(*arg, "--dry-run" | "-h" | "--help")) {
        return Err("usage: cargo xtask i18n-russian [--dry-run]".to_owned());
    }
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        println!(
            "usage: cargo xtask i18n-russian [--dry-run]\n\nFill missing entries in crates/ui-egui/src/i18n/ru.rs without changing existing translations."
        );
        return Ok(());
    }

    let dir = root.join("crates/ui-egui/src/i18n");
    let catalogs = read_catalogs(&dir)?;
    let russian = catalogs.get("ru").ok_or_else(|| format!("can't find {}", dir.join("ru.rs").display()))?;
    let reference: BTreeSet<&String> = catalogs.values().flatten().collect();
    let missing: Vec<&str> = reference.iter().filter(|key| !russian.contains(key.as_str())).map(|key| key.as_str()).collect();
    println!("i18n-russian: {} strings to translate", missing.len());
    if args.contains(&"--dry-run") {
        for key in missing {
            println!("  {key:?}");
        }
        return Ok(());
    }

    let mut translated = Vec::new();
    for (index, key) in missing.iter().enumerate() {
        let value = translate(key)?;
        validate(key, &value)?;
        translated.push((*key, value));
        println!("  [{}/{}] {key:?}", index + 1, missing.len());
    }
    let path = dir.join("ru.rs");
    let source = std::fs::read_to_string(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let updated = append(&source, &translated)?;
    let temporary = path.with_extension("rs.tmp");
    std::fs::write(&temporary, updated).map_err(|e| format!("can't write {}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, &path).map_err(|e| format!("can't replace {}: {e}", path.display()))?;
    println!("i18n-russian: added {} entries to {}; review the machine translations", translated.len(), path.display());
    Ok(())
}

fn read_catalogs(dir: &Path) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("can't read {}: {e}", dir.display()))?;
    let mut catalogs = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("can't read {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("rs") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(std::ffi::OsStr::to_str) else { continue };
        let source = std::fs::read_to_string(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
        if source.contains("fn translate(") {
            catalogs.insert(stem.to_owned(), catalog_keys(&source));
        }
    }
    Ok(catalogs)
}

fn translate(text: &str) -> Result<String, String> {
    let label = text.rsplit_once('|').map_or(text, |(_, label)| label);
    let mut protected = label.to_owned();
    let mut names: Vec<&str> = placeholders(text).into_iter().collect();
    names.sort_unstable();
    for (index, name) in names.iter().enumerate() {
        protected = protected.replace(&format!("{{{name}}}"), &format!("[[[{index}]]]"));
    }
    let mut translated = translate_raw(&protected)?;
    for (index, name) in names.iter().enumerate() {
        translated = translated.replace(&format!("[[[{index}]]]"), &format!("{{{name}}}"));
    }
    Ok(translated)
}

fn translate_raw(text: &str) -> Result<String, String> {
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--retry",
            "4",
            "--retry-all-errors",
            "--retry-delay",
            "1",
            "--get",
            ENDPOINT,
            "--data-urlencode",
            "client=gtx",
            "--data-urlencode",
            "sl=en",
            "--data-urlencode",
            "tl=ru",
            "--data-urlencode",
            "dt=t",
            "--data-urlencode",
        ])
        .arg(format!("q={text}"))
        .output()
        .map_err(|e| format!("can't run curl (install curl to use automatic translation): {e}"))?;
    if !output.status.success() {
        return Err(format!("translation request failed for {text:?}: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("invalid translation response for {text:?}: {e}"))?;
    translated_text(&response).ok_or_else(|| format!("translation response has no text for {text:?}"))
}

fn translated_text(response: &serde_json::Value) -> Option<String> {
    response.get(0)?.as_array()?.iter().map(|part| part.get(0)?.as_str()).collect()
}

fn placeholders(text: &str) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        out.insert(&after[..end]);
        rest = &after[end + 1..];
    }
    out
}

fn validate(source: &str, translated: &str) -> Result<(), String> {
    if translated.trim().is_empty() {
        return Err(format!("empty translation for {source:?}"));
    }
    if placeholders(source) != placeholders(translated) {
        return Err(format!("placeholders changed in translation of {source:?}: {translated:?}"));
    }
    if source.lines().count() != translated.lines().count() {
        return Err(format!("line count changed in translation of {source:?}: {translated:?}"));
    }
    if translated.contains('|') {
        return Err(format!("context separator appeared in translation of {source:?}: {translated:?}"));
    }
    Ok(())
}

fn append(source: &str, translated: &[(&str, String)]) -> Result<String, String> {
    let marker = "        _ => return None,";
    let at = source.find(marker).ok_or_else(|| "ru.rs has no fallback match arm".to_owned())?;
    let mut entries = String::new();
    for (key, value) in translated {
        entries.push_str(&format!("        {key:?} => {value:?},\n"));
    }
    let mut updated = String::with_capacity(source.len() + entries.len());
    updated.push_str(&source[..at]);
    updated.push_str(&entries);
    updated.push_str(&source[at..]);
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_translation_segments() {
        let response = serde_json::json!([[["Привет", "Hello", null, null, 10], [" мир", " world", null, null, 1]], null, "en"]);
        assert_eq!(translated_text(&response).as_deref(), Some("Привет мир"));
    }

    #[test]
    fn checks_placeholders_and_line_count() {
        validate("Page {page} of {pages}", "Страница {page} из {pages}").unwrap();
        assert!(validate("Page {page}", "Страница").is_err());
        assert!(validate("Top\nAlign", "Вверху").is_err());
    }

    #[test]
    fn appends_before_fallback() {
        let source = "    Some(match text {\n        \"Paste\" => \"Вставить\",\n        _ => return None,\n    })\n";
        let updated = append(source, &[("Line\nBreak", "Перевод\nстроки".to_owned())]).unwrap();
        assert!(updated.contains("        \"Line\\nBreak\" => \"Перевод\\nстроки\",\n        _ => return None,"));
    }
}
