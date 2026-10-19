//! Identifier normalisation and contract keys (R-9, R-20).

use crate::DEFAULT_SCHEMA;

/// Identifier as keyed: quotes (`""`, backticks, `[]`) removed, doubled quotes unescaped, then
/// lowercased.
pub fn normalise_ident(raw: &str) -> String {
    let raw = raw.trim();
    let b = raw.as_bytes();
    let unquoted = if b.len() >= 2 {
        match (b[0], b[b.len() - 1]) {
            (b'"', b'"') => raw[1..raw.len() - 1].replace("\"\"", "\""),
            (b'`', b'`') => raw[1..raw.len() - 1].replace("``", "`"),
            (b'[', b']') => raw[1..raw.len() - 1].replace("]]", "]"),
            _ => raw.to_string(),
        }
    } else {
        raw.to_string()
    };
    unquoted.to_lowercase()
}

/// A normalised schema name with SQLite's `main` folded to the default schema.
pub fn fold_schema(schema: &str) -> String {
    if schema == "main" {
        DEFAULT_SCHEMA.to_string()
    } else {
        schema.to_string()
    }
}

/// `<schema>.<name>` from 1 to 3 name parts (the last two are used). A missing schema becomes
/// `default_schema`; an explicit schema goes through [`fold_schema`].
pub fn table_key(parts: &[&str], default_schema: &str) -> String {
    let n = parts.len();
    match n {
        0 => format!("{default_schema}."),
        1 => format!("{default_schema}.{}", normalise_ident(parts[0])),
        _ => format!(
            "{}.{}",
            fold_schema(&normalise_ident(parts[n - 2])),
            normalise_ident(parts[n - 1])
        ),
    }
}

/// `<schema>.<table>.<column>`.
pub fn column_key(table_key: &str, column: &str) -> String {
    format!("{table_key}.{}", normalise_ident(column))
}

/// Like [`table_key`], for routines.
pub fn routine_key(parts: &[&str], default_schema: &str) -> String {
    table_key(parts, default_schema)
}

const SYSTEM_SCHEMAS: &[&str] = &[
    "information_schema",
    "pg_catalog",
    "mysql",
    "performance_schema",
    "sys",
];
const SYSTEM_NAMES: &[&str] = &[
    "dual",
    "sqlite_master",
    "sqlite_schema",
    "sqlite_sequence",
    "sqlite_temp_master",
];

/// True for catalog relations that are never bound: the system schemas, the unqualified
/// catalog names, and any unqualified name starting with `pg_`.
pub fn is_system_relation(parts: &[&str]) -> bool {
    let n = parts.len();
    if n >= 2 && SYSTEM_SCHEMAS.contains(&normalise_ident(parts[n - 2]).as_str()) {
        return true;
    }
    if n == 1 {
        let name = normalise_ident(parts[0]);
        return SYSTEM_NAMES.contains(&name.as_str()) || name.starts_with("pg_");
    }
    false
}
