//! Static checks of the stylesheet's theme variables (FEAT-033 T4).

mod web_assets {
    use std::collections::HashMap;
    use std::path::Path;

    const APP_CSS: &str = include_str!("../assets/web/app.css");

    /// Path of Pele's theme file, next to the render crate's assets.
    fn pele_theme() -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../katalogr-render/assets/pele/pele-theme.css");
        std::fs::read_to_string(p).expect("pele-theme.css is readable")
    }

    /// Declarations inside the first `selector { ... }` block of `css`.
    fn block(css: &str, selector: &str) -> HashMap<String, String> {
        let start = css
            .find(&format!("{selector} {{"))
            .unwrap_or_else(|| panic!("{selector} block missing"));
        let body_start = start + css[start..].find('{').unwrap() + 1;
        let body_end = body_start + css[body_start..].find('}').unwrap();
        css[body_start..body_end]
            .split(';')
            .filter_map(|d| d.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .filter(|(k, _)| k.starts_with("--"))
            .collect()
    }

    /// Every `var(--name` reference in `css`, without the names Pele sets for itself.
    fn vars_read(css: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (i, _) in css.match_indices("var(--") {
            let rest = &css[i + 4..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .unwrap_or(rest.len());
            let name = &rest[..end];
            if !name.starts_with("--pele-") {
                out.push(name.to_string());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    const LISTED: [&str; 8] = [
        "--bg",
        "--fg",
        "--icons",
        "--quote-bg",
        "--table-alternate-bg",
        "--table-border-color",
        "--links",
        "--main-font",
    ];

    #[test]
    fn css_defines_every_variable_pele_reads() {
        for class in ["html.light", "html.coal"] {
            let vars = block(APP_CSS, class);
            for name in LISTED {
                assert!(vars.contains_key(name), "{class} defines {name}");
            }
            for name in vars_read(&pele_theme()) {
                assert!(
                    vars.contains_key(&name),
                    "{class} defines {name}, which pele-theme.css reads"
                );
            }
        }
    }

    fn luminance(hex: &str) -> f64 {
        let h = hex.trim_start_matches('#');
        assert_eq!(h.len(), 6, "six hex digits in {hex}");
        let c = |i: usize| {
            let v = u8::from_str_radix(&h[i..i + 2], 16).unwrap() as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * c(0) + 0.7152 * c(2) + 0.0722 * c(4)
    }

    fn contrast(a: &str, b: &str) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn palette_contrast_meets_aa() {
        for class in ["html.light", "html.coal"] {
            let vars = block(APP_CSS, class);
            let bg = &vars["--bg"];
            for fg in ["--fg", "--links"] {
                let ratio = contrast(&vars[fg], bg);
                assert!(ratio >= 4.5, "{class} {fg} on --bg is {ratio:.2}, needs 4.5");
            }
            let icons = contrast(&vars["--icons"], bg);
            assert!(icons >= 3.0, "{class} --icons on --bg is {icons:.2}, needs 3");
        }
    }
}
