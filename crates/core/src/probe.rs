//! Headless engine-semantics probes over synthetic inputs.
//!
//! `--probe-hidden` and `--probe-width` are the original display-width probes.
//! `--probe-helper NAME` reads one JSON value per stdin line and prints one JSON result per
//! line, so `checks/scripts/text-parity.ts` can compare any small helper against the
//! TypeScript oracle. Non-finite numbers travel as the strings "NaN", "Infinity" and
//! "-Infinity" because JSON has no such literals.
use serde_json::{Value, json};
use std::io::{self, BufRead};

/// A number argument. Strings carry the values JSON cannot spell (NaN and the infinities) and
/// every non-safe-integer double, parsed by Rust's correctly rounded f64 parser rather than
/// serde_json's, which loses a unit in the last place on some long decimals.
fn num(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => match s.as_str() {
            "NaN" => Some(f64::NAN),
            "Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            _ => s.parse().ok(),
        },
        _ => None,
    }
}
fn text(value: &Value) -> Option<&str> {
    value.as_str()
}
/// A string argument that the original accepts as `string | null | undefined`; `null` is `None`.
fn text_opt(value: &Value) -> Option<Option<&str>> {
    match value {
        Value::Null => Some(None),
        Value::String(s) => Some(Some(s)),
        _ => None,
    }
}
fn u32_of(value: &Value) -> Option<u32> {
    u32::try_from(value.as_u64()?).ok()
}
fn usize_of(value: &Value) -> Option<usize> {
    usize::try_from(value.as_u64()?).ok()
}

/// A float result, with the three JSON cannot spell sent as their JavaScript names.
fn json_num(x: f64) -> Value {
    if x.is_nan() {
        json!("NaN")
    } else if x == f64::INFINITY {
        json!("Infinity")
    } else if x == f64::NEG_INFINITY {
        json!("-Infinity")
    } else {
        json!(x)
    }
}

