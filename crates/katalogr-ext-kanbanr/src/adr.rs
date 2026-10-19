//! Architecture decision records: `projects/<p>/docs/decisions/*.md` with YAML front matter.

use crate::{Build, find_line, str_fact};
use katalogr_model::annotate::{ADR_NS, ITEM_NS};
use katalogr_model::{ExportKey, ExtractError, Fact};
use serde_yaml_ng::Value;
use std::collections::BTreeMap;

pub(crate) fn extract(b: &mut Build<'_>) -> Result<(), ExtractError> {
    let parsed = b.markdown(b.text);
    let Some(front) = parsed.front_matter else {
        return Ok(());
    };
    let doc: Value = serde_yaml_ng::from_str(&front).map_err(|e| ExtractError::Parse {
        span: None,
        message: format!("front matter: {e}"),
    })?;
    let Some(id) = doc.get("id").and_then(Value::as_str).map(str::to_string) else {
        return Ok(());
    };
    let mut facts: BTreeMap<String, Fact> = BTreeMap::new();
    for key in ["status", "date"] {
        if let Some(f) = str_fact(&doc, key) {
            facts.insert(key.to_string(), f);
        }
    }
    if let Some(title) = &parsed.title {
        facts.insert("title".to_string(), Fact::Str(title.clone()));
    }
    let whole = b.whole();
    let (adr, _) = b.symbol(&[], "adr", &id, whole, facts);
    b.export(ADR_NS, id.clone(), adr);
    let affects_line = find_line(b.text, 1, "affects:").unwrap_or(1);
    for code in doc
        .get("affects")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let line = find_line(b.text, affects_line + 1, code);
        b.add_ref(
            adr,
            "affects",
            line.map(|l| b.line_span(l)),
            vec![ExportKey {
                namespace: ITEM_NS.to_string(),
                key: code.to_string(),
            }],
        );
    }
    Ok(())
}
