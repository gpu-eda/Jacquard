// SPDX-License-Identifier: Apache-2.0

//! `.lib.json` reader — JSON-serialized Liberty into the same
//! [`LibertyGroup`] tree the text parser ([`crate::parse`]) yields.
//!
//! SKY130 (`vendor/sky130_fd_sc_hd/`) ships its Liberty exclusively as
//! `.lib.json` (6848 per-cell-per-corner cell files + a small per-corner
//! library header), never as `.lib` text. To give SKY130 a generated
//! cell-model-IR descriptor (Decision 0019), the converter needs a `LibertyGroup`
//! tree — this module decodes the JSON encoding into exactly that tree so the
//! `liberty-to-cellir` converter consumes it unchanged.
//!
//! ## The flattened-group key encoding
//!
//! Liberty's concrete syntax has two statement forms — `name : value;`
//! (attribute) and `type (args) { body }` (group). The `.lib.json` encoding
//! flattens both into a single JSON object whose keys carry the distinction:
//!
//! - A key **containing a comma** is a *named group*: the substring before
//!   the first comma is the group type, the comma-separated remainder is the
//!   header argument list. The value is the group body (an object), e.g.
//!   `"pin,A": { "direction": "input" }` ⇒ `pin (A) { direction : input; }`
//!   and `"ff,IQ,IQ_N": { ... }` ⇒ `ff (IQ, IQ_N) { ... }`,
//!   `"cell_rise,del_1_7_7": { ... }` ⇒ `cell_rise (del_1_7_7) { ... }`.
//! - A **bare key** (no comma) whose value is an **object** is an *unnamed
//!   group*: `"internal_power": { ... }` ⇒ `internal_power () { ... }`.
//! - A **bare key** whose value is an **array of objects** is a *repeated*
//!   unnamed group — one group per element: `"timing": [ {..}, {..} ]` ⇒ two
//!   `timing () { .. }` groups in source order.
//! - Any other bare key (scalar, or array of scalars / array of scalar rows)
//!   is an **attribute**.
//!
//! ## Lookup tables and complex attributes
//!
//! Liberty's list-valued attributes (`index_1`, `index_2`, `values`) are
//! quoted-string-packed in text form — `index_1 ("0.01, 0.02, 0.03")`, and a
//! 2-D `values ("r0c0, r0c1", "r1c0, r1c1")` with one quoted string per row.
//! The JSON encoding uses native arrays instead: a 1-D `[0.01, 0.02, 0.03]`
//! and a 2-D `[[..], [..]]`. To produce the *same* [`Value`] shape the text
//! parser yields, this reader re-packs them:
//!
//! - a 1-D scalar array ⇒ a single [`Value::String`] of the elements joined
//!   with `", "` (matching `index_1 ("0.01, 0.02")`),
//! - a 2-D array (array of arrays) ⇒ one [`Value::String`] *per row*, each row
//!   the inner elements joined with `", "` (matching the multi-string
//!   `values (...)` form).
//!
//! Downstream's `first_table_value` reads `values.first_string()` and splits
//! on commas/spaces, so a 2-D table's first row string yields the first
//! scalar exactly as it does for text-parsed Liberty.

use serde_json::{Map, Value as JsonValue};

use crate::{Attribute, LibertyGroup, Value};

/// Parse a `.lib.json` object string as the **body** of one Liberty group of
/// the given `group_type` and header `names`.
///
/// The JSON top-level value must be an object — it is the flattened group
/// body (the SKY130 per-cell file's top-level object *is* the body of one
/// `cell (...)` group; the per-corner header file's top-level object *is* the
/// body of the `library (...)` group). The returned [`LibertyGroup`] is
/// structurally identical to what [`crate::parse`] produces for the
/// equivalent `.lib` text.
pub fn parse_group(
    content: &str,
    group_type: &str,
    names: Vec<String>,
) -> Result<LibertyGroup, String> {
    let root: JsonValue =
        serde_json::from_str(content).map_err(|e| format!("invalid .lib.json: {e}"))?;
    let JsonValue::Object(obj) = root else {
        return Err("top-level .lib.json value is not an object".to_string());
    };
    Ok(group_from_object(group_type.to_string(), names, &obj))
}

/// Build a [`LibertyGroup`] of `group_type`/`names` from a decoded JSON object.
fn group_from_object(
    group_type: String,
    names: Vec<String>,
    obj: &Map<String, JsonValue>,
) -> LibertyGroup {
    let mut group = LibertyGroup {
        group_type,
        names,
        attributes: Vec::new(),
        groups: Vec::new(),
    };
    for (key, value) in obj {
        decode_entry(key, value, &mut group);
    }
    group
}

