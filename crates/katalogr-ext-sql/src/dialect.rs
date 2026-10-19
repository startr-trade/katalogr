//! SQL dialects, the parse chain and dialect selection (R-10).

use crate::context::SqlContext;
use serde::{Deserialize, Serialize};
use sqlparser::dialect::{GenericDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Dialect {
    PostgreSql,
    MySql,
    Sqlite,
    Generic,
}

impl Dialect {
    pub fn as_str(self) -> &'static str {
        match self {
            Dialect::PostgreSql => "postgresql",
            Dialect::MySql => "mysql",
            Dialect::Sqlite => "sqlite",
            Dialect::Generic => "generic",
        }
    }

    /// Accepts postgresql, postgres, pg, mysql, mariadb, sqlite, sqlite3, generic, ansi
    /// (case-insensitive); otherwise None.
    pub fn from_name(s: &str) -> Option<Dialect> {
        match s.trim().to_ascii_lowercase().as_str() {
            "postgresql" | "postgres" | "pg" => Some(Dialect::PostgreSql),
            "mysql" | "mariadb" => Some(Dialect::MySql),
            "sqlite" | "sqlite3" => Some(Dialect::Sqlite),
            "generic" | "ansi" => Some(Dialect::Generic),
            _ => None,
        }
    }

    /// The sqlparser dialect.
    pub(crate) fn parser(self) -> &'static dyn sqlparser::dialect::Dialect {
        static PG: PostgreSqlDialect = PostgreSqlDialect {};
        static MY: MySqlDialect = MySqlDialect {};
        static LITE: SQLiteDialect = SQLiteDialect {};
        static GEN: GenericDialect = GenericDialect {};
        match self {
            Dialect::PostgreSql => &PG,
            Dialect::MySql => &MY,
            Dialect::Sqlite => &LITE,
            Dialect::Generic => &GEN,
        }
    }
}

/// The dialects to try, in order: `[d, Generic]` for a known `d`; for auto,
/// `[PostgreSql, MySql, Sqlite, Generic]`.
pub fn chain(known: Option<Dialect>) -> &'static [Dialect] {
    use Dialect::*;
    match known {
        Some(PostgreSql) => &[PostgreSql, Generic],
        Some(MySql) => &[MySql, Generic],
        Some(Sqlite) => &[Sqlite, Generic],
        Some(Generic) => &[Generic],
        None => &[PostgreSql, MySql, Sqlite, Generic],
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialectSource {
    Suffix,
    /// The config path.
    Context(String),
    Auto,
}

/// The file's dialect and where it came from: the suffix (`.pgsql`/`.psql` is PostgreSQL), else
/// the context's longest matching directory, else None (auto).
pub fn resolve(path: &str, ctx: &SqlContext) -> (Option<Dialect>, DialectSource) {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".pgsql") || lower.ends_with(".psql") {
        return (Some(Dialect::PostgreSql), DialectSource::Suffix);
    }
    match ctx.dialect_for(path) {
        Some((d, src)) => (Some(d), DialectSource::Context(src.to_string())),
        None => (None, DialectSource::Auto),
    }
}
