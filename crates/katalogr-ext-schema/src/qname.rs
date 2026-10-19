//! Keys and type words shared by the XSD and JSON Schema walkers.

/// Clark notation, always braced: `{urn:a}Order`, `{}Order` when there is no namespace.
pub fn clark(ns: &str, local: &str) -> String {
    format!("{{{ns}}}{local}")
}

/// The local part of a Clark key or a primitive word (text after the last '}').
pub fn local(key: &str) -> &str {
    key.rsplit_once('}').map_or(key, |(_, l)| l)
}

/// XSD built-in local name (`string`, `int`, `dateTime`, ...) to a primitive word; None when
/// it is not built in.
pub fn xsd_primitive(local: &str) -> Option<&'static str> {
    Some(match local {
        "string" | "normalizedString" | "token" | "anyURI" | "QName" | "NCName" | "Name" | "ID"
        | "IDREF" | "language" | "duration" | "gYear" | "gYearMonth" | "gMonth" | "gMonthDay"
        | "gDay" => "string",
        "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger"
        | "positiveInteger" | "negativeInteger" | "nonPositiveInteger" | "unsignedInt"
        | "unsignedLong" | "unsignedShort" | "unsignedByte" => "integer",
        "decimal" | "float" | "double" => "number",
        "boolean" => "boolean",
        "date" => "date",
        "dateTime" => "datetime",
        "time" => "time",
        "base64Binary" | "hexBinary" => "binary",
        "anyType" | "anySimpleType" => "any",
        // Every other built-in is text.
        "IDREFS" | "ENTITY" | "ENTITIES" | "NMTOKEN" | "NMTOKENS" | "NOTATION"
        | "anyAtomicType" | "dateTimeStamp" | "dayTimeDuration" | "yearMonthDuration" => "string",
        _ => return None,
    })
}

/// JSON Schema `type` and optional `format` to a primitive word.
pub fn json_primitive(ty: &str, format: Option<&str>) -> &'static str {
    match ty {
        "string" => match format {
            Some("date") => "date",
            Some("date-time") => "datetime",
            Some("time") => "time",
            Some("byte" | "binary") => "binary",
            _ => "string",
        },
        "integer" => "integer",
        "number" => "number",
        "boolean" => "boolean",
        "object" => "object",
        _ => "any",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use katalogr_model::structure::PRIMITIVES;

    #[test]
    fn xsd_primitive_table() {
        for (l, w) in [
            ("string", "string"),
            ("token", "string"),
            ("gYear", "string"),
            ("int", "integer"),
            ("unsignedByte", "integer"),
            ("decimal", "number"),
            ("double", "number"),
            ("boolean", "boolean"),
            ("date", "date"),
            ("dateTime", "datetime"),
            ("time", "time"),
            ("hexBinary", "binary"),
            ("anyType", "any"),
            ("anySimpleType", "any"),
            ("NMTOKEN", "string"),
        ] {
            assert_eq!(xsd_primitive(l), Some(w), "{l}");
        }
        assert_eq!(xsd_primitive("Order"), None);
        assert_eq!(xsd_primitive("String"), None, "names are case-sensitive");
        for l in ["string", "int", "dateTime", "anyType", "NMTOKEN"] {
            assert!(PRIMITIVES.contains(&xsd_primitive(l).unwrap()));
        }
        for (t, f, w) in [
            ("string", Some("date-time"), "datetime"),
            ("string", Some("date"), "date"),
            ("string", Some("uuid"), "string"),
            ("string", Some("binary"), "binary"),
            ("integer", None, "integer"),
            ("number", None, "number"),
            ("null", None, "any"),
        ] {
            assert_eq!(json_primitive(t, f), w);
            assert!(PRIMITIVES.contains(&w));
        }
    }

    #[test]
    fn clark_always_braced() {
        assert_eq!(clark("urn:a", "Order"), "{urn:a}Order");
        assert_eq!(clark("", "Order"), "{}Order");
        assert_eq!(local("{urn:a}Order"), "Order");
        assert_eq!(local("{}Order"), "Order");
        assert_eq!(local("string"), "string");
        assert_eq!(local("{https://h/p.json#x}A"), "A");
    }
}