/// Decode one `(key, value)` pair of a flattened group body, appending either
/// an attribute or one-or-more subgroups to `group`.
fn decode_entry(key: &str, value: &JsonValue, group: &mut LibertyGroup) {
    if let Some((gtype, rest)) = key.split_once(',') {
        // Named group: `"type,name[,arg...]"`.
        let names: Vec<String> = rest.split(',').map(|s| s.trim().to_string()).collect();
        match value {
            JsonValue::Object(o) => {
                group
                    .groups
                    .push(group_from_object(gtype.to_string(), names, o));
            }
            JsonValue::Array(a) if a.iter().all(|v| v.is_object()) && !a.is_empty() => {
                // A named group repeated under one key (rare). Each element is
                // its own group body sharing the type/name.
                for v in a {
                    if let JsonValue::Object(o) = v {
                        group
                            .groups
                            .push(group_from_object(gtype.to_string(), names.clone(), o));
                    }
                }
            }
            // A comma key with a scalar value is not valid Liberty; keep the
            // datum as an attribute under the raw key rather than dropping it.
            other => group.attributes.push(Attribute {
                name: key.to_string(),
                values: encode_value(other),
            }),
        }
        return;
    }

    match value {
        JsonValue::Object(o) => {
            // Bare key, object value ⇒ a single unnamed subgroup.
            group
                .groups
                .push(group_from_object(key.to_string(), Vec::new(), o));
        }
        JsonValue::Array(a) if !a.is_empty() && a.iter().all(|v| v.is_object()) => {
            // Bare key, array-of-objects ⇒ repeated unnamed subgroups.
            for v in a {
                if let JsonValue::Object(o) = v {
                    group
                        .groups
                        .push(group_from_object(key.to_string(), Vec::new(), o));
                }
            }
        }
        other => {
            // Scalar, or array of scalars / scalar rows ⇒ an attribute.
            group.attributes.push(Attribute {
                name: key.to_string(),
                values: encode_value(other),
            });
        }
    }
}

/// Render a JSON value as the attribute value list a text parse would yield.
fn encode_value(value: &JsonValue) -> Vec<Value> {
    match value {
        JsonValue::Array(a) => encode_list(a),
        scalar => vec![encode_scalar(scalar)],
    }
}

/// Encode a JSON array (already known not to be an array-of-objects) as the
/// `Vec<Value>` a text parse yields: a 2-D table becomes one packed string per
/// row; a 1-D list becomes a single packed string.
fn encode_list(a: &[JsonValue]) -> Vec<Value> {
    if !a.is_empty() && a.iter().all(|v| v.is_array()) {
        // 2-D: one `Value::String` per row, mirroring the multi-string
        // `values ("r0...", "r1...")` text form.
        a.iter()
            .map(|row| {
                let JsonValue::Array(cells) = row else {
                    unreachable!("all elements are arrays")
                };
                Value::String(join_scalars(cells))
            })
            .collect()
    } else {
        // 1-D: a single packed `Value::String`, mirroring `index_1 ("a, b")`.
        vec![Value::String(join_scalars(a))]
    }
}

