//! The error catalog (design 0013 §2): `schemas/error-codes.json` lists
//! every code the engine, the bridges and the gateway produce, and
//! `docs/errors.md` is generated from it.
//!
//! `CRIOGEN_UPDATE_DOCS=1 cargo test -p criogen-agent --test catalog`
//! rewrites `docs/errors.md` after a catalog change.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use criogen_providers::ModelErrorKind;
use criogen_storage::StorageErrorKind;
use serde_json::Value;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn catalog() -> Vec<Value> {
    let text = std::fs::read_to_string(repo().join("schemas/error-codes.json")).unwrap();
    let catalog: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(catalog["version"], 1);
    catalog["codes"].as_array().unwrap().clone()
}

fn by_code() -> BTreeMap<String, Value> {
    catalog()
        .into_iter()
        .map(|entry| (entry["code"].as_str().unwrap().to_string(), entry))
        .collect()
}

fn layers(entry: &Value) -> Vec<&str> {
    entry["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect()
}

#[test]
fn every_entry_is_well_formed_and_unique() {
    let entries = catalog();
    let mut seen = BTreeSet::new();
    for entry in &entries {
        let code = entry["code"].as_str().unwrap();
        assert!(seen.insert(code), "`{code}` is listed twice");
        assert!(
            !code.is_empty() && code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "`{code}` is not snake_case"
        );
        for layer in layers(entry) {
            assert!(
                [
                    "run",
                    "model",
                    "storage",
                    "thread",
                    "tool_result",
                    "build",
                    "gateway"
                ]
                .contains(&layer),
                "`{code}`: unknown layer `{layer}`"
            );
        }
        let category = entry["category"].as_str().unwrap();
        assert!(
            [
                "transient",
                "provider",
                "provider_fatal",
                "developer",
                "policy",
                "cancellation",
                "model_recoverable",
                "internal"
            ]
            .contains(&category),
            "`{code}`: unknown category `{category}`"
        );
        assert!(
            entry["retryable"].is_boolean() || entry["retryable"].is_null(),
            "`{code}`"
        );
        match &entry["family"] {
            Value::Null => {}
            Value::String(f) => assert!(
                [
                    "model",
                    "tool",
                    "limit",
                    "busy",
                    "suspended",
                    "storage",
                    "validation",
                    "cancelled",
                    "other"
                ]
                .contains(&f.as_str()),
                "`{code}`: unknown family `{f}`"
            ),
            other => panic!("`{code}`: family {other}"),
        }
        assert!(
            entry["description"].as_str().is_some_and(|d| d.len() > 10),
            "`{code}` needs a description"
        );
    }
}

#[test]
fn the_rust_enums_codes_are_all_listed() {
    let codes = by_code();
    let has = |code: &str, layer: &str| {
        let entry = codes
            .get(code)
            .unwrap_or_else(|| panic!("`{code}` is not in schemas/error-codes.json"));
        assert!(
            layers(entry).contains(&layer),
            "`{code}` should list layer `{layer}`"
        );
    };
    for kind in [
        ModelErrorKind::RateLimited,
        ModelErrorKind::Overloaded,
        ModelErrorKind::Server,
        ModelErrorKind::Timeout,
        ModelErrorKind::Network,
        ModelErrorKind::Auth,
        ModelErrorKind::Permission,
        ModelErrorKind::NotFound,
        ModelErrorKind::InvalidRequest,
        ModelErrorKind::ContextOverflow,
        ModelErrorKind::ContentFilter,
        ModelErrorKind::QuotaExhausted,
        ModelErrorKind::InvalidResponse,
        ModelErrorKind::Unknown,
    ] {
        has(kind.code(), "model");
        let entry = &codes[kind.code()];
        assert_eq!(entry["category"], kind.category(), "`{}`", kind.code());
        assert_eq!(entry["retryable"], kind.is_transient(), "`{}`", kind.code());
    }
    for kind in [
        StorageErrorKind::ThreadBusy,
        StorageErrorKind::Unavailable,
        StorageErrorKind::Conflict,
        StorageErrorKind::CheckpointNotFound,
        StorageErrorKind::Corrupt,
        StorageErrorKind::Incompatible,
        StorageErrorKind::InvalidValue,
        StorageErrorKind::EncryptionKeyMissing,
        StorageErrorKind::EncryptionKeyInvalid,
        StorageErrorKind::AuditFailed,
    ] {
        has(kind.code(), "storage");
    }
}

/// String literals that follow `prefix` within a few lines, matching a code.
fn codes_after(text: &str, prefix: &str, found: &mut BTreeMap<String, String>, origin: &str) {
    for (at, _) in text.match_indices(prefix) {
        // A call, not the function's definition or a longer name.
        let before = &text[..at];
        let joined = before
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if before.ends_with("fn ") || (joined && !prefix.starts_with('.')) {
            continue;
        }
        let rest = &text[at + prefix.len()..];
        // Within the call's function: never past the next item.
        let rest = &rest[..rest
            .find(
                "
fn ",
            )
            .unwrap_or(rest.len())];
        let window: String = rest.chars().take(240).collect();
        // Complete literals only: the window may cut the last one.
        let pieces: Vec<&str> = window.split('"').collect();
        let complete = pieces.len().saturating_sub(1);
        let literal = (1..complete)
            .step_by(2)
            .map(|i| pieces[i])
            .find(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        if let Some(code) = literal {
            found
                .entry(code.to_string())
                .or_insert_with(|| origin.to_string());
        }
    }
}

/// Codes inside the bodies of `fn code(` (`=> "…"` arms).
fn codes_in_code_fns(text: &str, found: &mut BTreeMap<String, String>, origin: &str) {
    for (at, _) in text.match_indices("fn code(") {
        let body = &text[at..];
        let end = body.find("\n    }\n").unwrap_or(body.len());
        for arm in body[..end].split("=> \"").skip(1) {
            if let Some(code) = arm.split('"').next()
                && code.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            {
                found
                    .entry(code.to_string())
                    .or_insert_with(|| origin.to_string());
            }
        }
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every code raised anywhere in the sources.
fn raised_codes() -> BTreeMap<String, String> {
    let root = repo();
    let mut files = Vec::new();
    for dir in ["crates", "sdks/python/src", "sdks/typescript/src"] {
        rust_files(&root.join(dir), &mut files);
    }
    let mut found = BTreeMap::new();
    for file in files {
        let path = file.to_string_lossy().replace('\\', "/");
        if !path.contains("/src/") || path.contains("/tests/") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        // Unit tests may use made-up codes.
        let text = text.split("#[cfg(test)]").next().unwrap_or_default();
        let origin = path.rsplit("/crates/").next().unwrap_or(&path).to_string();
        codes_in_code_fns(text, &mut found, &origin);
        for prefix in [
            ".with_kind(",
            "AgentError::new(",
            "error_result(",
            "ToolResultError::new(",
            "Failure::new(",
            "failure(",
            "error_response(",
        ] {
            codes_after(text, prefix, &mut found, &origin);
        }
    }
    found
}

#[test]
fn every_code_in_the_sources_is_listed_and_every_listed_code_is_used() {
    let codes = by_code();
    let raised = raised_codes();
    let missing: Vec<String> = raised
        .iter()
        .filter(|(code, _)| !codes.contains_key(*code))
        .map(|(code, origin)| format!("`{code}` (from {origin})"))
        .collect();
    assert!(
        missing.is_empty(),
        "add these to schemas/error-codes.json: {missing:?}"
    );
    let unused: Vec<&String> = codes.keys().filter(|c| !raised.contains_key(*c)).collect();
    assert!(
        unused.is_empty(),
        "listed but raised nowhere (remove, or check the scan): {unused:?}"
    );
}

/// The Markdown page for the catalog.
fn render() -> String {
    let entries = catalog();
    let mut out = String::from(
        "# Error codes\n\n\
         Generated from [`schemas/error-codes.json`](../schemas/error-codes.json) \
         (design 0013); do not edit by hand. Codes are stable: they may be added, \
         never renamed or removed before 1.0.\n\n\
         *SDK class* is the Python / TypeScript error class a run error with that \
         code raises (`-` for codes that appear inside other errors or messages).\n",
    );
    for (layer, title) in [
        ("run", "Run errors"),
        ("thread", "Thread errors"),
        ("storage", "Storage errors"),
        (
            "model",
            "Model errors (`details.model_error.kind` of `model_failed`)",
        ),
        (
            "tool_result",
            "Tool results (`error.code` of a tool message; the run goes on)",
        ),
        ("build", "Build errors"),
        ("gateway", "Gateway errors (`error.code` in the HTTP body)"),
    ] {
        out.push_str(&format!(
            "\n## {title}\n\n| Code | Category | Retryable | SDK class | HTTP | Meaning |\n|---|---|---|---|---|---|\n"
        ));
        for entry in entries.iter().filter(|e| layers(e).contains(&layer)) {
            let class = match entry["family"].as_str() {
                Some("model") => "`ModelError`",
                Some("tool") => "`ToolExecutionError`",
                Some("limit") => "`LimitExceededError`",
                Some("busy") => "`ThreadBusyError`",
                Some("suspended") => "`ThreadSuspendedError`",
                Some("storage") => "`StorageError`",
                Some("validation") => "`ValidationError`",
                Some("cancelled") => "`CancelledError`",
                Some(_) => "`CriogenError`",
                None => "-",
            };
            let retryable = match &entry["retryable"] {
                Value::Bool(true) => "yes",
                Value::Bool(false) => "no",
                _ => "depends",
            };
            let http = entry["http_status"]
                .as_u64()
                .map_or("-".to_string(), |s| s.to_string());
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {} |\n",
                entry["code"].as_str().unwrap(),
                entry["category"].as_str().unwrap(),
                retryable,
                class,
                http,
                entry["description"].as_str().unwrap()
            ));
        }
    }
    out
}

#[test]
fn the_docs_page_is_generated_from_the_catalog() {
    let path = repo().join("docs/errors.md");
    let expected = render();
    if std::env::var_os("CRIOGEN_UPDATE_DOCS").is_some() {
        std::fs::write(&path, &expected).unwrap();
    }
    let actual = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        actual == expected,
        "docs/errors.md is out of date: run CRIOGEN_UPDATE_DOCS=1 cargo test -p criogen-agent --test catalog"
    );
}
