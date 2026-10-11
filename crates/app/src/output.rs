//! The only place the viewer writes to stdout or stderr.
//!
//! `print!`-style macros are denied by lint, and `std::io::stdout`/`stderr` are denied by
//! `disallowed-methods` in clippy.toml, so all output goes through here (docs/plan.md §7).
#![expect(
    clippy::disallowed_methods,
    reason = "this module is the one sanctioned writer to stdout and stderr"
)]

use std::fmt::Display;
use std::io::Write as _;

/// Writes an error for the person running the viewer.
pub(crate) fn report_error(message: impl Display) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "if stderr cannot be written there is nowhere left to report that"
    )]
    let _ = writeln!(std::io::stderr().lock(), "gbk-markdown: {message}");
}

/// Writes one line of machine-read output (the probe report). Returns whether it was written.
#[must_use]
pub(crate) fn print_line(line: impl Display) -> bool {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}").is_ok() && stdout.flush().is_ok()
}
