//! Guards for the synthetic test data in `tests/data`.
//!
//! The data is hand-written to follow the hub's schema and holds no captured values. When
//! `DIRIGERA_CAPTURED_DIR` points to a directory of captured hub responses, every field in the
//! synthetic data must also appear in the matching captured response with the same JSON type, so
//! the test data cannot drift from what the hub actually sends, and no identifier from the
//! captured responses may appear in the synthetic data. Without it, including in CI, those
//! checks are skipped.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Synthetic file and the captured response it mirrors.
const PAIRS: [(&str, &str); 1] = [("home.json", "home.json")];

/// Fields whose captured values identify a real hub, device or person.
const IDENTIFYING: [&str; 7] = [
    "id",
    "relationId",
    "deviceId",
    "serialNumber",
    "uid",
    "verifiedUid",
    "email",
];

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn captured_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("DIRIGERA_CAPTURED_DIR").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("skipped: DIRIGERA_CAPTURED_DIR is not set");
    }
    dir
}

/// Collects every place where `synthetic` has a field that `captured` lacks or types differ.
/// `null` on either side matches anything, because nullable fields carry no type.
/// Arrays mix kinds of items, such as lights, outlets and remotes in `devices`, so each synthetic
/// item is held against the captured item it fits best. An empty captured array says nothing
/// about its items, so those are not checked.
fn schema_drift(synthetic: &Value, captured: &Value, path: &str, drift: &mut Vec<String>) {
    match (synthetic, captured) {
        (Value::Null, _)
        | (_, Value::Null)
        | (Value::Bool(_), Value::Bool(_))
        | (Value::Number(_), Value::Number(_))
        | (Value::String(_), Value::String(_)) => {}
        (Value::Object(fields), Value::Object(known)) => {
            for (key, value) in fields {
                let child = format!("{path}.{key}");
                match known.get(key) {
                    Some(known_value) => schema_drift(value, known_value, &child, drift),
                    None => drift.push(format!("{child} is not in the captured response")),
                }
            }
        }
        (Value::Array(items), Value::Array(known)) => {
            for (i, item) in items.iter().enumerate() {
                let child = format!("{path}[{i}]");
                let best = known
                    .iter()
                    .map(|example| {
                        let mut found = Vec::new();
                        schema_drift(item, example, &child, &mut found);
                        found
                    })
                    .min_by_key(Vec::len);
                drift.extend(best.unwrap_or_default());
            }
        }
        _ => drift.push(format!("{path} has a different JSON type than captured")),
    }
}

/// Shorter values, such as an empty email, would match by accident.
const MIN_IDENTIFIER_LEN: usize = 8;

/// Gathers the string values of identifying fields anywhere in `value`.
fn identifiers(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                match field {
                    Value::String(text)
                        if IDENTIFYING.contains(&key.as_str())
                            && text.len() >= MIN_IDENTIFIER_LEN =>
                    {
                        found.push(text.clone());
                    }
                    _ => identifiers(field, found),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| identifiers(item, found)),
        _ => {}
    }
}

#[test]
fn synthetic_data_is_valid_json() {
    for (synthetic, _) in PAIRS {
        read_json(&manifest_dir().join("tests/data").join(synthetic));
    }
}

#[test]
fn synthetic_data_matches_captured_schema() {
    let Some(captured_dir) = captured_dir() else {
        return;
    };
    let mut drift = Vec::new();
    for (synthetic, captured) in PAIRS {
        let synthetic_value = read_json(&manifest_dir().join("tests/data").join(synthetic));
        let captured_value = read_json(&captured_dir.join(captured));
        schema_drift(&synthetic_value, &captured_value, synthetic, &mut drift);
    }
    assert!(
        drift.is_empty(),
        "synthetic data drifted from the API schema:\n{}",
        drift.join("\n")
    );
}

#[test]
fn synthetic_data_holds_no_captured_identifiers() {
    let Some(captured_dir) = captured_dir() else {
        return;
    };
    for (synthetic, captured) in PAIRS {
        let text = fs::read_to_string(manifest_dir().join("tests/data").join(synthetic))
            .unwrap_or_else(|e| panic!("{synthetic}: {e}"));
        let mut found = Vec::new();
        identifiers(&read_json(&captured_dir.join(captured)), &mut found);
        let leaked: Vec<_> = found
            .iter()
            .filter(|id| text.contains(id.as_str()))
            .collect();
        assert!(
            leaked.is_empty(),
            "{synthetic} holds {} captured identifiers",
            leaked.len()
        );
    }
}
