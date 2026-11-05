//! The one-line query a `katalogr-query` block holds, parsed into a normalised [`ViewSpec`].

use crate::RenderError;
use std::collections::BTreeMap;

/// One view question. `args` holds every argument with its default filled in, so two spellings
/// of one question are equal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewSpec {
    pub op: String,
    pub args: BTreeMap<String, String>,
}

/// How an argument is read.
#[derive(Clone, Copy)]
enum Kind {
    /// A positive whole number, written back without leading zeros.
    Number,
    /// One of the listed words.
    Word(&'static [&'static str]),
    /// Any single word.
    Text,
    /// A bare switch, optionally followed by `true` or `false`.
    Switch,
}

struct Flag {
    name: &'static str,
    kind: Kind,
    default: &'static str,
}

struct Op {
    name: &'static str,
    /// Positional arguments, in order.
    positional: &'static [&'static str],
    flags: &'static [Flag],
}

const LEVELS: &[&str] = &["file", "package"];

const OPS: &[Op] = &[
    Op {
        name: "structure",
        positional: &[],
        flags: &[
            Flag {
                name: "level",
                kind: Kind::Word(LEVELS),
                default: "package",
            },
            Flag {
                name: "max-nodes",
                kind: Kind::Number,
                default: "60",
            },
            Flag {
                name: "inferred",
                kind: Kind::Switch,
                default: "false",
            },
        ],
    },
    Op {
        name: "contracts",
        positional: &[],
        flags: &[
            // An empty kind means every kind.
            Flag {
                name: "kind",
                kind: Kind::Text,
                default: "",
            },
            Flag {
                name: "max-nodes",
                kind: Kind::Number,
                default: "60",
            },
        ],
    },
    Op {
        name: "callers",
        positional: &["symbol"],
        flags: &[
            Flag {
                name: "depth",
                kind: Kind::Number,
                default: "2",
            },
            Flag {
                name: "max-nodes",
                kind: Kind::Number,
                default: "60",
            },
        ],
    },
    Op {
        name: "path",
        positional: &["from", "to"],
        flags: &[Flag {
            name: "max-len",
            kind: Kind::Number,
            default: "6",
        }],
    },
    Op {
        name: "cycles",
        positional: &[],
        flags: &[
            Flag {
                name: "level",
                kind: Kind::Word(LEVELS),
                default: "package",
            },
            Flag {
                name: "limit",
                kind: Kind::Number,
                default: "5",
            },
        ],
    },
];

fn bad<T>(msg: impl Into<String>) -> Result<T, RenderError> {
    Err(RenderError::Spec(msg.into()))
}

/// Split on spaces; a double-quoted word may hold spaces. The quotes are dropped.
fn words(text: &str) -> Result<Vec<String>, RenderError> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return bad("unterminated double quote");
    }
    if started {
        out.push(cur);
    }
    Ok(out)
}

fn value(flag: &Flag, raw: &str) -> Result<String, RenderError> {
    match flag.kind {
        Kind::Number => match raw.parse::<u64>() {
            Ok(n) if n > 0 => Ok(n.to_string()),
            _ => bad(format!(
                "--{} wants a whole number above 0, got `{raw}`",
                flag.name
            )),
        },
        Kind::Word(allowed) => {
            if allowed.contains(&raw) {
                Ok(raw.to_string())
            } else {
                bad(format!(
                    "--{} is one of {}, got `{raw}`",
                    flag.name,
                    allowed.join(", ")
                ))
            }
        }
        Kind::Text => {
            if raw.is_empty() {
                bad(format!("--{} wants a value", flag.name))
            } else {
                Ok(raw.to_string())
            }
        }
        Kind::Switch => match raw {
            "true" | "false" => Ok(raw.to_string()),
            _ => bad(format!("--{} is true or false, got `{raw}`", flag.name)),
        },
    }
}

