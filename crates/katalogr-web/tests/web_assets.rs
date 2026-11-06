//! Static checks of the embedded bundle (FEAT-033 T2).

mod web_assets {
    use katalogr_web::{asset, assets, etag, page_linked, write_bundle};
    use sha2::{Digest, Sha256};
    use std::path::{Path, PathBuf};

    const SUMS: &str = include_str!("../assets/cytoscape/SHA256SUMS");

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
    }

    fn hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn pinned(file: &str) -> String {
        SUMS.lines()
            .filter_map(|l| l.split_once("  "))
            .find(|(_, f)| *f == file)
            .map(|(h, _)| h.to_string())
            .unwrap_or_else(|| panic!("{file} not in SHA256SUMS"))
    }

    /// Where an asset's source lives, for the files of this crate.
    fn source(name: &str) -> Option<PathBuf> {
        match name {
            "cytoscape.min.js" => Some(root().join("cytoscape/cytoscape.min.js")),
            "cytoscape-LICENSE" => Some(root().join("cytoscape/LICENSE")),
            "pele-theme.css" | "pele.global.js" | "pele-init.js" => None,
            n => Some(root().join("web").join(n)),
        }
    }

    fn own_files() -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = assets()
            .iter()
            .filter(|a| {
                source(a.name).is_some()
                    && a.name != "cytoscape-LICENSE"
                    && a.name != "cytoscape.min.js"
            })
            .map(|a| (a.name.to_string(), a.text.to_string()))
            .collect();
        v.push(("index.html".into(), page_linked("")));
        v
    }

    #[test]
    fn vendored_matches_pin() {
        let js = std::fs::read(root().join("cytoscape/cytoscape.min.js")).unwrap();
        let lic = std::fs::read(root().join("cytoscape/LICENSE")).unwrap();
        assert_eq!(hex(&js), pinned("cytoscape.min.js"));
        assert_eq!(hex(&lic), pinned("LICENSE"));
        assert_eq!(
            hex(asset("cytoscape.min.js").unwrap().text.as_bytes()),
            pinned("cytoscape.min.js")
        );
    }

    #[test]
    fn licence_present() {
        let lic = asset("cytoscape-LICENSE").expect("licence is served");
        assert!(lic.text.contains("MIT"), "licence names MIT");
        assert!(lic.text.contains("Permission is hereby granted"));
    }

    #[test]
    fn no_remote_reference() {
        const ALLOWED: [&str; 3] = [
            "http://engelschall.com",
            "http://en.wikipedia.org/wiki/MIT_License",
            "http://opensource.org/licenses/MIT",
        ];
        let cy = asset("cytoscape.min.js").unwrap().text;
        for (i, _) in cy.match_indices("http") {
            let rest = &cy[i..];
            if rest.starts_with("http://") || rest.starts_with("https://") {
                assert!(
                    ALLOWED.iter().any(|u| rest.starts_with(u)),
                    "remote URL in cytoscape: {}",
                    &rest[..rest.len().min(60)]
                );
            }
        }
        for banned in [
            "fetch(",
            "XMLHttpRequest",
            "importScripts",
            "import(",
            "new Function",
            "eval(",
        ] {
            assert!(!cy.contains(banned), "cytoscape contains {banned}");
        }
        for (name, text) in own_files() {
            assert!(
                !text.contains("http://") && !text.contains("https://"),
                "{name} holds a remote reference"
            );
            assert!(
                !text.contains("importScripts") && !text.contains("XMLHttpRequest"),
                "{name} holds a banned call"
            );
            assert!(
                !text.contains("new Function") && !text.contains("eval("),
                "{name} evaluates text"
            );
        }
    }

    #[test]
    fn served_bytes_equal_source() {
        for a in assets() {
            match source(a.name) {
                Some(path) => {
                    let disk = std::fs::read(&path).unwrap();
                    assert_eq!(disk, a.text.as_bytes(), "{}", a.name);
                }
                None => {
                    use katalogr_render::assets as p;
                    let want: &[u8] = match a.name {
                        n if n == p::PELE_JS_NAME => p::PELE_JS,
                        n if n == p::PELE_INIT_JS_NAME => p::PELE_INIT_JS,
                        n if n == p::PELE_THEME_CSS_NAME => p::PELE_THEME_CSS,
                        n => panic!("unexpected asset {n}"),
                    };
                    assert_eq!(want, a.text.as_bytes(), "{}", a.name);
                }
            }
            assert!(asset(a.name).is_some());
            assert!(etag(a).starts_with('"') && etag(a).len() == 18);
        }
        assert!(asset("../app.css").is_none());
        assert!(asset("a/b").is_none());
        let dir = tempfile_dir();
        let written = write_bundle(&dir, "").unwrap();
        assert_eq!(written.len(), assets().len() + 1);
        for a in assets() {
            let disk = std::fs::read(dir.join("assets").join(a.name)).unwrap();
            assert_eq!(disk, a.text.as_bytes());
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("index.html")).unwrap(),
            page_linked("")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn tempfile_dir() -> PathBuf {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "bundle-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn assets_are_utf8() {
        // `Asset::text` is a `&str`, so the type already guarantees it; check the sources too.
        for a in assets() {
            if let Some(path) = source(a.name) {
                String::from_utf8(std::fs::read(&path).unwrap())
                    .unwrap_or_else(|_| panic!("{} is not UTF-8", a.name));
            }
            assert!(!a.text.contains('\0'), "{} holds NUL", a.name);
        }
        assert!(page_linked("").is_char_boundary(0));
    }

    #[test]
    fn api_base_is_escaped() {
        let page = page_linked("\"><script>alert(1)</script>&'");
        assert!(!page.contains("<script>alert"));
        assert!(
            page.contains("content=\"&quot;&gt;&lt;script&gt;alert(1)&lt;/script&gt;&amp;&#39;\"")
        );
        assert!(page_linked("").contains("<meta name=\"katalogr-api\" content=\"\">"));
        for m in [
            "<!--KW:API-->",
            "<!--KW:HEAD-->",
            "<!--KW:MANIFEST-->",
            "<!--KW:SCRIPTS-->",
        ] {
            assert!(!page_linked("").contains(m), "{m} left in the page");
        }
    }

    #[test]
    fn only_setsvg_writes_markup() {
        for a in assets().iter().filter(|a| a.name.starts_with("kw-")) {
            let n = a.text.matches("innerHTML").count()
                + a.text.matches("outerHTML").count()
                + a.text.matches("insertAdjacentHTML").count()
                + a.text.matches("document.write").count();
            if a.name == "kw-dom.js" {
                assert!(n <= 1, "kw-dom.js writes markup {n} times");
            } else {
                assert_eq!(n, 0, "{} writes markup", a.name);
            }
        }
    }

    #[test]
    fn bundle_within_budget() {
        let own: usize = own_files().iter().map(|(_, t)| t.len()).sum();
        assert!(own <= 150 * 1024, "own files are {own} bytes");
        let all: usize =
            assets().iter().map(|a| a.text.len()).sum::<usize>() + page_linked("").len();
        assert!(all <= 1300 * 1024, "served text is {all} bytes");
    }
}
