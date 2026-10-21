//! Thread count and round byte cap for indexing (FEAT-072, ADR-0027).

use crate::error::DistError;
use katalogr_engine::parallel::Parallelism;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;

/// Setting keys (config.yaml `settings`).
pub const THREADS_KEY: &str = "index.threads";
pub const BATCH_BYTES_KEY: &str = "index.batch_bytes";
pub const THREADS_ENV: &str = "KATALOGR_THREADS";

/// The largest accepted thread count.
const MAX_THREADS: u64 = 1024;
/// The smallest accepted `index.batch_bytes`.
const MIN_BATCH_BYTES: u64 = 1024 * 1024;

fn bad_threads(source: &str, got: &str) -> DistError {
    DistError::usage(format!(
        "{source} must be a positive integer up to {MAX_THREADS}, 0 or auto, got `{got}`"
    ))
}

/// `None` means available cores.
fn parse_threads(source: &str, text: &str) -> Result<Option<NonZeroUsize>, DistError> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    match t.parse::<u64>() {
        Ok(0) => Ok(None),
        Ok(n) if n <= MAX_THREADS => Ok(NonZeroUsize::new(n as usize)),
        _ => Err(bad_threads(source, t)),
    }
}

/// Precedence: `KATALOGR_THREADS`, then `index.threads`, then available cores. `0` or `auto`
/// means available cores. Anything else that is not a positive integer up to 1024 is a usage
/// error (exit 2) naming its source. `index.batch_bytes` is an integer of at least 1 MiB.
pub fn resolve_parallelism(
    settings: &BTreeMap<String, serde_yaml_ng::Value>,
    env: Option<&str>,
) -> Result<Parallelism, DistError> {
    let mut p = Parallelism::from_cores();
    let chosen = match (env, settings.get(THREADS_KEY)) {
        (Some(e), _) => parse_threads(THREADS_ENV, e)?,
        (None, Some(v)) => {
            let text = match v {
                serde_yaml_ng::Value::Number(n) => n.to_string(),
                serde_yaml_ng::Value::String(s) => s.clone(),
                other => return Err(bad_threads(THREADS_KEY, &format!("{other:?}"))),
            };
            parse_threads(THREADS_KEY, &text)?
        }
        (None, None) => None,
    };
    if let Some(n) = chosen {
        p.threads = n;
    }
    if let Some(v) = settings.get(BATCH_BYTES_KEY) {
        let bytes = v
            .as_u64()
            .filter(|b| *b >= MIN_BATCH_BYTES)
            .ok_or_else(|| {
                DistError::usage(format!(
                    "{BATCH_BYTES_KEY} must be an integer of at least {MIN_BATCH_BYTES}, got `{}`",
                    serde_yaml_ng::to_string(v).unwrap_or_default().trim()
                ))
            })?;
        p.batch_bytes = bytes;
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml_ng::Value;

    fn settings(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn n(v: usize) -> NonZeroUsize {
        NonZeroUsize::new(v).unwrap()
    }

    #[test]
    fn threads_precedence() {
        let cores = Parallelism::from_cores().threads;
        let none = settings(&[]);
        assert_eq!(resolve_parallelism(&none, None).unwrap().threads, cores);
        let set = settings(&[(THREADS_KEY, Value::from(3))]);
        assert_eq!(resolve_parallelism(&set, None).unwrap().threads, n(3));
        assert_eq!(resolve_parallelism(&set, Some("5")).unwrap().threads, n(5));
        assert_eq!(
            resolve_parallelism(&set, Some("auto")).unwrap().threads,
            cores
        );
        assert_eq!(resolve_parallelism(&set, Some("0")).unwrap().threads, cores);
        let auto = settings(&[(THREADS_KEY, Value::from("auto"))]);
        assert_eq!(resolve_parallelism(&auto, None).unwrap().threads, cores);
        let zero = settings(&[(THREADS_KEY, Value::from(0))]);
        assert_eq!(resolve_parallelism(&zero, None).unwrap().threads, cores);
        assert_eq!(resolve_parallelism(&none, Some("1")).unwrap().threads, n(1));
        assert_eq!(
            resolve_parallelism(&none, Some("1024")).unwrap().threads,
            n(1024)
        );
        let b = settings(&[(BATCH_BYTES_KEY, Value::from(2 * 1024 * 1024))]);
        assert_eq!(resolve_parallelism(&b, None).unwrap().batch_bytes, 2 << 20);
        assert_eq!(
            resolve_parallelism(&none, None).unwrap().batch_bytes,
            Parallelism::DEFAULT_BATCH_BYTES
        );
    }

    #[test]
    fn threads_invalid_is_usage_error() {
        let none = settings(&[]);
        for bad in ["abc", "-1", "1025", "1.5", ""] {
            let e = resolve_parallelism(&none, Some(bad)).unwrap_err();
            assert!(e.is_usage(), "{bad}");
            assert!(e.to_string().contains(THREADS_ENV), "{e}");
        }
        let s = settings(&[(THREADS_KEY, Value::from("many"))]);
        let e = resolve_parallelism(&s, None).unwrap_err();
        assert!(e.is_usage() && e.to_string().contains(THREADS_KEY), "{e}");
        let s = settings(&[(THREADS_KEY, Value::from(2000))]);
        assert!(resolve_parallelism(&s, None).unwrap_err().is_usage());
        // The environment wins even over a bad setting.
        assert!(resolve_parallelism(&s, Some("2")).is_ok());
        let s = settings(&[(BATCH_BYTES_KEY, Value::from(1000))]);
        let e = resolve_parallelism(&s, None).unwrap_err();
        assert!(
            e.is_usage() && e.to_string().contains(BATCH_BYTES_KEY),
            "{e}"
        );
    }
}