impl ViewSpec {
    /// Parse one query line such as `structure --level package --max-nodes 40`. Unknown ops and
    /// flags, missing or extra positionals and bad values are errors.
    pub fn parse(text: &str) -> Result<ViewSpec, RenderError> {
        let text = text.trim();
        if text.is_empty() {
            return bad("empty query");
        }
        if text.contains('\n') {
            return bad("a query is one line");
        }
        let words = words(text)?;
        let name = &words[0];
        let Some(op) = OPS.iter().find(|o| o.name == name) else {
            let known: Vec<&str> = OPS.iter().map(|o| o.name).collect();
            return bad(format!(
                "unknown op `{name}`; known ops: {}",
                known.join(", ")
            ));
        };
        let mut args: BTreeMap<String, String> = BTreeMap::new();
        let mut positional: Vec<&str> = Vec::new();
        let mut i = 1;
        while i < words.len() {
            let w = words[i].as_str();
            i += 1;
            let Some(flag_text) = w.strip_prefix("--") else {
                positional.push(w);
                continue;
            };
            let (fname, inline) = match flag_text.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (flag_text, None),
            };
            let Some(flag) = op.flags.iter().find(|f| f.name == fname) else {
                return bad(format!("unknown flag `--{fname}` for `{}`", op.name));
            };
            if args.contains_key(flag.name) {
                return bad(format!("--{} given twice", flag.name));
            }
            let raw = match (inline, flag.kind) {
                (Some(v), _) => v,
                (None, Kind::Switch) => match words.get(i).map(String::as_str) {
                    Some(next @ ("true" | "false")) => {
                        i += 1;
                        next.to_string()
                    }
                    _ => "true".to_string(),
                },
                (None, _) => match words.get(i) {
                    Some(v) => {
                        i += 1;
                        v.clone()
                    }
                    None => return bad(format!("--{} wants a value", flag.name)),
                },
            };
            args.insert(flag.name.to_string(), value(flag, &raw)?);
        }
        if positional.len() != op.positional.len() {
            return bad(format!(
                "`{}` takes {} argument(s) ({}), got {}",
                op.name,
                op.positional.len(),
                op.positional.join(" "),
                positional.len()
            ));
        }
        for (name, v) in op.positional.iter().zip(positional) {
            args.insert((*name).to_string(), v.to_string());
        }
        for f in op.flags {
            args.entry(f.name.to_string())
                .or_insert_with(|| f.default.to_string());
        }
        Ok(ViewSpec {
            op: op.name.to_string(),
            args,
        })
    }
}

#[cfg(test)]
fn args(spec: &ViewSpec) -> Vec<(&str, &str)> {
    spec.args
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

#[cfg(test)]
#[test]
fn parses_every_op() {
    let s = ViewSpec::parse("structure --level file --max-nodes 40 --inferred").unwrap();
    assert_eq!(s.op, "structure");
    assert_eq!(
        args(&s),
        [("inferred", "true"), ("level", "file"), ("max-nodes", "40")]
    );
    let s = ViewSpec::parse("contracts --kind rpc --max-nodes 10").unwrap();
    assert_eq!(args(&s), [("kind", "rpc"), ("max-nodes", "10")]);
    let s = ViewSpec::parse("callers \"my symbol\" --depth 3 --max-nodes 20").unwrap();
    assert_eq!(
        args(&s),
        [("depth", "3"), ("max-nodes", "20"), ("symbol", "my symbol")]
    );
    let s = ViewSpec::parse("path a b --max-len 9").unwrap();
    assert_eq!(args(&s), [("from", "a"), ("max-len", "9"), ("to", "b")]);
    let s = ViewSpec::parse("cycles --level file --limit 2").unwrap();
    assert_eq!(args(&s), [("level", "file"), ("limit", "2")]);
}

#[cfg(test)]
#[test]
fn defaults_are_filled() {
    let s = ViewSpec::parse("structure").unwrap();
    assert_eq!(
        args(&s),
        [
            ("inferred", "false"),
            ("level", "package"),
            ("max-nodes", "60")
        ]
    );
    assert_eq!(
        ViewSpec::parse("contracts").unwrap().args["max-nodes"],
        "60"
    );
    let c = ViewSpec::parse("callers x").unwrap();
    assert_eq!(
        (c.args["depth"].as_str(), c.args["max-nodes"].as_str()),
        ("2", "60")
    );
    assert_eq!(ViewSpec::parse("path a b").unwrap().args["max-len"], "6");
    let c = ViewSpec::parse("cycles").unwrap();
    assert_eq!(
        (c.args["level"].as_str(), c.args["limit"].as_str()),
        ("package", "5")
    );
    // Two spellings of one question are equal.
    assert_eq!(
        ViewSpec::parse("structure --max-nodes 060 --level=package").unwrap(),
        ViewSpec::parse("  structure  --inferred false ")
            .unwrap()
            .with("max-nodes", "60")
    );
}

#[cfg(test)]
impl ViewSpec {
    fn with(mut self, k: &str, v: &str) -> ViewSpec {
        self.args.insert(k.into(), v.into());
        self
    }
}

#[cfg(test)]
#[test]
fn bad_block_is_error() {
    let cases = [
        "",
        "   ",
        "nonsense",
        "mdbook",
        "structure --nope 1",
        "structure --level dir",
        "structure --max-nodes",
        "structure --max-nodes 0",
        "structure --max-nodes x",
        "structure --max-nodes 1 --max-nodes 2",
        "structure extra",
        "callers",
        "path onlyone",
        "path a b c",
        "callers \"unterminated",
        "structure\ncycles",
    ];
    for c in cases {
        assert!(
            matches!(ViewSpec::parse(c), Err(RenderError::Spec(_))),
            "`{c}` should be refused"
        );
    }
}
