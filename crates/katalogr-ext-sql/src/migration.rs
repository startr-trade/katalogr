//! Migration file names to facts (R-8), hand-parsed.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MigrationScheme {
    Versioned,
    Undo,
    Repeatable,
    Up,
    Down,
}

impl MigrationScheme {
    pub fn as_str(self) -> &'static str {
        match self {
            MigrationScheme::Versioned => "versioned",
            MigrationScheme::Undo => "undo",
            MigrationScheme::Repeatable => "repeatable",
            MigrationScheme::Up => "up",
            MigrationScheme::Down => "down",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationName {
    pub scheme: MigrationScheme,
    pub version: Option<String>,
    pub description: String,
    /// Each version part left-padded with zeros to 20 digits, joined by `.`, so that lexical
    /// order is numeric order.
    pub order: Option<String>,
}

/// Strip the SQL file extension.
fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

/// Digits separated by `.` or `_` (Flyway), normalised to dots without leading zeros.
fn flyway_version(v: &str) -> Option<String> {
    if v.is_empty() || !v.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let mut parts = Vec::new();
    for p in v.split(['.', '_']) {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        parts.push(strip_zeros(p));
    }
    Some(parts.join("."))
}

fn strip_zeros(p: &str) -> String {
    let t = p.trim_start_matches('0');
    if t.is_empty() {
        "0".to_string()
    } else {
        t.to_string()
    }
}

fn order_of(version: &str) -> String {
    version
        .split('.')
        .map(|p| format!("{p:0>20}"))
        .collect::<Vec<_>>()
        .join(".")
}

fn make(scheme: MigrationScheme, version: Option<String>, desc: &str) -> MigrationName {
    MigrationName {
        scheme,
        order: version.as_deref().map(order_of),
        version,
        description: desc.replace('_', " "),
    }
}

/// From a repo-relative path. None when the name follows no known convention.
pub fn parse_migration_name(path: &str) -> Option<MigrationName> {
    let mut comps = path.rsplit('/');
    let file = comps.next()?;
    let parent = comps.next();
    let s = stem(file);

    // Flyway: V<ver>__<desc>, U<ver>__<desc>, R__<desc>.
    if let Some(desc) = s.strip_prefix("R__") {
        return Some(make(MigrationScheme::Repeatable, None, desc));
    }
    if let Some(first) = s.chars().next()
        && (first == 'V' || first == 'U')
        && let Some((ver, desc)) = s[1..].split_once("__")
        && let Some(version) = flyway_version(ver)
    {
        let scheme = if first == 'V' {
            MigrationScheme::Versioned
        } else {
            MigrationScheme::Undo
        };
        return Some(make(scheme, Some(version), desc));
    }

    // Numeric prefix: <digits>_<desc>[.up|.down].sql
    if s.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        let (body, scheme) = if let Some(b) = s.strip_suffix(".up") {
            (b, MigrationScheme::Up)
        } else if let Some(b) = s.strip_suffix(".down") {
            (b, MigrationScheme::Down)
        } else {
            (s, MigrationScheme::Versioned)
        };
        if let Some((digits, desc)) = body.split_once('_')
            && !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
        {
            return Some(make(scheme, Some(strip_zeros(digits)), desc));
        }
    }

    // Directory form: <digits and '-'>_<desc>/up.sql or down.sql.
    let scheme = match s {
        "up" => MigrationScheme::Up,
        "down" => MigrationScheme::Down,
        _ => return None,
    };
    let (digits, desc) = parent?.split_once('_')?;
    if digits.is_empty()
        || !digits.as_bytes()[0].is_ascii_digit()
        || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'-')
    {
        return None;
    }
    let version: String = digits.chars().filter(|c| *c != '-').collect();
    Some(make(scheme, Some(strip_zeros(&version)), desc))
}
