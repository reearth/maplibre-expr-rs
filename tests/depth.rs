//! Parse-time depth limits: macro expansion, JSON nesting, and the fact that
//! both are per-parse state rather than state on the shared `Options`.

use std::sync::Arc;
use std::thread;

use maplibre_expr::{parse, parse_with, Options};
use serde_json::json;

/// The macro-depth counter must be local to one parse: two threads sharing a
/// single `Arc<Options>` must not spend each other's budget. Thread A parses a
/// recursive (always failing) macro while thread B parses a one-level,
/// non-recursive macro that must always succeed.
#[test]
fn concurrent_parses_do_not_share_macro_depth() {
    let mut opts = Options::new();
    // deep() = deep()  — always exhausts the macro-depth budget.
    opts.macro_def("deep", vec![], json!(["deep"]));
    // shallow(x) = x + 1  — one level, must never hit any limit.
    opts.macro_def("shallow", vec!["x".into()], json!(["+", ["var", "x"], 1]));
    let opts = Arc::new(opts);

    let a = {
        let opts = Arc::clone(&opts);
        thread::spawn(move || {
            for _ in 0..5000 {
                assert!(parse_with(&json!(["deep"]), &opts).is_err());
            }
        })
    };
    let b = {
        let opts = Arc::clone(&opts);
        thread::spawn(move || {
            let mut failures = 0usize;
            for _ in 0..5000 {
                if parse_with(&json!(["shallow", 1]), &opts).is_err() {
                    failures += 1;
                }
            }
            failures
        })
    };
    a.join().unwrap();
    assert_eq!(
        b.join().unwrap(),
        0,
        "non-recursive macro failed spuriously"
    );
}

/// Two nested macro calls in sequence must not accumulate depth across parses.
#[test]
fn macro_depth_resets_between_parses() {
    let mut opts = Options::new();
    opts.macro_def("inc", vec!["x".into()], json!(["+", ["var", "x"], 1]));
    // 60 nested `inc` calls: under the 64-level macro limit, and each parse
    // must start from zero.
    let mut expr = json!(1);
    for _ in 0..60 {
        expr = json!(["inc", expr]);
    }
    for _ in 0..10 {
        assert!(parse_with(&expr, &opts).is_ok());
    }
}

/// The macro-depth message must describe what is actually measured (nesting
/// depth), not assert recursion as the cause.
#[test]
fn macro_depth_message_describes_nesting() {
    let mut opts = Options::new();
    opts.macro_def("deep", vec![], json!(["deep"]));
    let msg = parse_with(&json!(["deep"]), &opts).unwrap_err().to_string();
    assert!(
        msg.contains("Macro expansion nested more than 64 levels deep while expanding 'deep'"),
        "unexpected message: {msg}"
    );
    assert!(
        msg.contains("a macro that expands to itself"),
        "recursion should be named as one possible cause: {msg}"
    );

    // The same limit is reached by nesting that is not recursive at all, and
    // the message must fit that case too.
    let mut opts = Options::new();
    opts.macro_def("inc", vec!["x".into()], json!(["+", ["var", "x"], 1]));
    let mut expr = json!(1);
    for _ in 0..200 {
        expr = json!(["inc", expr]);
    }
    let msg = parse_with(&expr, &opts).unwrap_err().to_string();
    assert!(
        msg.contains("Macro expansion nested more than 64 levels deep while expanding 'inc'"),
        "unexpected message: {msg}"
    );
}

/// Deeply nested JSON must produce an `Err`, never a stack overflow.
#[test]
fn deeply_nested_json_errors_instead_of_overflowing() {
    // Deep enough to overflow the native stack if the parser recursed freely
    // (measured: ~10 KiB of stack per level in a debug build), but still cheap
    // for serde_json itself to build and drop.
    let mut expr = json!(1);
    for _ in 0..500 {
        expr = json!(["+", expr, 1]);
    }
    let msg = parse(&expr).unwrap_err().to_string();
    assert!(
        msg.contains("Expression nested more than 100 levels deep"),
        "unexpected message: {msg}"
    );
}

/// Nesting just under the limit still parses.
#[test]
fn nesting_below_the_limit_parses() {
    // The outermost array is level 1 and the innermost literal is not an array,
    // so 100 wrappers sit exactly at the limit; one more is over it.
    let mut expr = json!(1);
    for _ in 0..100 {
        expr = json!(["+", expr, 1]);
    }
    assert!(parse(&expr).is_ok());

    let deeper = json!(["+", expr, 1]);
    assert!(parse(&deeper).is_err());
}

/// The nesting limit must also be per-parse: repeated parses of a deep-but-ok
/// expression keep succeeding.
#[test]
fn nesting_depth_resets_between_parses() {
    let mut expr = json!(1);
    for _ in 0..80 {
        expr = json!(["+", expr, 1]);
    }
    for _ in 0..5 {
        assert!(parse(&expr).is_ok());
    }
}