fn helper(name: &str, input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let args = input.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let missing = || format!("{name}: expected a JSON array of arguments");
    let at = |i: usize| args.get(i).ok_or_else(missing);
    Ok(match name {
        // 1. fitting and wrapping
        "fit" => json!(crate::terminal::fit(
            text(at(0)?).ok_or("fit: string")?,
            usize_of(at(1)?).ok_or("fit: max")?
        )),
        "wrap" => json!(crate::terminal::wrap(
            text(at(0)?).ok_or("wrap: string")?,
            usize_of(at(1)?).ok_or("wrap: max")?
        )),
        // 2. compact numbers and durations
        "tokens" => json!(crate::terminal::tokens(match at(0)? {
            Value::Null => f64::NAN,
            v => num(v).ok_or("tokens: number")?,
        })),
        "duration" => json!(crate::terminal::duration(match at(0)? {
            Value::Null => f64::NAN,
            v => num(v).ok_or("duration: number")?,
        })),
        "span" => {
            let sec = match at(0)? {
                Value::Null => None,
                v => Some(num(v).ok_or("span: number")?),
            };
            json!(crate::terminal::span(sec))
        }
        "clock" => json!(crate::terminal::clock(
            num(at(0)?).ok_or("clock: t")?,
            num(at(1)?).ok_or("clock: day")?
        )),
        "when" => {
            let end = match at(1)? {
                Value::Null => None,
                v => Some(num(v).ok_or("when: end")?),
            };
            json!(crate::terminal::when(
                num(at(0)?).ok_or("when: start")?,
                end,
                num(at(2)?).ok_or("when: now")?
            ))
        }
        // 3. model names and families
        "short_model" => json!(crate::terminal::short_model(
            text(at(0)?).ok_or("short_model: string")?
        )),
        "family" => json!(family_name(crate::terminal::family(
            text_opt(at(0)?).ok_or("family: model")?.unwrap_or(""),
            text_opt(at(1)?).ok_or("family: hint")?,
        ))),
        "category" => json!(crate::model::category(
            text(at(0)?).ok_or("category: type")?,
            text_opt(at(1)?).ok_or("category: tool")?
        )),
        // 4. identity and links
        "thread_ref" => json!(crate::integration::thread_ref(at(0)?).map(|(e, i)| json!([e, i]))),
        "thread_key" => json!(crate::integration::thread_key(
            text(at(0)?).ok_or("thread_key: env")?,
            text(at(1)?).ok_or("thread_key: id")?
        )),
        "environment_id_valid" => json!(crate::integration::environment_id_valid(
            text(at(0)?).ok_or("environment_id_valid: string")?
        )),
        "encode_uri_component" => json!(crate::integration::encode_segment(
            text(at(0)?).ok_or("encode_uri_component: string")?
        )),
        "thread_link" => json!(crate::integration::thread_link(
            text(at(0)?).ok_or("thread_link: env")?,
            text(at(1)?).ok_or("thread_link: id")?
        )),
        "agent_thread_link" => {
            let agent: crate::model::Agent = serde_json::from_value(at(0)?.clone())?;
            let local = text_opt(at(1)?).ok_or("agent_thread_link: local")?;
            json!(crate::integration::agent_thread_link(&agent, local))
        }
        "thread_for_title" => {
            let agents: Vec<crate::model::Agent> = serde_json::from_value(at(1)?.clone())?;
            json!(crate::integration::thread_for_title(
                text(at(0)?).ok_or("thread_for_title: title")?,
                &agents
            ))
        }
        // 5. delegation ordering
        "locale_compare" => json!(match crate::terminal::locale_compare(
            text(at(0)?).ok_or("locale_compare: a")?,
            text(at(1)?).ok_or("locale_compare: b")?
        ) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }),
        // 6. remaining string and number helpers
        "window_length" => match crate::overview::window_length(
            text(at(0)?).ok_or("window_length: label")?,
        ) {
            Some(v) => json_num(v),
            None => Value::Null,
        },
        "hex" => json!(crate::integration::hex(
            text_opt(at(0)?).ok_or("hex: value")?
        )),
        "mix" => json!(crate::terminal::mix(
            u32_of(at(0)?).ok_or("mix: a")?,
            u32_of(at(1)?).ok_or("mix: b")?,
            num(at(2)?).ok_or("mix: t")?
        )),
        "luminance" => json_num(crate::integration::luminance(
            u32_of(at(0)?).ok_or("luminance: color")?
        )),
        "contrast" => json_num(crate::integration::contrast(
            u32_of(at(0)?).ok_or("contrast: a")?,
            u32_of(at(1)?).ok_or("contrast: b")?
        )),
        "chroma" => json!(crate::integration::chroma(
            u32_of(at(0)?).ok_or("chroma: color")?
        )),
        "ramp" => json!(crate::terminal::ramp(
            &crate::terminal::Palette::tokyo(),
            num(at(0)?).ok_or("ramp: t")?
        )),
        "reach" => {
            let surfaces: Vec<u32> = at(3)?
                .as_array()
                .ok_or("reach: surfaces")?
                .iter()
                .map(u32_of)
                .collect::<Option<_>>()
                .ok_or("reach: surface")?;
            json!(crate::integration::reach(
                u32_of(at(0)?).ok_or("reach: from")?,
                u32_of(at(1)?).ok_or("reach: to")?,
                num(at(2)?).ok_or("reach: target")?,
                &surfaces
            ))
        }
        "level" => json!(crate::terminal::level(
            num(at(0)?).ok_or("level: v")?,
            num(at(1)?).ok_or("level: max")?
        )
        .to_string()),
        "spark" => {
            let values: Vec<f64> = at(0)?
                .as_array()
                .ok_or("spark: values")?
                .iter()
                .map(|v| num(v).ok_or("spark: value"))
                .collect::<Result<_, _>>()?;
            let max = match at(1)? {
                Value::Null => None,
                v => Some(num(v).ok_or("spark: max")?),
            };
            json!(crate::terminal::spark(&values, max))
        }
        "big_width" => json!(crate::terminal::big_width(
            text(at(0)?).ok_or("big_width: string")?,
            at(1)?.as_bool().ok_or("big_width: large")?
        )),
        // Diagnostic: ECMAScript Number::toString, whose formatting the duration helpers use.
        "number_string" => json!(crate::terminal::js_number(
            num(at(0)?).ok_or("number_string: number")?
        )),
        // 7. formatting helpers the details pane used to keep privately
        "js_round" => json_num(crate::terminal::js_round(
            num(at(0)?).ok_or("js_round: number")?,
        )),
        "to_fixed" => json!(crate::terminal::to_fixed(
            num(at(0)?).ok_or("to_fixed: number")?,
            usize_of(at(1)?).ok_or("to_fixed: digits")?,
        )),
        "fixed_number" => json!(crate::terminal::fixed_number(
            num(at(0)?).ok_or("fixed_number: number")?,
            usize_of(at(1)?).ok_or("fixed_number: digits")?,
        )),
        "short_duration" => json!(crate::terminal::short_duration(
            num(at(0)?).ok_or("short_duration: seconds")?,
        )),
        "coarse" => json!(crate::terminal::coarse(
            num(at(0)?).ok_or("coarse: seconds")?,
        )),
        "spinner" => json!(crate::terminal::spinner(
            num(at(0)?).ok_or("spinner: now")?,
        )
        .to_string()),
        "whole" => json!(crate::terminal::whole(
            text(at(0)?).ok_or("whole: string")?,
        )),
        "limit_label" => json!(crate::terminal::limit_label(
            text(at(0)?).ok_or("limit_label: string")?,
        )),
        "glyph" => json!(crate::model::glyph(
            text(at(0)?).ok_or("glyph: category")?,
        )
        .to_string()),
        "ratio" => {
            let value = |v: &Value| -> Option<Option<f64>> {
                match v {
                    Value::Null => Some(None),
                    v => num(v).map(Some),
                }
            };
            match crate::model::ratio(
                value(at(0)?).ok_or("ratio: used")?,
                value(at(1)?).ok_or("ratio: max")?,
            ) {
                Some(v) => json_num(v),
                None => Value::Null,
            }
        }
        "tidy" => json!(crate::model::tidy(
            text(at(0)?).ok_or("tidy: string")?,
            text(at(1)?).ok_or("tidy: home")?,
        )),
        "item_label" => {
            let item: crate::model::Item = serde_json::from_value(at(0)?.clone())?;
            json!(item.display(text(at(1)?).ok_or("item_label: home")?))
        }
        _ => return Err(format!("unknown helper {name:?}").into()),
    })
}

