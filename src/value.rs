//! Runtime values produced by evaluating an expression.

use std::collections::BTreeMap;
use std::fmt;

use crate::color::Color;

/// A value in the MapLibre expression type system.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Color(Color),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
    /// A resolved image reference (the `image` operator).
    Image {
        name: String,
        available: bool,
    },
    /// Formatted text (the `format` operator): a list of styled sections.
    Formatted(Vec<FormatSection>),
    /// A `numberArray` value.
    NumberArray(Vec<f64>),
    /// A `colorArray` value.
    ColorArray(Vec<Color>),
    /// A `padding` value: `[top, right, bottom, left]`.
    Padding([f64; 4]),
    /// A `projectionDefinition`: a named projection or a transition between two.
    Projection(Projection),
    /// A locale-aware string collator (the `collator` operator).
    Collator {
        case_sensitive: bool,
        diacritic_sensitive: bool,
        locale: Option<String>,
    },
}

/// A projection definition value.
#[derive(Debug, Clone, PartialEq)]
pub enum Projection {
    Named(String),
    Transition {
        from: String,
        to: String,
        transition: f64,
    },
}

/// One styled section of a [`Value::Formatted`] value.
#[derive(Debug, Clone, PartialEq)]
pub struct FormatSection {
    pub text: String,
    /// `(name, available)` for an image section.
    pub image: Option<(String, bool)>,
    pub scale: Option<f64>,
    pub font_stack: Option<String>,
    pub text_color: Option<Color>,
    pub vertical_align: Option<String>,
}

impl Value {
    /// The MapLibre type name of this value (`"number"`, `"string"`, ...).
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Color(_) => "color",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
            Value::Image { .. } => "resolvedImage",
            Value::Formatted(_) => "formatted",
            Value::NumberArray(_) => "numberArray",
            Value::ColorArray(_) => "colorArray",
            Value::Padding(_) => "padding",
            Value::Projection(_) => "projectionDefinition",
            Value::Collator { .. } => "collator",
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Truthiness per the MapLibre `to-boolean` rules.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0 && !n.is_nan(),
            Value::String(s) => !s.is_empty(),
            _ => true,
        }
    }

    /// Build a literal [`Value`] from raw JSON (used by the `literal` operator
    /// and by bare literals in an expression).
    pub fn from_json(json: &serde_json::Value) -> Value {
        match json {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
            serde_json::Value::String(s) => Value::String(s.clone()),
            serde_json::Value::Array(a) => Value::Array(a.iter().map(Value::from_json).collect()),
            serde_json::Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, v)| (k.clone(), Value::from_json(v)))
                    .collect(),
            ),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, ""),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) => write!(f, "{}", format_number(*n)),
            Value::String(s) => write!(f, "{s}"),
            Value::Color(c) => write!(f, "{c}"),
            Value::Array(a) => {
                let parts: Vec<String> = a.iter().map(|v| v.to_string()).collect();
                write!(f, "{}", parts.join(","))
            }
            Value::Object(_) => write!(f, "{self:?}"),
            Value::Image { name, .. } => write!(f, "{name}"),
            Value::Formatted(sections) => {
                for s in sections {
                    write!(f, "{}", s.text)?;
                }
                Ok(())
            }
            Value::NumberArray(v) => {
                let parts: Vec<String> = v.iter().map(|n| format_number(*n)).collect();
                write!(f, "{}", parts.join(","))
            }
            Value::ColorArray(v) => {
                let parts: Vec<String> = v.iter().map(|c| c.to_string()).collect();
                write!(f, "{}", parts.join(","))
            }
            Value::Padding(v) => {
                let parts: Vec<String> = v.iter().map(|n| format_number(*n)).collect();
                write!(f, "{}", parts.join(","))
            }
            Value::Projection(Projection::Named(s)) => write!(f, "{s}"),
            Value::Projection(_) => write!(f, "{self:?}"),
            Value::Collator { .. } => write!(f, "collator"),
        }
    }
}

