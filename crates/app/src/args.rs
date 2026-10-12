//! Command-line arguments, parsed once at the boundary.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

pub(crate) const USAGE: &str = "\
usage: gbk-markdown [OPTIONS] FILE

options:
  --exit-after-first-frame  print the probe report after the first frame and exit
  --bench-scroll            after the first frame, scroll to the end, report frame times, exit
  --t0=NS                   spawn time in CLOCK_REALTIME nanoseconds, for the startup report
  --screenshot=PATH         save the first frame as PNG
  --spike=empty|paragraphs  an empty window (the startup floor), or the document (default)
  --check-heights           when the height worker finishes, compare its heights with layouts
                            made here, report mismatches and exit
  --no-prepare              do not lay out the first screen before allocation (measurement)
  --application             start with gtk::Application instead of gtk::init (measurement)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Args {
    pub(crate) file: Option<PathBuf>,
    pub(crate) spike: Spike,
    pub(crate) startup: Startup,
    pub(crate) probe: ProbeArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spike {
    Empty,
    Paragraphs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Startup {
    Application,
    PlainInit,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent measurement switches, each its own option"
)]
pub(crate) struct ProbeArgs {
    pub(crate) exit_after_first_frame: bool,
    pub(crate) bench_scroll: bool,
    pub(crate) check_heights: bool,
    pub(crate) no_prepare: bool,
    /// Spawn time since the Unix epoch, as passed by scripts/bench-startup.
    pub(crate) t0: Option<Duration>,
    pub(crate) screenshot: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UsageError {
    #[error("unknown option {0}")]
    UnknownOption(String),
    #[error("an option is not valid UTF-8")]
    OptionNotUtf8,
    #[error("--t0 needs a whole number of nanoseconds")]
    BadT0,
    #[error("--spike must be empty or paragraphs")]
    BadSpike,
    #[error("no FILE given")]
    MissingFile,
    #[error("more than one FILE given")]
    ExtraFile,
}

/// Parses the arguments after the program name.
pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Args, UsageError> {
    let mut file = None;
    let mut spike = Spike::Paragraphs;
    // Plain gtk::init is the default: it skips GApplication's session-bus connection, measured
    // 20-45 ms faster to first frame in M0 (docs/perf-baseline.md).
    let mut startup = Startup::PlainInit;
    let mut probe = ProbeArgs::default();

    for arg in args {
        if !arg.as_encoded_bytes().starts_with(b"--") {
            if file.replace(PathBuf::from(arg)).is_some() {
                return Err(UsageError::ExtraFile);
            }
            continue;
        }
        let arg = arg
            .into_string()
            .map_err(|_not_utf8| UsageError::OptionNotUtf8)?;
        let (name, value) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (arg.as_str(), None),
        };
        match (name, value) {
            ("--exit-after-first-frame", None) => probe.exit_after_first_frame = true,
            ("--bench-scroll", None) => probe.bench_scroll = true,
            ("--check-heights", None) => probe.check_heights = true,
            ("--no-prepare", None) => probe.no_prepare = true,
            ("--application", None) => startup = Startup::Application,
            ("--t0", Some(value)) => {
                let nanos = value
                    .parse::<u64>()
                    .map_err(|_not_a_number| UsageError::BadT0)?;
                probe.t0 = Some(Duration::from_nanos(nanos));
            }
            ("--screenshot", Some(path)) => probe.screenshot = Some(PathBuf::from(path)),
            ("--spike", Some("empty")) => spike = Spike::Empty,
            ("--spike", Some("paragraphs")) => spike = Spike::Paragraphs,
            ("--spike", Some(_)) => return Err(UsageError::BadSpike),
            _ => return Err(UsageError::UnknownOption(name.to_owned())),
        }
    }

    if file.is_none() && spike == Spike::Paragraphs {
        return Err(UsageError::MissingFile);
    }
    Ok(Args {
        file,
        spike,
        startup,
        probe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Args, UsageError> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn a_file_alone_uses_the_defaults() {
        let args = parse_strs(&["README.md"]).expect("valid");
        assert_eq!(args.file, Some(PathBuf::from("README.md")));
        assert_eq!(args.spike, Spike::Paragraphs);
        assert_eq!(args.startup, Startup::PlainInit);
        assert_eq!(args.probe, ProbeArgs::default());
    }

    #[test]
    fn probe_options_are_read() {
        let args = parse_strs(&[
            "--t0=1500",
            "--exit-after-first-frame",
            "--screenshot=/tmp/a.png",
            "--application",
            "--bench-scroll",
            "--no-prepare",
            "--check-heights",
            "x.md",
        ])
        .expect("valid");
        assert!(args.probe.bench_scroll);
        assert!(args.probe.no_prepare);
        assert!(args.probe.check_heights);
        assert_eq!(args.probe.t0, Some(Duration::from_nanos(1500)));
        assert!(args.probe.exit_after_first_frame);
        assert_eq!(args.probe.screenshot, Some(PathBuf::from("/tmp/a.png")));
        assert_eq!(args.startup, Startup::Application);
    }

    #[test]
    fn the_paragraph_spike_can_be_named() {
        let args = parse_strs(&["--spike=empty", "--spike=paragraphs", "a.md"]).expect("valid");
        assert_eq!(args.spike, Spike::Paragraphs);
    }

    #[test]
    fn the_empty_spike_needs_no_file() {
        let args = parse_strs(&["--spike=empty"]).expect("valid");
        assert_eq!(args.spike, Spike::Empty);
        assert_eq!(args.file, None);
    }

    #[test]
    fn refusals() {
        assert_eq!(parse_strs(&[]), Err(UsageError::MissingFile));
        assert_eq!(parse_strs(&["a.md", "b.md"]), Err(UsageError::ExtraFile));
        assert_eq!(parse_strs(&["--t0=soon", "a.md"]), Err(UsageError::BadT0));
        assert_eq!(
            parse_strs(&["--t0", "a.md"]),
            Err(UsageError::UnknownOption("--t0".to_owned()))
        );
        assert_eq!(
            parse_strs(&["--spike=fast", "a.md"]),
            Err(UsageError::BadSpike)
        );
        assert_eq!(
            parse_strs(&["--verbose", "a.md"]),
            Err(UsageError::UnknownOption("--verbose".to_owned()))
        );
    }
}
