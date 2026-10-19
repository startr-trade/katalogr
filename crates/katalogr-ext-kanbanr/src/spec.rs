//! Item specs: `projects/<p>/features/**/features-spec/<CODE>.md`.

use crate::Build;
use katalogr_model::ExportKey;
use katalogr_model::annotate::ITEM_NS;

pub(crate) fn extract(b: &mut Build<'_>) {
    b.markdown(b.text);
    let stem = b
        .path
        .as_str()
        .rsplit('/')
        .next()
        .and_then(|f| f.strip_suffix(".md"))
        .unwrap_or("")
        .to_string();
    if !stem.is_empty() {
        b.add_ref(
            b.aid,
            "specifies",
            None,
            vec![ExportKey {
                namespace: ITEM_NS.to_string(),
                key: stem,
            }],
        );
    }
}