/// Format a number the way JavaScript's `String(n)` would: no trailing `.0`,
/// `"NaN"` / `"Infinity"` / `"-Infinity"` for the non-finite values, `"0"` for
/// negative zero, and exponential notation exactly when the decimal exponent is
/// `>= 21` or `<= -7` (`1e21` -> `"1e+21"`, `1e-7` -> `"1e-7"`).
///
/// This is a direct transcription of `Number::toString` (ECMA-262 §6.1.6.1.20).
/// That algorithm is stated in terms of the shortest decimal digit string `s`
/// that round-trips to the same double, together with the position `n` of the
/// decimal point; Rust's `{:e}` produces exactly that pair, so the digits are
/// taken from it rather than recomputed.
pub fn format_number(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v == 0.0 {
        // Covers -0.0: JavaScript's `String(-0)` is `"0"`.
        return "0".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if v < 0.0 {
        return format!("-{}", format_number(-v));
    }

    // `{:e}` renders the shortest round-tripping form as `<d>[.<ddd>]e<exp>`,
    // i.e. the value is `0.<digits> * 10^(exp + 1)`.
    let sci = format!("{v:e}");
    let (mantissa, exp) = match sci.split_once('e') {
        Some(parts) => parts,
        // `LowerExp` for `f64` always emits an exponent; fall back rather than
        // panic if that ever stops holding.
        None => return sci,
    };
    let Ok(exp) = exp.parse::<i32>() else {
        return sci;
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    // `k` is the digit count and `n` the decimal-point position, named as in
    // the spec's step 5.
    let k = digits.len() as i32;
    let n = exp + 1;

    if k <= n && n <= 21 {
        // Integer, padded with trailing zeros.
        digits + &"0".repeat((n - k) as usize)
    } else if 0 < n && n <= 21 {
        // Decimal point inside the digits.
        let split = n as usize;
        format!("{}.{}", &digits[..split], &digits[split..])
    } else if -6 < n && n <= 0 {
        // Leading `0.` plus `-n` zeros.
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else if k == 1 {
        format!(
            "{digits}e{}{}",
            if n >= 1 { "+" } else { "-" },
            (n - 1).abs()
        )
    } else {
        format!(
            "{}.{}e{}{}",
            &digits[..1],
            &digits[1..],
            if n >= 1 { "+" } else { "-" },
            (n - 1).abs()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::format_number;

    /// Every expectation here is the literal output of `String(n)` in a
    /// JavaScript engine, so the table doubles as the parity contract for
    /// `to-string` / `concat` / error-message interpolation.
    #[test]
    fn format_number_matches_javascript_string() {
        let cases: &[(f64, &str)] = &[
            // Plain integers and decimals.
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (2.5, "2.5"),
            (-2.5, "-2.5"),
            (123.456, "123.456"),
            (0.1, "0.1"),
            (1.0 / 3.0, "0.3333333333333333"),
            // The integer window that used to saturate at `i64::MAX`.
            (1e18, "1000000000000000000"),
            (9.223372036854776e18, "9223372036854776000"), // 2^63
            (1e19, "10000000000000000000"),
            (1e20, "100000000000000000000"),
            (-1e20, "-100000000000000000000"),
            // The positive exponent threshold: >= 21 goes exponential, with `+`.
            (1e21, "1e+21"),
            (-1e21, "-1e+21"),
            (1.5e21, "1.5e+21"),
            (1e22, "1e+22"),
            (f64::MAX, "1.7976931348623157e+308"),
            // The negative exponent threshold: <= -7 goes exponential, no `+`.
            (1e-6, "0.000001"),
            (1.5e-6, "0.0000015"),
            (1e-7, "1e-7"),
            (-1e-7, "-1e-7"),
            (1.5e-7, "1.5e-7"),
            (1e-10, "1e-10"),
            (5e-324, "5e-324"),
            // Non-finite values.
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for (n, expected) in cases {
            assert_eq!(format_number(*n), *expected, "String({n:?})");
        }
    }

    /// The shortest-round-trip digits must survive the reformatting: parsing the
    /// rendering back has to yield the very same double.
    #[test]
    fn format_number_round_trips() {
        let cases = [
            1e18,
            9.223372036854776e18,
            1e20,
            1e21,
            1e-7,
            5e-324,
            f64::MAX,
            123.456,
            1.0 / 3.0,
            -2.5,
        ];
        for n in cases {
            let s = format_number(n);
            assert_eq!(s.parse::<f64>().unwrap(), n, "round-trip of {s}");
        }
    }
}
