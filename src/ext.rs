//! Optional user extensions: macros and functions plugged into the parser and
//! runtime.
//!
//! - A **macro** is expanded at parse time into `["let", ...]` binding its
//!   parameters to the call arguments — zero runtime cost, but no recursion
//!   (macro expansion must terminate; a recursion limit guards against cycles).
//! - A **function** is left as a call in the tree and invoked at evaluation
//!   time, so it may recurse (guarded by a call-depth limit).
//!
//! Both are provided via [`Options`], passed to [`parse_with`](crate::parse_with)
//! and [`evaluate_with`](crate::evaluate_with).

use std::cell::Cell;
use std::collections::HashMap;

/// Maximum macro-expansion depth before assuming a recursive macro.
pub(crate) const MAX_MACRO_DEPTH: usize = 256;
/// Maximum user-function call depth before erroring. Kept conservative so deep
/// recursion errors cleanly rather than overflowing the native stack.
pub(crate) const MAX_CALL_DEPTH: usize = 64;

/// A parse-time macro: `body` is expanded with `params` bound to the call
/// arguments (as a `let`). `body` is raw JSON in the expression grammar.
#[derive(Debug, Clone)]
pub struct Macro {
    pub params: Vec<String>,
    pub body: serde_json::Value,
}

/// An eval-time function: `body` (raw JSON) is evaluated with `params` bound to
/// the argument values. May reference itself or other functions (recursion is
/// bounded at runtime).
#[derive(Debug, Clone)]
pub struct Function {
    pub params: Vec<String>,
    pub body: serde_json::Value,
}

/// Parser/runtime extension registry.
#[derive(Debug, Default)]
pub struct Options {
    pub(crate) macros: HashMap<String, Macro>,
    pub(crate) functions: HashMap<String, Function>,
    /// Current macro-expansion depth (interior mutability during parsing).
    pub(crate) depth: Cell<usize>,
}

impl Options {
    pub fn new() -> Options {
        Options::default()
    }

    /// Register a macro expanded at parse time.
    pub fn macro_def(
        &mut self,
        name: impl Into<String>,
        params: Vec<String>,
        body: serde_json::Value,
    ) -> &mut Options {
        self.macros.insert(name.into(), Macro { params, body });
        self
    }

    /// Register a function invoked at evaluation time (may recurse).
    pub fn function(
        &mut self,
        name: impl Into<String>,
        params: Vec<String>,
        body: serde_json::Value,
    ) -> &mut Options {
        self.functions
            .insert(name.into(), Function { params, body });
        self
    }
}
