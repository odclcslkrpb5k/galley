//! Lint canary (docs/plan.md §7): one deliberate violation per lint in `[workspace.lints]`.
//!
//! Each violation carries a marker `canary: <table>:<configured name> -> <diagnostic code>`.
//! `scripts/lint-canary` fails unless the markers name exactly the configured lints and every
//! diagnostic fires. This catches a misspelled, renamed or mis-prioritised lint, and a lint the
//! pinned toolchain no longer knows. The normal gate steps exclude this crate.

use std::cmp::Ordering;
use std::fmt;

// canary: rust:unknown_lints -> unknown_lints
#[expect(clippy::this_lint_does_not_exist, reason = "canary")]
pub fn unknown_lint() {}

// canary: rust:rust_2018_idioms -> elided_lifetimes_in_paths
pub fn elided_lifetime(_formatter: &mut fmt::Formatter) {}

// canary: rust:unsafe_code -> unsafe_code
pub fn unsafe_block(value: &u8) -> u8 {
    unsafe { std::ptr::read(value) }
}

// canary: rust:unused_must_use -> unused_must_use
pub fn unused_result(text: &str) {
    text.parse::<u8>();
}

// canary: rust:missing_debug_implementations -> missing_debug_implementations
pub struct NoDebug;

mod private {
    // canary: rust:unreachable_pub -> unreachable_pub
    pub fn reachable_only_in_crate() {}
}

pub fn use_private() {
    private::reachable_only_in_crate();
}

// canary: clippy:pedantic -> clippy::must_use_candidate
pub fn must_use_candidate() -> u8 {
    1
}

// canary: clippy:unwrap_used -> clippy::unwrap_used
pub fn unwrap(value: Option<u8>) -> u8 {
    value.unwrap()
}

// canary: clippy:expect_used -> clippy::expect_used
pub fn expect(value: Option<u8>) -> u8 {
    value.expect("canary")
}

// canary: clippy:panic -> clippy::panic
pub fn panics() {
    panic!("canary");
}

// canary: clippy:todo -> clippy::todo
pub fn not_done() {
    todo!();
}

// canary: clippy:unimplemented -> clippy::unimplemented
pub fn not_implemented() {
    unimplemented!();
}

// canary: clippy:unreachable -> clippy::unreachable
pub fn not_reachable() {
    unreachable!();
}

// canary: clippy:panic_in_result_fn -> clippy::panic_in_result_fn
pub fn panic_in_result(value: u8) -> Result<u8, fmt::Error> {
    assert!(value > 0);
    Ok(value)
}

// canary: clippy:unwrap_in_result -> clippy::unwrap_in_result
pub fn unwrap_in_result(text: &str) -> Result<u8, fmt::Error> {
    let value = text.parse::<u8>().unwrap();
    Ok(value)
}

// canary: clippy:indexing_slicing -> clippy::indexing_slicing
pub fn index(values: &[u8]) -> u8 {
    values[0]
}

// canary: clippy:string_slice -> clippy::string_slice
pub fn slice(text: &str) -> &str {
    &text[1..]
}

// canary: clippy:as_conversions -> clippy::as_conversions
pub fn widen(value: u32) -> u64 {
    value as u64
}

// canary: clippy:wildcard_enum_match_arm -> clippy::wildcard_enum_match_arm
pub fn wildcard(order: Ordering) -> u8 {
    match order {
        Ordering::Less => 0,
        _ => 1,
    }
}

// canary: clippy:let_underscore_must_use -> clippy::let_underscore_must_use
pub fn let_underscore(text: &str) {
    let _ = text.parse::<u8>();
}

// canary: clippy:map_err_ignore -> clippy::map_err_ignore
pub fn map_err_ignore(text: &str) -> Result<u8, fmt::Error> {
    text.parse::<u8>().map_err(|_| fmt::Error)
}

// canary: clippy:allow_attributes -> clippy::allow_attributes
#[allow(dead_code, reason = "canary")]
fn allowed() {}

// canary: clippy:allow_attributes_without_reason -> clippy::allow_attributes_without_reason
#[expect(dead_code)]
fn expected_without_reason() {}

// canary: clippy:dbg_macro -> clippy::dbg_macro
pub fn debug_print(value: u8) -> u8 {
    dbg!(value)
}

// canary: clippy:print_stdout -> clippy::print_stdout
pub fn stdout() {
    println!("canary");
}

// canary: clippy:print_stderr -> clippy::print_stderr
pub fn stderr() {
    eprintln!("canary");
}
