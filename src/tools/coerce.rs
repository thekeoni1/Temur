//! T33 tolerant scalar coercion at the argument boundary.
//!
//! Evidence (T32 matrix pass, 2026-08-15, archived): Llama-3.2-3B emitted
//! sixteen structurally perfect tool calls whose scalar arguments arrived as
//! JSON *strings* — `"false"` where the schema says boolean, and `"600000"`,
//! `"120000"`, `"1200000"`, `"0"`, `"null"` where it says number. Every one
//! was rejected at [`super::parse_input`], and the model, having no other
//! idea, resent the identical call until the repeat guard stopped it. No
//! other model in the ten-model matrix produced one.
//!
//! The fix is deliberately NOT a central value-walk over the whole argument
//! object: a blind `"false"` → `false` rewrite would corrupt legitimate
//! string fields — an `oldString` of `"false"` must stay the four-character
//! string. So these helpers are applied field by field, to exactly the four
//! non-string scalar arg fields in the tree (edit `replaceAll`, read
//! `offset`/`limit`, bash `timeout`), which is the entire surface.
//!
//! What is accepted, and nothing else:
//!   * bool — a JSON string that is exactly `"true"` or `"false"`.
//!   * u64  — a JSON string of ASCII digits only: no sign, no whitespace,
//!     no trimming, no separators.
//!   * `Option` fields only — a JSON string that is exactly `"null"`, read
//!     as absent (one of the archived sixteen shapes).
//!
//! Real booleans, real numbers, real JSON `null`, and absent fields all take
//! the pre-T33 path byte-for-byte. Anything else — `"maybe"`, `""`,
//! `"12.5"`, `"-3"`, floats, negatives — still fails LOUDLY as
//! `InvalidInput`, with a message that names the accepted forms so the
//! agent loop stays self-healing. `input_schema()` is untouched for every
//! tool: the schema remains the contract, and this is parse-time tolerance,
//! not a relaxation of what temur asks for.

use serde::de::{Deserializer, Error};
use serde::Deserialize;
use serde_json::Value;

/// `deserialize_with` for a plain `bool` arg (edit `replaceAll`).
pub fn lenient_bool<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(d)? {
        Value::String(s) => match s.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(D::Error::custom(format!(
                "expected a boolean, or the string \"true\" or \"false\"; got the string {}",
                quoted(other)
            ))),
        },
        // Every non-string value takes the ordinary path, so a real
        // boolean is unchanged and a number still fails with serde's own
        // wording.
        v => serde_json::from_value(v).map_err(D::Error::custom),
    }
}

/// `deserialize_with` for an `Option<u64>` arg (read `offset`/`limit`, bash
/// `timeout`). The field must also carry `#[serde(default)]`: naming a
/// `deserialize_with` turns off serde's implicit "missing `Option` is
/// `None`", and an absent field must keep meaning absent.
pub fn lenient_opt_u64<'de, D>(d: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(d)? {
        Value::String(s) => {
            if s == "null" {
                return Ok(None);
            }
            // Digits only. `str::parse` would also take a leading `+`/`-`
            // and we want negatives to stay rejected, so the shape is
            // checked before the parse.
            if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
                return match s.parse::<u64>() {
                    Ok(n) => Ok(Some(n)),
                    // Digits, but too many of them: say so rather than
                    // claim digit strings are unacceptable.
                    Err(_) => Err(D::Error::custom(format!(
                        "number out of range for u64: the string {}",
                        quoted(&s)
                    ))),
                };
            }
            Err(D::Error::custom(format!(
                "expected a number, or a string of digits like \"600000\", or the string \
                 \"null\"; got the string {}",
                quoted(&s)
            )))
        }
        // Real `null` is absent, exactly as before.
        Value::Null => Ok(None),
        // Numbers (including floats and negatives) keep their pre-T33
        // outcome and their pre-T33 message.
        v => serde_json::from_value::<u64>(v).map(Some).map_err(D::Error::custom),
    }
}

/// The offending string as it appeared, quoted, so an empty string is
/// visible in the error rather than vanishing.
fn quoted(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}
