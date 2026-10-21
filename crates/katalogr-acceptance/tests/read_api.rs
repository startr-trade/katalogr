//! The read-API transcript (FEAT-075 R-5, R-9): every `Reader` method answered for every fixture
//! case and for the eval corpora, recorded before the cache format changes. A new cache format
//! must reproduce it byte for byte. Ids are hex and floats are bit patterns. Bless with
//! `KATALOGR_BLESS=1` after reviewing the diff.
mod read_api {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_acceptance::{Case, full_registry, load_manifest};
    use katalogr_model::{Id, Name, tokenize};
    use katalogr_store::{LinkKey, Posting, RANK_CONTEXT, Reader};
    use serde::Deserialize;
    use serde_json::{Value, json};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::OnceLock;

    fn root() -> Utf8PathBuf {
        Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize_utf8()
            .unwrap()
    }

    fn golden_path() -> Utf8PathBuf {
        Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/read_api.json")
    }

    fn j<T: serde::Serialize>(v: &T) -> Value {
        serde_json::to_value(v).unwrap()
    }

    fn bits(f: f64) -> String {
        format!("{:016x}", f.to_bits())
    }

    fn key(k: &LinkKey) -> Value {
        json!([k.from.to_hex(), k.kind.0, k.to.to_hex()])
    }

    fn posting(p: &Posting) -> Value {
        json!([p.field, p.symbol.to_hex(), p.tf])
    }

