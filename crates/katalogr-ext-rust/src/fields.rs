//! Fields of code types (FEAT-101): the neutral `field_type`, `required` and `many` facts of a
//! struct, union or enum-variant field, read from the type as written.

/// What the type of a field says, in the neutral vocabulary of `katalogr_model::structure`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shape {
    pub field_type: String,
    pub required: bool,
    pub many: bool,
}

/// Standard-library and prelude type names that are never the project's own: no `uses-type`
/// reference is made to them. Sorted, unique.
pub(crate) const STD_TYPES: &[&str] = &[
    "Arc",
    "BTreeMap",
    "BTreeSet",
    "Box",
    "Bytes",
    "Cell",
    "Cow",
    "Duration",
    "HashMap",
    "HashSet",
    "IndexMap",
    "Instant",
    "Mutex",
    "Option",
    "PathBuf",
    "PhantomData",
    "Pin",
    "Rc",
    "RefCell",
    "Result",
    "RwLock",
    "String",
    "Vec",
    "VecDeque",
];

fn shape_of(field_type: &str, required: bool, many: bool) -> Shape {
    Shape {
        field_type: field_type.to_string(),
        required,
        many,
    }
}

/// `a  ::  b < c >` as `a::b<c>`: whitespace outside of words removed where it is not needed.
fn compact(s: &str) -> String {
    let mut out = String::new();
    for w in s.split_whitespace() {
        let needs_space = out
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
            && w.chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if needs_space {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// Splits `Name<A, B>` into `("Name", ["A", "B"])` at the top level; lifetimes are dropped.
/// A text with no generic part gives no arguments.
fn split_generic(s: &str) -> (String, Vec<String>) {
    let Some(open) = s.find('<') else {
        return (s.to_string(), Vec::new());
    };
    let name = s[..open].to_string();
    let inner_end = s.rfind('>').unwrap_or(s.len());
    let inner = &s[open + 1..inner_end.max(open + 1)];
    let mut args = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, c) in inner.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                args.push(inner[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    args.push(inner[start..].trim().to_string());
    args.retain(|a| !a.is_empty() && !a.starts_with('\''));
    (name, args)
}

/// The array element of `[T; N]` or `[T]`, without the length.
fn element(inner: &str) -> &str {
    let (mut depth, mut cut) = (0i32, inner.len());
    for (i, c) in inner.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ';' if depth == 0 => {
                cut = i;
                break;
            }
            _ => {}
        }
    }
    inner[..cut].trim()
}

/// The shape of the type `type_text`, as written.
pub(crate) fn shape(type_text: &str) -> Shape {
    let t = compact(type_text);
    let mut t = t.trim();
    // References: `&T`, `&'a T`, `&mut T`.
    if let Some(rest) = t.strip_prefix('&') {
        let mut rest = rest.trim_start();
        if rest.starts_with('\'') {
            rest = rest[1..]
                .trim_start_matches(|c: char| c.is_alphanumeric() || c == '_')
                .trim_start();
        }
        rest = rest.strip_prefix("mut ").unwrap_or(rest).trim_start();
        return shape(rest);
    }
    // Arrays and slices.
    if let Some(inner) = t.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        let elem = element(inner);
        if elem == "u8" {
            return shape_of("binary", true, false);
        }
        let mut s = shape(elem);
        s.required = true;
        s.many = true;
        return s;
    }
    if t.starts_with('(') || t.starts_with("dyn ") || t.starts_with("impl ") || t.starts_with("fn")
    {
        return shape_of(t, true, false);
    }
    t = t.trim_start_matches("::");
    let (path, args) = split_generic(t);
    let name = path.rsplit("::").next().unwrap_or("").trim();
    let first = || args.first().map(String::as_str).unwrap_or("");
    match name {
        "Option" => {
            let mut s = shape(first());
            s.required = false;
            s
        }
        "Vec" if first() == "u8" => shape_of("binary", true, false),
        "Bytes" => shape_of("binary", true, false),
        "Vec" | "VecDeque" | "HashSet" | "BTreeSet" => {
            let mut s = shape(first());
            s.required = true;
            s.many = true;
            s
        }
        "HashMap" | "BTreeMap" | "IndexMap" => shape_of("object", true, false),
        "String" | "str" | "char" => shape_of("string", true, false),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => shape_of("integer", true, false),
        "f32" | "f64" => shape_of("number", true, false),
        "bool" => shape_of("boolean", true, false),
        "Box" | "Arc" | "Rc" | "Cow" => shape(first()),
        _ => shape_of(&path, true, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_types_sorted_unique() {
        assert!(STD_TYPES.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn shapes() {
        let s = |t: &str| {
            let s = shape(t);
            (s.field_type, s.required, s.many)
        };
        assert_eq!(s("Option<String>"), ("string".into(), false, false));
        assert_eq!(
            s("Vec<crate::model::Line>"),
            ("crate::model::Line".into(), true, true)
        );
        assert_eq!(s("Vec<u8>"), ("binary".into(), true, false));
        assert_eq!(s("&'a [u8]"), ("binary".into(), true, false));
        assert_eq!(s("[Line; 4]"), ("Line".into(), true, true));
        assert_eq!(s("Option<Vec<i32>>"), ("integer".into(), false, true));
        assert_eq!(s("Box<Order>"), ("Order".into(), true, false));
        assert_eq!(s("Cow<'_, str>"), ("string".into(), true, false));
        assert_eq!(s("HashMap<String, Order>"), ("object".into(), true, false));
        assert_eq!(s("&mut f64"), ("number".into(), true, false));
    }
}
