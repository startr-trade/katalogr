//! The katalogr brand is present and wired in (FEAT-108).

mod archtest {
    use std::fs;
    use std::path::{Path, PathBuf};

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read(rel: &str) -> String {
        fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    const SVGS: &[&str] = &[
        "assets/brand/katalogr-mark.svg",
        "assets/brand/katalogr-favicon.svg",
        "assets/brand/katalogr-app-icon.svg",
        "assets/brand/katalogr-lockup-light.svg",
        "assets/brand/katalogr-lockup-dark.svg",
        "assets/brand/katalogr-social-preview.svg",
        "docs/theme/favicon.svg",
        "docs/src/images/brand/katalogr-lockup-light.svg",
        "docs/src/images/brand/katalogr-lockup-dark.svg",
    ];

    const PNGS: &[&str] = &[
        "assets/brand/apple-touch-icon.png",
        "assets/brand/katalogr-social-preview.png",
        "docs/theme/favicon.png",
    ];

    /// R-1: every brand file exists; each SVG is one well-formed `<svg>` element with a
    /// `viewBox` and an `aria-label`; each PNG has the PNG signature; and the README, the docs
    /// introduction and `book.toml` reference them.
    #[test]
    fn brand_assets_present() {
        for rel in SVGS {
            let svg = read(rel);
            let body = svg.trim();
            let start = body
                .find("<svg")
                .unwrap_or_else(|| panic!("{rel}: no <svg>"));
            assert!(body.ends_with("</svg>"), "{rel}: does not end with </svg>");
            let open = &body[start..body[start..].find('>').map(|i| start + i).unwrap()];
            assert!(open.contains("viewBox=\""), "{rel}: no viewBox");
            assert!(open.contains("aria-label=\""), "{rel}: no aria-label");
            assert_eq!(
                body.matches("<svg").count(),
                body.matches("</svg>").count(),
                "{rel}: unbalanced <svg>"
            );
        }
        for rel in PNGS {
            let bytes = fs::read(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
            assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "{rel}: not a PNG");
        }
        let readme = read("README.md");
        assert!(readme.contains("assets/brand/katalogr-lockup-light.svg"));
        assert!(readme.contains("assets/brand/katalogr-lockup-dark.svg"));
        let intro = read("docs/src/introduction.md");
        assert!(intro.contains("images/brand/katalogr-lockup-light.svg"));
        assert!(intro.contains("images/brand/katalogr-lockup-dark.svg"));
        let book = read("docs/book.toml");
        assert!(
            book.contains("\"brand.css\""),
            "book.toml does not load brand.css"
        );
        assert!(read("docs/brand.css").contains(".brand-lockup"));
    }
}
