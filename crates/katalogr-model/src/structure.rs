//! Neutral names of the facts that describe a structural field (a member of a type).
//! Extensions that model structures set them; `mirror_drift` compares them.

/// Str: a primitive word or a type key `{ns}Name`.
pub const FIELD_TYPE: &str = "field_type";
/// Bool.
pub const REQUIRED: &str = "required";
/// Bool: a list or repeated member.
pub const MANY: &str = "many";
/// Primitive words an extension maps its own scalar types onto.
pub const PRIMITIVES: &[&str] = &[
    "any", "binary", "boolean", "date", "datetime", "integer", "number", "object", "string", "time",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_sorted_unique() {
        assert!(PRIMITIVES.windows(2).all(|w| w[0] < w[1]));
    }
}
