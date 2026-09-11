//! Optional user extensions: macros, expression functions, and external (Rust)
//! functions plugged into the parser and runtime.
//!
//! - A **macro** ([`Options::macro_def`]) is expanded at parse time into
//!   `["let", ...]` binding its parameters to the call arguments — zero runtime
//!   cost, but no recursion (a depth limit guards against cycles).
//! - An **expression function** ([`Options::expr_fn`]) is left as a call in
//!   the tree and invoked at evaluation time, so it may recurse (bounded by a
//!   call-depth limit).
//! - An **external function** ([`Options::external`]) is a Rust closure invoked with
//!   the evaluated argument values (and the context), returning a value
//!   dynamically.
//!
//! All are provided via [`Options`], passed to [`parse_with`](crate::parse_with)
//! and [`evaluate_with`](crate::evaluate_with). [`Options`] is `Send + Sync`
//! (external closures must be too), so a registry can be shared across threads.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, OnceLock};

use crate::ast::Expr;
use crate::context::EvaluationContext;
use crate::error::{EvalError, ParseError};
use crate::value::Value;

/// Maximum macro-expansion depth before assuming a recursive macro.
pub(crate) const MAX_MACRO_DEPTH: usize = 64;
/// Maximum expression-function call depth before erroring. Kept conservative so deep
/// recursion errors cleanly rather than overflowing the native stack.
pub(crate) const MAX_CALL_DEPTH: usize = 64;

/// An external function: a Rust closure called with the evaluated arguments
/// and the context.
pub type ExternalFn =
    Arc<dyn Fn(&[Value], &EvaluationContext) -> Result<Value, EvalError> + Send + Sync>;

/// A parse-time macro: `body` is expanded with `params` bound to the call
/// arguments (as a `let`). `body` is raw JSON in the expression grammar.
#[derive(Debug, Clone)]
pub struct Macro {
    pub params: Vec<String>,
    pub body: serde_json::Value,
}

/// An expression function: `body` (raw JSON) is evaluated at call time with
/// `params` bound to the argument values. May reference itself or other
/// expression functions (recursion is bounded at runtime).
///
/// Bodies are compiled lazily, once per [`Options`], on the first
/// [`evaluate_with`](crate::evaluate_with); registering anything else on the
/// `Options` afterwards recompiles them on the next evaluation.
#[derive(Debug, Clone)]
pub struct ExprFn {
    pub params: Vec<String>,
    pub body: serde_json::Value,
}

/// An [`ExprFn`] whose body has been parsed, ready for the evaluator.
#[derive(Debug, Clone)]
pub(crate) struct CompiledFn {
    pub(crate) params: Vec<String>,
    pub(crate) body: Expr,
}

/// Parser/runtime extension registry.
pub struct Options {
    pub(crate) macros: HashMap<String, Macro>,
    pub(crate) expr_fns: HashMap<String, ExprFn>,
    /// name -> (arity, closure)
    pub(crate) externals: HashMap<String, (usize, ExternalFn)>,
    /// Current macro-expansion depth (transient parse state).
    pub(crate) depth: AtomicUsize,
    /// Expression functions with their bodies parsed, built on first use and
    /// reset by every registration (bodies may refer to names registered
    /// later, including their own for recursion, so they can't be parsed
    /// eagerly at registration time).
    compiled: OnceLock<Result<HashMap<String, CompiledFn>, ParseError>>,
    /// Whether the parser transparently converts legacy function objects
    /// (`{type, property, stops, ...}`) to modern expressions before parsing.
    /// On by default; see [`crate::convert`].
    pub(crate) convert_legacy: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            macros: HashMap::new(),
            expr_fns: HashMap::new(),
            externals: HashMap::new(),
            depth: AtomicUsize::new(0),
            compiled: OnceLock::new(),
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
        self.compiled = OnceLock::new();
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
        self.compiled = OnceLock::new();
        self
    }

    /// Register an expression function: a body written in the expression
    /// language, invoked at evaluation time (may recurse).
    pub fn expr_fn(
        &mut self,
        name: impl Into<String>,
        params: Vec<String>,
        body: serde_json::Value,
    ) -> &mut Options {
        self.expr_fns.insert(name.into(), ExprFn { params, body });
        self.compiled = OnceLock::new();
        self
    }

    /// Register an external Rust function of the given arity. The closure
    /// receives the evaluated argument values and the evaluation context.
    pub fn external<F>(&mut self, name: impl Into<String>, arity: usize, f: F) -> &mut Options
    where
        F: Fn(&[Value], &EvaluationContext) -> Result<Value, EvalError> + Send + Sync + 'static,
    {
        self.externals.insert(name.into(), (arity, Arc::new(f)));
        self.compiled = OnceLock::new();
        self
    }

    /// The registered expression functions with their bodies parsed against
    /// this `Options` (so they may call macros, other expression functions,
    /// externals, and themselves). Parsed once and cached until the next
    /// registration; a body that fails to parse fails every evaluation.
    pub(crate) fn compiled_fns(&self) -> Result<&HashMap<String, CompiledFn>, &ParseError> {
        self.compiled
            .get_or_init(|| {
                self.expr_fns
                    .iter()
                    .map(|(name, f)| {
                        let body = crate::parse::parse(&f.body, self)?;
                        Ok((
                            name.clone(),
                            CompiledFn {
                                params: f.params.clone(),
                                body,
                            },
                        ))
                    })
                    .collect()
            })
            .as_ref()
    }
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field("macros", &self.macros)
            .field("expr_fns", &self.expr_fns)
            .field("externals", &self.externals.keys().collect::<Vec<_>>())
            .finish()
    }
}
