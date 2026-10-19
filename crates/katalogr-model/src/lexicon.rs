//! Identifier-aware tokenisation shared by the index (postings) and the query side (FEAT-025).
//!
//! Deterministic and language-neutral: no stemming, no synonyms, no models.

/// A searchable part of a symbol. Weights are fixed (BM25F style).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    Name,
    Doc,
    Section,
    Contract,
}

impl Field {
    /// Every field, in index order.
    pub const ALL: [Field; 4] = [Field::Name, Field::Doc, Field::Section, Field::Contract];

    /// Position in per-field arrays and the byte stored in postings keys.
    pub fn index(self) -> usize {
        match self {
            Field::Name => 0,
            Field::Doc => 1,
            Field::Section => 2,
            Field::Contract => 3,
        }
    }

    pub fn from_index(i: usize) -> Option<Field> {
        Field::ALL.get(i).copied()
    }

    /// The field's name as printed in `matched`.
    pub fn label(self) -> &'static str {
        match self {
            Field::Name => "name",
            Field::Doc => "doc",
            Field::Section => "section",
            Field::Contract => "contract",
        }
    }

    /// Fixed field weight: name 3.0, contract 2.0, section 1.5, doc 1.0.
    pub fn weight(self) -> f64 {
        match self {
            Field::Name => 3.0,
            Field::Contract => 2.0,
            Field::Section => 1.5,
            Field::Doc => 1.0,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Class {
    Lower,
    Upper,
    Digit,
}

fn class(c: char) -> Class {
    if c.is_numeric() {
        Class::Digit
    } else if c.is_uppercase() {
        Class::Upper
    } else {
        Class::Lower
    }
}

/// Words of one alphanumeric run, split at lower-to-upper, acronym-end and letter/digit
/// boundaries.
fn run_words(run: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for (i, &c) in run.iter().enumerate() {
        if i > 0 {
            let prev = class(run[i - 1]);
            let here = class(c);
            let next_lower = run.get(i + 1).is_some_and(|n| class(*n) == Class::Lower);
            let boundary = match (prev, here) {
                (Class::Lower, Class::Upper) => true,
                (Class::Upper, Class::Upper) => next_lower,
                (a, b) => (a == Class::Digit) != (b == Class::Digit),
            };
            if boundary {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn keep(word: &str) -> bool {
    word.chars().count() >= 2 || word.chars().all(|c| c.is_numeric())
}

/// Split on non-alphanumerics, then camelCase / PascalCase / acronym / digit boundaries; lowercase;
/// drop one-letter words (a lone digit is kept). Each whitespace-separated identifier that splits
/// into several words also yields the whole identifier, lowercased and joined, after its words.
///
/// `"applyRateCode"` gives `apply, rate, code, applyratecode`; `"HTTPServer2"` gives
/// `http, server, 2, httpserver2`.
pub fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in s.split_whitespace() {
        let mut words = Vec::new();
        let mut run: Vec<char> = Vec::new();
        for c in chunk.chars().chain(std::iter::once(' ')) {
            if c.is_alphanumeric() {
                run.push(c);
            } else if !run.is_empty() {
                words.extend(run_words(&run));
                run.clear();
            }
        }
        let whole: String = words.concat();
        let several = words.len() > 1;
        out.extend(words.into_iter().filter(|w| keep(w)));
        if several && keep(&whole) {
            out.push(whole);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Vec<String> {
        tokenize(s)
    }

    #[test]
    fn tokenize_forms() {
        assert_eq!(
            t("applyRateCode"),
            ["apply", "rate", "code", "applyratecode"]
        );
        assert_eq!(t("HTTPServer2"), ["http", "server", "2", "httpserver2"]);
        assert_eq!(
            t("snake_case_name"),
            ["snake", "case", "name", "snakecasename"]
        );
        assert_eq!(t("kebab-case"), ["kebab", "case", "kebabcase"]);
        assert_eq!(
            t("a.b.config"),
            ["config", "abconfig"],
            "one-letter words are dropped"
        );
        assert_eq!(
            t("pkg.util.Parse"),
            ["pkg", "util", "parse", "pkgutilparse"]
        );
        assert_eq!(t("rate"), ["rate"]);
        assert_eq!(t("rate code"), ["rate", "code"]);
        assert_eq!(t("PascalCase"), ["pascal", "case", "pascalcase"]);
        assert_eq!(t("v2"), ["2", "v2"], "a lone digit is kept");
        assert!(t("").is_empty());
        assert!(t("__ -- ..").is_empty());
        assert_eq!(t("Éclair_Crème"), ["éclair", "crème", "éclaircrème"]);
    }

    #[test]
    fn field_order_and_weights() {
        for (i, f) in Field::ALL.iter().enumerate() {
            assert_eq!(f.index(), i);
            assert_eq!(Field::from_index(i), Some(*f));
        }
        assert!(Field::Name.weight() > Field::Contract.weight());
        assert!(Field::Contract.weight() > Field::Section.weight());
        assert!(Field::Section.weight() > Field::Doc.weight());
    }
}
