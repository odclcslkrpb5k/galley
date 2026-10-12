//! Command-line arguments, parsed once at the boundary.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::renderer::Renderer;

pub(crate) const USAGE: &str = "\
usage: gbk-markdown [OPTIONS] [--] FILE

options:
  --anchor=NAME             open at the heading or anchor NAME (as in FILE#NAME)
  --offline                 show remote images as placeholders: fetch nothing
  --renderer=cairo|gl       GTK's renderer (default cairo, faster to first frame); overrides
                            GSK_RENDERER
  --exit-after-first-frame  print the probe report after the first frame and exit
  --bench-scroll            after the first frame, scroll to the end, report frame times, exit
  --t0=NS                   spawn time in CLOCK_REALTIME nanoseconds, for the startup report
  --screenshot=PATH         save the first frame as PNG
  --link-at=X,Y             with --exit-after-first-frame, report the link at that point
  --wait-images             with --exit-after-first-frame, report and screenshot once no image
                            is loading
  --wait-highlights         highlight all code; with --exit-after-first-frame, report and
                            screenshot once it is, with --check-heights compare after it is
  --click-at=X,Y            with --exit-after-first-frame, click there after the first frame
                            (links outside the document are reported, not opened), then
                            report and screenshot the next frame
  --empty-window            show no document, for measuring GTK's startup floor (no FILE)
  --check-heights           when the height worker finishes, compare its heights with layouts
                            made here, report mismatches and exit
  --no-prepare              do not lay out the first screen before allocation (measurement)
  --application             start with gtk::Application instead of gtk::init (measurement)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Args {
    pub(crate) file: Option<PathBuf>,
    /// Where to open the file: a heading's or `<a id>`'s name.
    pub(crate) anchor: Option<String>,
    /// Fetch no remote images.
    pub(crate) offline: bool,
    pub(crate) mode: Mode,
    /// `None`: not given (see `renderer::env_changes`).
    pub(crate) renderer: Option<Renderer>,
    pub(crate) startup: Startup,
    pub(crate) probe: ProbeArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Document,
    /// No document: GTK's startup floor (docs/perf-baseline.md).
    EmptyWindow,
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
    /// A point of the window, in whole pixels, to report the link at after the first frame.
    pub(crate) link_at: Option<(i32, i32)>,
    /// A point to click after the first frame.
    pub(crate) click_at: Option<(i32, i32)>,
    /// Wait until no image is loading before reporting.
    pub(crate) wait_images: bool,
    /// Highlight all code, and wait until it is before reporting or checking heights.
    pub(crate) wait_highlights: bool,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UsageError {
    #[error("unknown option {0}")]
    UnknownOption(String),
    #[error("an option is not valid UTF-8")]
    OptionNotUtf8,
    #[error("--t0 needs a whole number of nanoseconds")]
    BadT0,
    #[error("--link-at and --click-at need X,Y in whole pixels")]
    BadPoint,
    #[error("--renderer must be cairo or gl")]
    BadRenderer,
    #[error("no FILE given")]
    MissingFile,
    #[error("more than one FILE given")]
    ExtraFile,
}

/// Parses the arguments after the program name.
pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Args, UsageError> {
    let mut file = None;
    let mut anchor = None;
    let mut offline = false;
    let mut options_ended = false;
    let mut mode = Mode::Document;
    let mut renderer = None;
    // Plain gtk::init is the default: it skips GApplication's session-bus connection, measured
    // 20-45 ms faster to first frame in M0 (docs/perf-baseline.md).
    let mut startup = Startup::PlainInit;
    let mut probe = ProbeArgs::default();

    for arg in args {
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if options_ended || !arg.as_encoded_bytes().starts_with(b"--") {
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
            ("--wait-images", None) => probe.wait_images = true,
            ("--wait-highlights", None) => probe.wait_highlights = true,
            ("--application", None) => startup = Startup::Application,
            ("--t0", Some(value)) => {
                let nanos = value
                    .parse::<u64>()
                    .map_err(|_not_a_number| UsageError::BadT0)?;
                probe.t0 = Some(Duration::from_nanos(nanos));
            }
            ("--screenshot", Some(path)) => probe.screenshot = Some(PathBuf::from(path)),
            ("--anchor", Some(name)) => anchor = Some(name.to_owned()),
            ("--offline", None) => offline = true,
            ("--link-at", Some(point)) => probe.link_at = Some(parse_point(point)?),
            ("--click-at", Some(point)) => probe.click_at = Some(parse_point(point)?),
            ("--empty-window", None) => mode = Mode::EmptyWindow,
            ("--renderer", Some("cairo")) => renderer = Some(Renderer::Cairo),
            ("--renderer", Some("gl")) => renderer = Some(Renderer::Gl),
            ("--renderer", Some(_)) => return Err(UsageError::BadRenderer),
            _ => return Err(UsageError::UnknownOption(name.to_owned())),
        }
    }

    if file.is_none() && mode == Mode::Document {
        return Err(UsageError::MissingFile);
    }
    Ok(Args {
        file,
        anchor,
        offline,
        mode,
        renderer,
        startup,
        probe,
    })
}

/// `X,Y` in whole pixels.
fn parse_point(point: &str) -> Result<(i32, i32), UsageError> {
    let (x, y) = point.split_once(',').ok_or(UsageError::BadPoint)?;
    let number = |text: &str| {
        text.trim()
            .parse::<i32>()
            .map_err(|_nan| UsageError::BadPoint)
    };
    Ok((number(x)?, number(y)?))
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
        assert_eq!(args.anchor, None);
        assert!(!args.offline);
        assert_eq!(args.mode, Mode::Document);
        assert_eq!(args.renderer, None);
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
            "--wait-images",
            "--wait-highlights",
            "x.md",
        ])
        .expect("valid");
        assert!(args.probe.wait_images);
        assert!(args.probe.wait_highlights);
        assert!(args.probe.bench_scroll);
        assert!(args.probe.no_prepare);
        assert!(args.probe.check_heights);
        assert_eq!(args.probe.t0, Some(Duration::from_nanos(1500)));
        assert!(args.probe.exit_after_first_frame);
        assert_eq!(args.probe.screenshot, Some(PathBuf::from("/tmp/a.png")));
        assert_eq!(args.startup, Startup::Application);
        let args = parse_strs(&["--link-at=10,20", "--click-at=3, 4", "x.md"]).expect("valid");
        assert_eq!(args.probe.link_at, Some((10, 20)));
        assert_eq!(args.probe.click_at, Some((3, 4)));
        for bad in [
            "--link-at=10",
            "--link-at=a,1",
            "--link-at=1,",
            "--link-at=1.5,2",
        ] {
            assert_eq!(parse_strs(&[bad, "x.md"]), Err(UsageError::BadPoint));
        }
    }

    #[test]
    fn the_renderer_can_be_chosen_and_the_last_choice_wins() {
        let args = parse_strs(&["--renderer=gl", "a.md"]).expect("valid");
        assert_eq!(args.renderer, Some(Renderer::Gl));
        let args = parse_strs(&["--renderer=gl", "--renderer=cairo", "a.md"]).expect("valid");
        assert_eq!(args.renderer, Some(Renderer::Cairo));
        assert_eq!(
            parse_strs(&["--renderer=vulkan", "a.md"]),
            Err(UsageError::BadRenderer)
        );
        assert_eq!(
            parse_strs(&["--renderer", "a.md"]),
            Err(UsageError::UnknownOption("--renderer".to_owned()))
        );
    }

    #[test]
    fn an_anchor_and_a_file_after_the_options_end() {
        assert!(parse_strs(&["--offline", "a.md"]).expect("valid").offline);
        let args = parse_strs(&["--anchor=install", "--", "--odd name.md"]).expect("valid");
        assert_eq!(args.anchor, Some("install".to_owned()));
        assert_eq!(args.file, Some(PathBuf::from("--odd name.md")));
        assert_eq!(
            parse_strs(&["--", "a.md", "--"]),
            Err(UsageError::ExtraFile)
        );
    }

    #[test]
    fn the_empty_window_needs_no_file() {
        let args = parse_strs(&["--empty-window"]).expect("valid");
        assert_eq!(args.mode, Mode::EmptyWindow);
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
            parse_strs(&["--empty-window=yes"]),
            Err(UsageError::UnknownOption("--empty-window".to_owned()))
        );
        assert_eq!(
            parse_strs(&["--verbose", "a.md"]),
            Err(UsageError::UnknownOption("--verbose".to_owned()))
        );
    }
}