/// Join scalar JSON values with `", "` into the packed-string form Liberty
/// text uses for list attributes.
fn join_scalars(a: &[JsonValue]) -> String {
    a.iter()
        .map(scalar_to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render a single scalar JSON value to its source-text spelling.
fn scalar_to_string(value: &JsonValue) -> String {
    match value {
        JsonValue::String(s) => s.clone(),
        JsonValue::Number(n) => n.to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Null => String::new(),
        // Nested arrays/objects inside a "scalar" position are not expected;
        // fall back to compact JSON so nothing is silently lost.
        other => other.to_string(),
    }
}

/// Encode a scalar JSON value as a single [`Value`].
fn encode_scalar(value: &JsonValue) -> Value {
    match value {
        JsonValue::String(s) => Value::String(s.clone()),
        JsonValue::Number(n) => {
            let f = n.as_f64().unwrap_or(f64::NAN);
            Value::number(n.to_string(), f)
        }
        JsonValue::Bool(b) => Value::Ident(b.to_string()),
        JsonValue::Null => Value::String(String::new()),
        other => Value::String(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_named_pin_group_and_scalar_attrs() {
        let g = parse_group(
            r#"{ "direction": "output", "function": "(!A) | (!B) | (!C)" }"#,
            "pin",
            vec!["Y".to_string()],
        )
        .unwrap();
        assert_eq!(g.group_type, "pin");
        assert_eq!(g.first_name(), Some("Y"));
        assert_eq!(g.attr("direction").unwrap().first_string(), Some("output"));
        assert_eq!(
            g.attr("function").unwrap().first_string(),
            Some("(!A) | (!B) | (!C)")
        );
    }

    #[test]
    fn decodes_flattened_subgroup_keys() {
        // A cell body with `pin,A` / `pin,Y` flattened-group keys.
        let cell = parse_group(
            r#"{
                "area": 5.0,
                "pin,A": { "direction": "input" },
                "pin,Y": { "direction": "output", "function": "!A" }
            }"#,
            "cell",
            vec!["sky130_fd_sc_hd__inv_1".to_string()],
        )
        .unwrap();
        assert_eq!(cell.first_name(), Some("sky130_fd_sc_hd__inv_1"));
        assert_eq!(cell.attr("area").unwrap().first_number(), Some(5.0));
        let pins: Vec<_> = cell.groups_of_type("pin").collect();
        assert_eq!(pins.len(), 2);
        assert_eq!(pins[0].first_name(), Some("A"));
        assert_eq!(pins[1].first_name(), Some("Y"));
        assert_eq!(pins[1].attr("function").unwrap().first_string(), Some("!A"));
    }

    #[test]
    fn decodes_ff_multi_arg_group() {
        let cell = parse_group(
            r#"{ "ff,IQ,IQ_N": { "clocked_on": "CLK", "next_state": "D" } }"#,
            "cell",
            vec!["dff".to_string()],
        )
        .unwrap();
        let ff = cell.group_of_type("ff").unwrap();
        assert_eq!(ff.names, vec!["IQ".to_string(), "IQ_N".to_string()]);
        assert_eq!(ff.attr("clocked_on").unwrap().first_string(), Some("CLK"));
        assert_eq!(ff.attr("next_state").unwrap().first_string(), Some("D"));
    }

    #[test]
    fn decodes_repeated_bare_group_array() {
        // `timing` as an array of objects ⇒ two `timing` subgroups.
        let pin = parse_group(
            r#"{
                "direction": "output",
                "timing": [
                    { "related_pin": "A", "timing_sense": "negative_unate" },
                    { "related_pin": "B", "timing_sense": "negative_unate" }
                ]
            }"#,
            "pin",
            vec!["Y".to_string()],
        )
        .unwrap();
        let timings: Vec<_> = pin.groups_of_type("timing").collect();
        assert_eq!(timings.len(), 2);
        assert_eq!(
            timings[0].attr("related_pin").unwrap().first_string(),
            Some("A")
        );
        assert_eq!(
            timings[1].attr("related_pin").unwrap().first_string(),
            Some("B")
        );
    }

    #[test]
    fn decodes_lookup_table_1d_and_2d() {
        let table = parse_group(
            r#"{
                "index_1": [0.01, 0.0230506, 0.0531329],
                "index_2": [0.0005, 0.0025, 0.005],
                "values": [
                    [0.0319453, 0.0377438, 0.0522698],
                    [0.0316862, 0.0372976, 0.0513602]
                ]
            }"#,
            "cell_rise",
            vec!["del_1_7_7".to_string()],
        )
        .unwrap();
        // 1-D index packs into one string.
        assert_eq!(
            table.attr("index_1").unwrap().first_string(),
            Some("0.01, 0.0230506, 0.0531329")
        );
        // 2-D values: one Value per row, first row first.
        let values = table.attr("values").unwrap();
        assert_eq!(values.values.len(), 2);
        assert_eq!(
            values.first_string(),
            Some("0.0319453, 0.0377438, 0.0522698")
        );
        // The first scalar (as `first_table_value` would extract it).
        let first = values
            .first_string()
            .unwrap()
            .split([',', ' '])
            .find(|s| !s.is_empty())
            .unwrap();
        assert_eq!(first.parse::<f64>().unwrap(), 0.0319453);
    }

    #[test]
    fn decodes_operating_conditions_header() {
        let lib = parse_group(
            r#"{
                "default_operating_conditions": "tt_025C_1v80",
                "nom_voltage": 1.8,
                "nom_temperature": 25.0,
                "operating_conditions,tt_025C_1v80": {
                    "process": 1.0, "temperature": 25.0, "voltage": 1.8
                }
            }"#,
            "library",
            vec!["sky130_fd_sc_hd__tt_025C_1v80".to_string()],
        )
        .unwrap();
        assert_eq!(
            lib.attr("default_operating_conditions")
                .unwrap()
                .first_string(),
            Some("tt_025C_1v80")
        );
        let oc = lib.group_of_type("operating_conditions").unwrap();
        assert_eq!(oc.first_name(), Some("tt_025C_1v80"));
        assert_eq!(oc.attr("voltage").unwrap().first_number(), Some(1.8));
        assert_eq!(oc.attr("temperature").unwrap().first_number(), Some(25.0));
    }

    #[test]
    fn rejects_non_object_root() {
        assert!(parse_group("[1, 2, 3]", "cell", vec![]).is_err());
        assert!(parse_group("not json", "cell", vec![]).is_err());
    }
}
