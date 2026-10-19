//! SQL extension (FEAT-017): tables, columns, views, routines, triggers and policies per
//! dialect, migrations, and the guest of embedded SQL text in host code (ADR-0021).

pub mod context;
mod ddl;
pub mod dialect;
pub mod embedded;
mod extract;
mod lines;
pub mod migration;
pub mod names;
pub mod refs;
pub mod scan;
pub mod split;

pub use context::{DialectDir, SchemaChoice, SqlContext, is_test_resource, schema_from_embedded};
pub use dialect::{Dialect, DialectSource, chain, resolve};
pub use embedded::embedded_refs;
pub use migration::{MigrationName, MigrationScheme, parse_migration_name};
pub use names::{
    column_key, fold_schema, is_system_relation, normalise_ident, routine_key, table_key,
};
pub use scan::{Ddl, Header, ObjectKind, Refs, Tok, TokKind, recognise_header, scan_body, tokens};
pub use split::{RawStatement, split_statements, split_statements_in};

/// Contract kind of tables and views: any relation code can query.
pub const CONTRACT_TABLE: &str = "sql_table";
/// Contract kind of columns.
pub const CONTRACT_COLUMN: &str = "sql_column";
/// Contract kind of functions and procedures.
pub const CONTRACT_ROUTINE: &str = "sql_routine";
/// The fallback default schema when no configuration names one.
pub const DEFAULT_SCHEMA: &str = "public";

use katalogr_model::{
    Binding, EmbeddedInput, ExtractError, ExtractInput, Extractor, Fragment, RelPath,
};

/// The SQL extension: host of `.sql` files and guest of embedded SQL text.
#[derive(Clone, Copy, Debug, Default)]
pub struct SqlExtractor;

impl Extractor for SqlExtractor {
    fn id(&self) -> &'static str {
        "sql"
    }

    fn handles(&self, p: &RelPath) -> bool {
        let s = p.as_str().to_ascii_lowercase();
        s.ends_with(".sql") || s.ends_with(".pgsql") || s.ends_with(".psql")
    }

    fn context_files(&self) -> &'static [&'static str] {
        &[
            "flyway.conf",
            "flyway.toml",
            "sqlc.yaml",
            "sqlc.yml",
            "sqlc.json",
            ".sqlfluff",
            "application.properties",
            "application.yml",
            "application.yaml",
        ]
    }

    fn is_context_file(&self, p: &RelPath) -> bool {
        let name = p.as_str().rsplit('/').next().unwrap_or("");
        self.context_files().contains(&name) && !is_test_resource(p.as_str())
    }

    fn build_context(&self, files: &[(RelPath, Vec<u8>)]) -> Result<Vec<u8>, ExtractError> {
        Ok(SqlContext::build(files).to_bytes())
    }

    fn patterns(&self) -> &'static [&'static str] {
        &[
            "tables-columns",
            "foreign-keys",
            "views",
            "routines",
            "triggers",
            "policies",
            "migrations",
            "alter-across-files",
            "procedural-body",
            "delimiter",
            "dml",
            "dialect-context",
            "default-schema",
            "parse-error",
        ]
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        self.extract_with_errors(input).map(|(f, _)| f)
    }

    fn extract_with_errors(
        &self,
        input: &ExtractInput,
    ) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
        Ok(extract::extract(input))
    }

    fn embedded_version(&self) -> Option<&'static str> {
        Some("1")
    }

    fn accepts_embedded(&self, text: &str) -> bool {
        embedded::accepts(text)
    }

    fn extract_embedded(&self, input: &EmbeddedInput) -> Vec<Binding> {
        embedded::extract(input)
    }

    fn embedded_context(&self, context: &[u8]) -> Vec<u8> {
        SqlContext::from_bytes(context).embedded_bytes()
    }
}
