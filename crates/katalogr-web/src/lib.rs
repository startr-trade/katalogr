//! The embedded read-only web view: a page, its scripts and styles, and the vendored graph
//! library, all text embedded in the binary. Nothing is fetched at build or run time.

pub mod manifest;

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One embedded file. `text` is valid UTF-8: every asset is text.
pub struct Asset {
    pub name: &'static str,
    pub mime: &'static str,
    pub text: &'static str,
}

/// The manifest text, as embedded in the page.
pub const VIEWS_JSON: &str = include_str!("../assets/web/views.json");
/// The version of the vendored graph library.
pub const CYTOSCAPE_VERSION: &str = "3.34.4";

const INDEX_HTML: &str = include_str!("../assets/web/index.html");

const JS: &str = "text/javascript";
const CSS: &str = "text/css";
const TXT: &str = "text/plain";

/// Scripts of the page after Pele, in load order.
const SCRIPT_NAMES: [&str; 8] = [
    "cytoscape.min.js",
    "kw-core.js",
    "kw-dom.js",
    "kw-diagram.js",
    "kw-explore.js",
    "kw-lineage.js",
    "kw-views.js",
    "kw-main.js",
];

fn table() -> &'static (Vec<Asset>, Vec<String>) {
    static TABLE: OnceLock<(Vec<Asset>, Vec<String>)> = OnceLock::new();
    TABLE.get_or_init(|| {
        use katalogr_render::assets as pele;
        let utf8 = |b: &'static [u8]| std::str::from_utf8(b).expect("embedded asset is UTF-8");
        let assets = vec![
            Asset {
                name: pele::PELE_THEME_CSS_NAME,
                mime: CSS,
                text: utf8(pele::PELE_THEME_CSS),
            },
            Asset {
                name: pele::PELE_JS_NAME,
                mime: JS,
                text: utf8(pele::PELE_JS),
            },
            Asset {
                name: pele::PELE_INIT_JS_NAME,
                mime: JS,
                text: utf8(pele::PELE_INIT_JS),
            },
            Asset {
                name: "app.css",
                mime: CSS,
                text: include_str!("../assets/web/app.css"),
            },
            Asset {
                name: "cytoscape.min.js",
                mime: JS,
                text: include_str!("../assets/cytoscape/cytoscape.min.js"),
            },
            Asset {
                name: "kw-core.js",
                mime: JS,
                text: include_str!("../assets/web/kw-core.js"),
            },
            Asset {
                name: "kw-dom.js",
                mime: JS,
                text: include_str!("../assets/web/kw-dom.js"),
            },
            Asset {
                name: "kw-diagram.js",
                mime: JS,
                text: include_str!("../assets/web/kw-diagram.js"),
            },
            Asset {
                name: "kw-explore.js",
                mime: JS,
                text: include_str!("../assets/web/kw-explore.js"),
            },
            Asset {
                name: "kw-lineage.js",
                mime: JS,
                text: include_str!("../assets/web/kw-lineage.js"),
            },
            Asset {
                name: "kw-views.js",
                mime: JS,
                text: include_str!("../assets/web/kw-views.js"),
            },
            Asset {
                name: "kw-main.js",
                mime: JS,
                text: include_str!("../assets/web/kw-main.js"),
            },
            Asset {
                name: "views.json",
                mime: TXT,
                text: VIEWS_JSON,
            },
            Asset {
                name: "cytoscape-LICENSE",
                mime: TXT,
                text: include_str!("../assets/cytoscape/LICENSE"),
            },
        ];
        let etags = assets.iter().map(|a| compute_etag(a.text)).collect();
        (assets, etags)
    })
}

fn compute_etag(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

/// Every file of the bundle, in load order for scripts. Pele's files come from katalogr-render.
pub fn assets() -> &'static [Asset] {
    &table().0
}

/// The asset with this exact name, else None. A name holding `/` or `..` is simply not found.
pub fn asset(name: &str) -> Option<&'static Asset> {
    assets().iter().find(|a| a.name == name)
}

/// `"<first 16 hex of SHA-256 of text>"`, with the quotes. Computed once for each bundled asset
/// and copied out; an `Asset` that is not in the bundle gets its tag computed on the call.
pub fn etag(asset: &Asset) -> String {
    let (assets, etags) = table();
    match assets.iter().position(|a| a.name == asset.name) {
        Some(i) => etags[i].clone(),
        None => compute_etag(asset.text),
    }
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The HTML page that links the assets under `assets/`. `api_base` goes in the `katalogr-api`
/// meta tag, HTML-escaped; empty means the same origin.
pub fn page_linked(api_base: &str) -> String {
    let api = format!(
        "<meta name=\"katalogr-api\" content=\"{}\">",
        escape_attr(api_base)
    );
    let head = format!(
        "<link rel=\"icon\" href=\"data:,\">\n{}\n<link rel=\"stylesheet\" href=\"assets/app.css\">",
        katalogr_render::assets::head_tags(Some("assets/"))
    );
    let manifest = format!(
        "<script type=\"application/json\" id=\"kw-views\">{}</script>",
        VIEWS_JSON.trim_end().replace('<', "\\u003c")
    );
    let scripts: Vec<String> = SCRIPT_NAMES
        .iter()
        .map(|n| format!("<script src=\"assets/{n}\"></script>"))
        .collect();
    INDEX_HTML
        .replace("<!--KW:API-->", &api)
        .replace("<!--KW:HEAD-->", &head)
        .replace("<!--KW:MANIFEST-->", &manifest)
        .replace("<!--KW:SCRIPTS-->", &scripts.join("\n"))
}

/// Writes `index.html` and `assets/<name>` for every asset into `dir`, creating it. Overwrites.
pub fn write_bundle(dir: &Path, api_base: &str) -> std::io::Result<Vec<PathBuf>> {
    let assets_dir = dir.join("assets");
    std::fs::create_dir_all(&assets_dir)?;
    let mut written = Vec::new();
    let index = dir.join("index.html");
    std::fs::write(&index, page_linked(api_base))?;
    written.push(index);
    for a in assets() {
        let path = assets_dir.join(a.name);
        std::fs::write(&path, a.text)?;
        written.push(path);
    }
    Ok(written)
}
