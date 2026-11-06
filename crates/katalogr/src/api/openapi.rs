//! The OpenAPI 3.1 document, built from the op table and the `schema/v1` files (FEAT-049 R-3).
//! `schema/openapi/v1.json` holds it; `api::openapi_current` checks the file is current.

use serde_json::{Map, Value, json};

use super::ops::{COMMON, Op, Param, Ty};

/// The `schema/v1` file `name`, embedded in the binary.
fn schema_text(name: &str) -> Option<&'static str> {
    Some(match name {
        "callers" => include_str!("../../../../schema/v1/callers.schema.json"),
        "communities" => include_str!("../../../../schema/v1/communities.schema.json"),
        "contract" => include_str!("../../../../schema/v1/contract.schema.json"),
        "coverage" => include_str!("../../../../schema/v1/coverage.schema.json"),
        "cycles" => include_str!("../../../../schema/v1/cycles.schema.json"),
        "dangling" => include_str!("../../../../schema/v1/dangling.schema.json"),
        "deps" => include_str!("../../../../schema/v1/deps.schema.json"),
        "dead-code" => include_str!("../../../../schema/v1/dead-code.schema.json"),
        "docs-affected" => include_str!("../../../../schema/v1/docs-affected.schema.json"),
        "doctor" => include_str!("../../../../schema/v1/doctor.schema.json"),
        "explain" => include_str!("../../../../schema/v1/explain.schema.json"),
        "find" => include_str!("../../../../schema/v1/find.schema.json"),
        "hubs" => include_str!("../../../../schema/v1/hubs.schema.json"),
        "impact" => include_str!("../../../../schema/v1/impact.schema.json"),
        "lineage" => include_str!("../../../../schema/v1/lineage.schema.json"),
        "map" => include_str!("../../../../schema/v1/map.schema.json"),
        "mirrors" => include_str!("../../../../schema/v1/mirrors.schema.json"),
        "path" => include_str!("../../../../schema/v1/path.schema.json"),
        "position" => include_str!("../../../../schema/v1/position.schema.json"),
        "rationale" => include_str!("../../../../schema/v1/rationale.schema.json"),
        "refs" => include_str!("../../../../schema/v1/refs.schema.json"),
        "search" => include_str!("../../../../schema/v1/search.schema.json"),
        "show" => include_str!("../../../../schema/v1/show.schema.json"),
        "tests" => include_str!("../../../../schema/v1/tests.schema.json"),
        "view" => include_str!("../../../../schema/v1/view.schema.json"),
        "why" => include_str!("../../../../schema/v1/why.schema.json"),
        _ => return None,
    })
}

/// Point every `#/$defs/X` of a schema at `#/components/schemas/<name>.X`.
fn retarget(v: &mut Value, name: &str) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                match (k.as_str(), &*x) {
                    ("$ref", Value::String(r)) if r.starts_with("#/$defs/") => {
                        *x = json!(format!(
                            "#/components/schemas/{name}.{}",
                            &r["#/$defs/".len()..]
                        ));
                    }
                    _ => retarget(x, name),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| retarget(x, name)),
        _ => {}
    }
}

/// Add a schema file to `components` as `name`, with its definitions as `name.X`.
fn add_schema(components: &mut Map<String, Value>, name: &str) -> Option<String> {
    let mut schema: Value = serde_json::from_str(schema_text(name)?).ok()?;
    retarget(&mut schema, name);
    let obj = schema.as_object_mut()?;
    obj.remove("$schema");
    if let Some(Value::Object(defs)) = obj.remove("$defs") {
        for (k, d) in defs {
            components.insert(format!("{name}.{k}"), d);
        }
    }
    components.insert(name.to_string(), schema);
    Some(format!("#/components/schemas/{name}"))
}