fn family_name(f: crate::terminal::ProviderFamily) -> &'static str {
    match f {
        crate::terminal::ProviderFamily::Claude => "claude",
        crate::terminal::ProviderFamily::OpenAi => "openai",
        crate::terminal::ProviderFamily::Grok => "grok",
        crate::terminal::ProviderFamily::Gemini => "gemini",
        crate::terminal::ProviderFamily::Other => "other",
    }
}

fn run_helper(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        let input: Value = serde_json::from_str(&line?)?;
        println!("{}", serde_json::to_string(&helper(name, &input)?)?);
    }
    Ok(())
}

pub fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|arg| arg == "--probe-helper") {
        let name = args.get(i + 1).ok_or("--probe-helper needs a name")?;
        run_helper(name)?;
        return Ok(true);
    }
    if args.iter().any(|arg| arg == "--probe-width") {
        for line in io::stdin().lock().lines() {
            let value: serde_json::Value = serde_json::from_str(&line?)?;
            let result = if let Some(cp) = value.as_u64() {
                char::from_u32(cp as u32).map(crate::terminal::cell_width).unwrap_or(1)
            } else {
                crate::terminal::width(value.as_str().ok_or("expected string")?)
            };
            println!("{}", result);
        }
        return Ok(true);
    }
    if !args.iter().any(|arg| arg == "--probe-hidden") { return Ok(false); }
    for line in io::stdin().lock().lines() {
        let cp: u32 = line?.parse()?;
        // Rust strings cannot contain lone surrogates; the table represents them directly.
        let hidden = char::from_u32(cp).map(crate::terminal::is_hidden).unwrap_or_else(|| crate::terminal::HIDDEN_RANGES.iter().any(|&(a, b)| (a..=b).contains(&cp)));
        println!("{}", u8::from(hidden));
    }
    Ok(true)
}
