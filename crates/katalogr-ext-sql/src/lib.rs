//! SQL extension (FEAT-017): tables, columns, views, routines, triggers and policies per
//! dialect, migrations, and the guest of embedded SQL text in host code (ADR-0021).

pub mod context;
pub mod dialect;
mod lines;
pub mod migration;
pub mod names;
pub mod split;

pub use context::{DialectDir, SchemaChoice, SqlContext, is_test_resource, schema_from_embedded};
pub use dialect::{Dialect, DialectSource, chain, resolve};
pub use migration::{MigrationName, MigrationScheme, parse_migration_name};
pub use names::{
    column_key, fold_schema, is_system_relation, normalise_ident, routine_key, table_key,
};
pub use split::{RawStatement, split_statements, split_statements_in};

/// Contract kind of tables and views: any relation code can query.
pub const CONTRACT_TABLE: &str = "sql_table";
/// Contract kind of columns.
pub const CONTRACT_COLUMN: &str = "sql_column";
/// Contract kind of functions and procedures.
pub const CONTRACT_ROUTINE: &str = "sql_routine";
/// The fallback default schema when no configuration names one.
pub const DEFAULT_SCHEMA: &str = "public";
