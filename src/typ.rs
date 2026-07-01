//! The MapLibre expression type system: [`Type`], subtyping, and formatting.
//!
//! Used by the [`typecheck`](crate::typecheck) pass to infer each node's type
//! and reject expressions the reference implementation rejects at compile time.

use std::fmt;

use crate::value::Value;

/// A type in the MapLibre expression type lattice.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Null,
    Number,
    String,
    Boolean,
    Color,
    Object,
    /// The top type — a supertype of every concrete type.
    Value,
    /// `array<itemType, N>`; `N` is `None` for unspecified length.
    Array(Box<Type>, Option<usize>),
    ProjectionDefinition,
    Collator,
    Formatted,
    Padding,
    NumberArray,
    ColorArray,
    ResolvedImage,
    VariableAnchorOffsetCollection,
}

impl Type {
    /// A fixed-length array type helper.
    pub fn array(item: Type, n: Option<usize>) -> Type {
        Type::Array(Box::new(item), n)
    }

    /// The bare kind name (`"number"`, `"array"`, ...).
    pub fn kind(&self) -> &'static str {
        match self {
            Type::Null => "null",
            Type::Number => "number",
            Type::String => "string",
            Type::Boolean => "boolean",
            Type::Color => "color",
            Type::Object => "object",
            Type::Value => "value",
            Type::Array(..) => "array",
            Type::ProjectionDefinition => "projectionDefinition",
            Type::Collator => "collator",
            Type::Formatted => "formatted",
            Type::Padding => "padding",
            Type::NumberArray => "numberArray",
            Type::ColorArray => "colorArray",
            Type::ResolvedImage => "resolvedImage",
            Type::VariableAnchorOffsetCollection => "variableAnchorOffsetCollection",
        }
    }

    /// The type of a concrete runtime [`Value`] (`typeOf` in the spec).
    pub fn of_value(v: &Value) -> Type {
        match v {
            Value::Null => Type::Null,
            Value::Bool(_) => Type::Boolean,
            Value::Number(_) => Type::Number,
            Value::String(_) => Type::String,
            Value::Color(_) => Type::Color,
            Value::Object(_) => Type::Object,
            Value::Image { .. } => Type::ResolvedImage,
            Value::Array(items) => {
                let mut item_type: Option<Type> = None;
                for it in items {
                    let t = Type::of_value(it);
                    match &item_type {
                        None => item_type = Some(t),
                        Some(existing) if *existing == t => {}
                        Some(_) => {
                            item_type = Some(Type::Value);
                            break;
                        }
                    }
                }
                Type::array(item_type.unwrap_or(Type::Value), Some(items.len()))
            }
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Array(item, Some(n)) => write!(f, "array<{item}, {n}>"),
            Type::Array(item, None) => {
                if matches!(**item, Type::Value) {
                    write!(f, "array")
                } else {
                    write!(f, "array<{item}>")
                }
            }
            other => write!(f, "{}", other.kind()),
        }
    }
}

/// Returns `true` if `t` is a subtype of `expected` (the spec's `checkSubtype`
/// returning no error). `Value` is the top type; array subtyping compares item
/// type and, when present, length.
pub fn is_subtype(expected: &Type, t: &Type) -> bool {
    if let Type::Array(exp_item, exp_n) = expected {
        if let Type::Array(t_item, t_n) = t {
            let item_ok = (*t_n == Some(0) && matches!(**t_item, Type::Value))
                || is_subtype(exp_item, t_item);
            let n_ok = exp_n.is_none() || exp_n == t_n;
            return item_ok && n_ok;
        }
        return false;
    }
    if expected.kind() == t.kind() {
        return true;
    }
    if matches!(expected, Type::Value) {
        return is_value_member(t);
    }
    false
}

/// Whether `t` is a member of the `value` union (everything concrete except a
/// bare collator).
fn is_value_member(t: &Type) -> bool {
    !matches!(t, Type::Collator)
}
