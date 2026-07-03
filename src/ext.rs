//! Optional user extensions: macros, expression functions, and native (Rust)
//! functions plugged into the parser and runtime.
//!
//! - A **macro** ([`Options::macro_def`]) is expanded at parse time into
//!   `["let", ...]` binding its parameters to the call arguments — zero runtime
//!   cost, but no recursion (a depth limit guards against cycles).
//! - A **function** ([`Options::function`]) is left as a call in the tree and
//!   invoked at evaluation time, so it may recurse (bounded by a call-depth
//!   limit).
//! - A **native function** ([`Options::native`]) is a Rust closure invoked with
//!   the evaluated argument values (and the context), returning a value
//!   dynamically.
//!
//! All are provided via [`Options`], passed to [`parse_with`](crate::parse_with)
//! and [`evaluate_with`](crate::evaluate_with). [`Options`] is `Send + Sync`
//! (native closures must be too), so a registry can be shared across threads.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use crate::context::EvaluationContext;
use crate::error::EvalError;
use crate::value::Value;

/// Maximum macro-expansion depth before assuming a recursive macro.
pub(crate) const MAX_MACRO_DEPTH: usize = 64;
/// Maximum user-function call depth before erroring. Kept conservative so deep
/// recursion errors cleanly rather than overflowing the native stack.
pub(crate) const MAX_CALL_DEPTH: usize = 64;

/// A native function: called with the evaluated arguments and the context.
pub type NativeFn =
    Arc<dyn Fn(&[Value], &EvaluationContext) -> Result<Value, EvalError> + Send + Sync>;

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
pub struct Options {
    pub(crate) macros: HashMap<String, Macro>,
    pub(crate) functions: HashMap<String, Function>,
    /// name -> (arity, closure)
    pub(crate) natives: HashMap<String, (usize, NativeFn)>,
    /// Current macro-expansion depth (transient parse state).
    pub(crate) depth: AtomicUsize,
    /// Whether the parser transparently converts legacy function objects
    /// (`{type, property, stops, ...}`) to modern expressions before parsing.
    /// On by default; see [`crate::convert`].
    pub(crate) convert_legacy: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            macros: HashMap::new(),
            functions: HashMap::new(),
            natives: HashMap::new(),
            depth: AtomicUsize::new(0),
            convert_legacy: true,
        }
    }
}

impl Options {
    pub fn new() -> Options {
        Options::default()
    }

    /// Enable or disable transparent conversion of legacy function objects
    /// (on by default). When disabled, a bare JSON object is rejected as a
    /// parse error rather than being treated as a legacy function.
    pub fn convert_legacy(&mut self, enabled: bool) -> &mut Options {
        self.convert_legacy = enabled;
        self
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

    /// Register a native Rust function of the given arity. The closure receives
    /// the evaluated argument values and the evaluation context.
    pub fn native<F>(&mut self, name: impl Into<String>, arity: usize, f: F) -> &mut Options
    where
        F: Fn(&[Value], &EvaluationContext) -> Result<Value, EvalError> + Send + Sync + 'static,
    {
        self.natives.insert(name.into(), (arity, Arc::new(f)));
        self
    }
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field("macros", &self.macros)
            .field("functions", &self.functions)
            .field("natives", &self.natives.keys().collect::<Vec<_>>())
            .finish()
    }
}