    fn ok<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("read failed: {e:?}"))
    }

    /// The answers of every `Reader` method over one snapshot.
    fn transcript(tx: &impl Reader) -> Value {
        let mut t = serde_json::Map::new();
        let artifacts = ok(tx.artifacts());
        let symbols = ok(tx.symbols());
        let links = ok(tx.links_all());
        t.insert("artifacts".into(), j(&artifacts));
        t.insert("symbols".into(), j(&symbols));
        t.insert(
            "links_all".into(),
            Value::Array(links.iter().map(|(k, r)| json!([key(k), j(r)])).collect()),
        );

        let mut ids: Vec<Id> = artifacts
            .iter()
            .map(|(i, _)| *i)
            .chain(symbols.iter().map(|(i, _)| *i))
            .collect();
        ids.sort();
        ids.dedup();
        let kinds: BTreeSet<String> = links.iter().map(|(k, _)| k.kind.0.clone()).collect();

        let mut per_id = Vec::new();
        for id in &ids {
            let out: Vec<Value> = ok(tx.links_out(*id, None))
                .iter()
                .map(|(k, r)| json!([key(k), j(r)]))
                .collect();
            let ins: Vec<Value> = ok(tx.links_in(*id)).iter().map(key).collect();
            let mut by_kind = serde_json::Map::new();
            for kind in &kinds {
                let ks = ok(tx.link_keys_out(*id, &katalogr_model::LinkKind(kind.clone())));
                by_kind.insert(kind.clone(), Value::Array(ks.iter().map(key).collect()));
            }
            per_id.push(json!({
                "id": id.to_hex(),
                "links_out": out,
                "links_in": ins,
                "link_keys_out": by_kind,
            }));
        }
        t.insert("per_id".into(), Value::Array(per_id));

        t.insert("symbols_many".into(), j(&ok(tx.symbols_many(&ids))));
        let briefs: Vec<Value> = ok(tx.symbol_briefs_many(&ids))
            .iter()
            .map(|b| match b {
                Some(b) => json!({"leaf": b.leaf, "facts": j(&b.facts)}),
                None => Value::Null,
            })
            .collect();
        t.insert("symbol_briefs_many".into(), Value::Array(briefs));
        let keys: Vec<LinkKey> = links.iter().map(|(k, _)| k.clone()).collect();
        let link_briefs: Vec<Value> = ok(tx.link_briefs_many(&keys))
            .iter()
            .map(|b| match b {
                Some(b) => json!({"confidence": j(&b.confidence), "weight": bits(b.weight)}),
                None => Value::Null,
            })
            .collect();
        t.insert("link_briefs_many".into(), Value::Array(link_briefs));
        let degrees: BTreeMap<String, u64> = ok(tx.in_degrees())
            .into_iter()
            .map(|(i, n)| (i.to_hex(), n))
            .collect();
        t.insert("in_degrees".into(), j(&degrees));

        // Exports and references: every namespace and key the corpus mentions.
        let mut export_keys = BTreeSet::new();
        let mut namespaces = BTreeSet::new();
        for (_, a) in &artifacts {
            for e in &a.exports {
                export_keys.insert(e.key.clone());
                namespaces.insert(e.key.namespace.clone());
            }
            for r in &a.refs {
                for c in &r.candidates {
                    export_keys.insert(c.clone());
                    namespaces.insert(c.namespace.clone());
                }
            }
        }
        let mut exports_in = serde_json::Map::new();
        for ns in &namespaces {
            exports_in.insert(ns.clone(), j(&ok(tx.exports_in(ns))));
        }
        t.insert("exports_in".into(), Value::Object(exports_in));
        let mut per_key = Vec::new();
        for k in &export_keys {
            per_key.push(json!({
                "key": j(k),
                "exports": j(&ok(tx.exports(k))),
                "refs_by_key": j(&ok(tx.refs_by_key(k))),
            }));
        }
        t.insert("per_key".into(), Value::Array(per_key));

        let unresolved = ok(tx.unresolved_refs(None));
        t.insert("unresolved_refs".into(), j(&unresolved));
        t.insert(
            "unresolved".into(),
            Value::Array(
                unresolved
                    .iter()
                    .map(|(r, _)| json!([j(r), j(&ok(tx.unresolved(*r)))]))
                    .collect(),
            ),
        );
        t.insert("index_errors_all".into(), j(&ok(tx.index_errors_all())));
        let contracts = ok(tx.contracts());
        t.insert("contracts".into(), j(&contracts));
        t.insert(
            "contract".into(),
            Value::Array(
                contracts
                    .iter()
                    .map(|(i, _)| json!([i.to_hex(), j(&ok(tx.contract(*i)))]))
                    .collect(),
            ),
        );
        t.insert(
            "bindings_of".into(),
            Value::Array(
                artifacts
                    .iter()
                    .map(|(i, _)| json!([i.to_hex(), j(&ok(tx.bindings_of(*i)))]))
                    .collect(),
            ),
        );

        // Postings: every token of every symbol name, and the first three characters of each.
        let mut terms = BTreeSet::new();
        for (_, s) in &symbols {
            terms.extend(tokenize(&s.name.canonical()));
        }
        let mut post = Vec::new();
        let mut prefixes = BTreeSet::new();
        for term in &terms {
            let ps: Vec<Value> = ok(tx.postings(term)).iter().map(posting).collect();
            post.push(json!([term, ps]));
            if term.chars().count() >= 3 {
                prefixes.insert(term.chars().take(3).collect::<String>());
            }
        }
        t.insert("postings".into(), Value::Array(post));
        let mut pre = Vec::new();
        for p in &prefixes {
            let rows: Vec<Value> = ok(tx.postings_prefix(p, 8))
                .iter()
                .map(|(term, ps)| json!([term, ps.iter().map(posting).collect::<Vec<_>>()]))
                .collect();
            pre.push(json!([p, rows]));
        }
        t.insert("postings_prefix".into(), Value::Array(pre));

        t.insert(
            "doc_len".into(),
            Value::Array(
                symbols
                    .iter()
                    .map(|(i, _)| json!([i.to_hex(), j(&ok(tx.doc_len(*i)))]))
                    .collect(),
            ),
        );
        let st = ok(tx.lexicon_stats());
        t.insert(
            "lexicon_stats".into(),
            json!({"docs": st.docs, "total_len": st.total_len}),
        );
        t.insert("shard_hashes".into(), j(&ok(tx.shard_hashes())));
        let rank = ok(tx.cached_rank(RANK_CONTEXT)).map(|m| {
            m.into_iter()
                .map(|(i, f)| (i.to_hex(), bits(f)))
                .collect::<BTreeMap<_, _>>()
        });
        t.insert("cached_rank".into(), j(&rank));
        t.insert(
            "canonical_graph".into(),
            Value::String(katalogr_engine::canonical_graph(tx)),
        );
        Value::Object(t)
    }

    #[derive(Deserialize)]
    struct Repo {
        name: String,
        path: String,
    }
    #[derive(Deserialize)]
    struct Query {
        id: String,
        op: String,
        input: Option<String>,
    }
    #[derive(Deserialize)]
    struct Golden {
        set: String,
        #[serde(default)]
        repos: Vec<Repo>,
        queries: Vec<Query>,
    }

    /// `(cases, search)`: the transcript per case and the search hits per eval query.
    fn compute() -> (Value, Value) {
        let root = root();
        let fx = root.join("fixtures");
        let manifest = load_manifest(&fx).unwrap();
        let dirs: BTreeSet<&String> = manifest
            .technologies
            .values()
            .flat_map(|p| p.values().flatten())
            .collect();
        let mut cases = serde_json::Map::new();
        for dir in dirs {
            let case = Case::open(&full_registry, &fx.join(dir)).unwrap();
            cases.insert(dir.clone(), case.read(|tx| transcript(tx)).unwrap());
        }
        let eval = fx.join("eval");
        let mut search = serde_json::Map::new();
        for set in ["core", "cross"] {
            let text = std::fs::read_to_string(eval.join(set).join("golden.yaml")).unwrap();
            let g: Golden = serde_yaml_ng::from_str(&text).unwrap();
            let dirs: Vec<(String, Utf8PathBuf)> = g
                .repos
                .iter()
                .map(|r| (r.name.clone(), eval.join(set).join(&r.path)))
                .collect();
            let repos: Vec<(&str, &Utf8Path)> = dirs
                .iter()
                .map(|(n, p)| (n.as_str(), p.as_path()))
                .collect();
            let case = Case::open_repos(&full_registry, &repos).unwrap();
            cases.insert(
                format!("eval/{}", g.set),
                case.read(|tx| transcript(tx)).unwrap(),
            );
            let mut hits = serde_json::Map::new();
            for q in g.queries.iter().filter(|q| q.op == "search") {
                let input = q.input.as_deref().unwrap();
                let found = case
                    .read(|tx| katalogr_query::search(tx, input, 20))
                    .unwrap()
                    .unwrap();
                hits.insert(q.id.clone(), j(&found));
            }
            search.insert(g.set.clone(), Value::Object(hits));
        }
        (Value::Object(cases), Value::Object(search))
    }

    fn computed() -> &'static (Value, Value) {
        static ONCE: OnceLock<(Value, Value)> = OnceLock::new();
        ONCE.get_or_init(compute)
    }

    /// Compare one part of the golden with the computed value; bless rewrites the whole file.
    fn check(part: &str) {
        let (cases, search) = computed();
        let actual = json!({"cases": cases, "search": search});
        let path = golden_path();
        if std::env::var_os("KATALOGR_BLESS").is_some_and(|v| v == "1") {
            let mut text = serde_json::to_string_pretty(&actual).unwrap();
            text.push('\n');
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, text).unwrap();
            std::fs::rename(&tmp, &path).unwrap();
            return;
        }
        let want: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        if want[part] != actual[part] {
            let w = serde_json::to_string_pretty(&want[part]).unwrap();
            let a = serde_json::to_string_pretty(&actual[part]).unwrap();
            let first = w
                .lines()
                .zip(a.lines())
                .enumerate()
                .find(|(_, (x, y))| x != y)
                .map(|(n, (x, y))| format!("line {}: golden `{x}` vs actual `{y}`", n + 1))
                .unwrap_or_else(|| "length differs".into());
            panic!("read-API {part} differs from tests/golden/read_api.json: {first}");
        }
    }

    #[test]
    fn transcript_matches_golden() {
        check("cases");
    }

    #[test]
    fn search_answers_unchanged() {
        check("search");
    }
}