fn param_schema(p: &Param) -> Value {
    let mut s = match p.ty {
        Ty::Str => json!({"type": "string"}),
        Ty::Int(min) => json!({"type": "integer", "minimum": min, "maximum": u32::MAX}),
        Ty::Float => json!({"type": "number", "minimum": 0}),
        Ty::Bool => json!({"type": "boolean"}),
        Ty::Enum(values) => json!({"type": "string", "enum": values}),
    };
    if let Some(d) = p.default {
        let typed = match p.ty {
            Ty::Int(_) | Ty::Float => d.parse::<f64>().ok().map(|f| json!(f)),
            Ty::Bool => Some(json!(d == "true")),
            _ => Some(json!(d)),
        };
        if let (Some(t), Some(o)) = (typed, s.as_object_mut()) {
            o.insert("default".into(), t);
        }
    }
    s
}

fn parameter(p: &Param) -> Value {
    json!({
        "name": p.name,
        "in": "query",
        "required": p.required,
        "description": p.help,
        "schema": param_schema(p),
    })
}

fn error_response(description: &str) -> Value {
    json!({
        "description": description,
        "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}},
    })
}

/// The document for `ops`.
pub fn document(ops: &[Op]) -> Value {
    let mut components = Map::new();
    components.insert(
        "Error".into(),
        json!({
            "type": "object",
            "required": ["schema", "error", "message"],
            "properties": {
                "schema": {"type": "integer"},
                "error": {"type": "string"},
                "message": {"type": "string"},
            },
        }),
    );
    let mut paths = Map::new();
    let meta_ref = add_schema(&mut components, "contract");
    let ok_json = |schema: Option<String>| -> Value {
        match schema {
            Some(r) => json!({
                "description": "The same JSON as the CLI prints with `--json`.",
                "headers": {"ETag": {"description": "The view ID, when the answer is exact", "schema": {"type": "string"}}},
                "content": {"application/json": {"schema": {"$ref": r}}},
            }),
            None => json!({
                "description": "The same JSON as the CLI prints with `--json`.",
                "headers": {"ETag": {"description": "The view ID, when the answer is exact", "schema": {"type": "string"}}},
                "content": {"application/json": {"schema": {"type": "object"}}},
            }),
        }
    };
    paths.insert(
        "/api/v1/meta".into(),
        json!({"get": {
            "operationId": "meta",
            "summary": "What this server serves: versions and capabilities.",
            "responses": {"200": ok_json(meta_ref)},
        }}),
    );
    paths.insert(
        "/api/v1/openapi.json".into(),
        json!({"get": {
            "operationId": "openapi",
            "summary": "This document.",
            "responses": {"200": {"description": "The OpenAPI document."}},
        }}),
    );
    for op in ops {
        let schema = op.schema.and_then(|s| add_schema(&mut components, s));
        let parameters: Vec<Value> = op.params.iter().chain(COMMON).map(parameter).collect();
        paths.insert(
            format!("/api/v1/{}", op.name),
            json!({"get": {
                "operationId": op.name,
                "summary": op.summary,
                "parameters": parameters,
                "responses": {
                    "200": ok_json(schema),
                    "304": {"description": "The view ID in `If-None-Match` is the current one."},
                    "400": error_response("A bad or unknown parameter."),
                    "401": error_response("Not authenticated."),
                    "403": error_response("Denied, or an unexpected Host header."),
                    "404": error_response("No match, or no such route."),
                    "405": error_response("Only GET is served."),
                    "409": error_response("Ambiguous, or the position is unavailable."),
                    "431": error_response("The request head is over 8 KiB."),
                    "500": error_response("An error."),
                    "503": error_response("The queue is full; retry after 1 second."),
                },
            }}),
        );
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "katalogr API",
            "version": "1",
            "description": "A stateless, read-only HTTP/JSON API over katalogr's core queries.",
        },
        "paths": paths,
        "components": {"schemas": components},
    })
}

/// The document as the file holds it: sorted keys, two-space indent, a closing newline.
pub fn pretty(ops: &[Op]) -> String {
    let mut s = serde_json::to_string_pretty(&document(ops)).expect("serialise openapi");
    s.push('\n');
    s
}
