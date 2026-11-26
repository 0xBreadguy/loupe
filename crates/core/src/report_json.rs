//! Print-only `"schema"` key for the Recap, Tools and Trace JSON objects.
//!
//! Recap and Tools are the structs the Insights screen holds. Trace is the
//! struct its own screen holds. The opt-in cache stores run facts for Recap
//! and Tools, not these objects, and keys them with its own entry version.
//! Trace's step cache stores steps, not this object. The printed version stays
//! here so none of those see it.
//!
//! A wrapper with `#[serde(flatten)]` would not keep the rest of the text: this
//! crate's `serde_json` has no `preserve_order`, so a flattened value is a
//! `BTreeMap` and every key would be rewritten in alphabetical order. Prefixing
//! `to_string_pretty`'s own text leaves every later byte as the serializer wrote it.
use serde::Serialize;

/// Pretty-print `value` and put `"schema": N` on the first line of the object.
/// Every later byte is `serde_json::to_string_pretty`'s own text.
pub(crate) fn with_schema<T: Serialize>(
    value: &T,
    schema: u32,
) -> Result<String, serde_json::Error> {
    let body = serde_json::to_string_pretty(value)?;
    let Some(rest) = body.strip_prefix('{') else {
        return Ok(body);
    };
    Ok(format!("{{\n  \"schema\": {schema},{rest}"))
}

#[cfg(test)]
mod tests {
    use super::with_schema;
    use crate::{platform, recap, tools, trace};

    /// The stamped text is the serializer's text plus one line, and that line is first.
    fn schema_is_first_and_the_only_addition(stamped: &str, bare: &str) {
        let mut lines: Vec<&str> = stamped.lines().collect();
        assert!(lines.len() >= 2, "{stamped}");
        assert_eq!(lines[0], "{");
        assert_eq!(
            lines[1], "  \"schema\": 1,",
            "schema is not the first key:\n{stamped}"
        );
        lines.remove(1);
        assert_eq!(
            lines.join("\n"),
            bare,
            "stamping changed more than the schema line"
        );
        let stamped_value: serde_json::Value = serde_json::from_str(stamped).unwrap();
        let bare_value: serde_json::Value = serde_json::from_str(bare).unwrap();
        assert_eq!(stamped_value["schema"], 1);
        assert!(
            bare_value.get("schema").is_none(),
            "the report struct carries schema"
        );
    }

    fn week_at(now: f64) -> (recap::Recap, tools::Tools) {
        let local: recap::Local = &platform::local_time;
        let period = recap::Period::new(now, recap::Which::LastWeek, local);
        (
            recap::demo(&period, local).unwrap(),
            tools::demo(&period, local).unwrap(),
        )
    }

    #[test]
    fn recap_json_schema_is_1_and_first() {
        let (report, _) = week_at(1_795_069_560_000.0);
        let bare = serde_json::to_string_pretty(&report).unwrap();
        let stamped = recap::printed_json(&report).unwrap();
        assert_eq!(recap::JSON_SCHEMA, 1);
        assert_eq!(stamped, with_schema(&report, recap::JSON_SCHEMA).unwrap());
        schema_is_first_and_the_only_addition(&stamped, &bare);
    }

    #[test]
    fn tools_json_schema_is_1_and_first() {
        let (_, report) = week_at(1_795_069_560_000.0);
        let bare = serde_json::to_string_pretty(&report).unwrap();
        let stamped = tools::printed_json(&report).unwrap();
        assert_eq!(tools::JSON_SCHEMA, 1);
        assert_eq!(stamped, with_schema(&report, tools::JSON_SCHEMA).unwrap());
        schema_is_first_and_the_only_addition(&stamped, &bare);
    }

    #[test]
    fn trace_json_schema_is_1_and_first() {
        let report = trace::demo(1_795_069_560_000.0).unwrap();
        let bare = serde_json::to_string_pretty(&report).unwrap();
        let stamped = trace::printed_json(&report).unwrap();
        assert_eq!(trace::JSON_SCHEMA, 1);
        assert_eq!(stamped, with_schema(&report, trace::JSON_SCHEMA).unwrap());
        schema_is_first_and_the_only_addition(&stamped, &bare);
    }
}
