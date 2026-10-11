//! gbk-markdown: a lightweight Markdown viewer.

mod args;
mod document_view;
mod extent;
mod probe;
mod spike;
mod units;

use std::cell::RefCell;
use std::fmt::Display;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use gtk::prelude::*;
use gtk::{gio, glib};

use args::{Args, Spike, Startup};
use document_view::DocumentView;
use probe::Probe;

const APP_ID: &str = "io.github.odclcslkrpb5k.GbkMarkdown";

/// Why the file could not be shown.
#[derive(Debug, thiserror::Error)]
enum LoadError {
    #[error("cannot read the file ({0})")]
    Read(std::io::ErrorKind),
    #[error("the file loader stopped unexpectedly")]
    LoaderPanicked,
}

type Loader = JoinHandle<Result<Vec<String>, LoadError>>;

fn main() -> ExitCode {
    let main_started = Instant::now();
    install_panic_hook();
    let args = match args::parse(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            report_error(format_args!("{error}\n\n{}", args::USAGE));
            return ExitCode::from(2);
        }
    };
    // Read and parse the file on a worker while GTK starts on the main thread (docs/plan.md §4.4).
    let loader: Option<Loader> = match (args.spike, args.file.clone()) {
        (Spike::Paragraphs, Some(path)) => Some(thread::spawn(move || load(&path))),
        (Spike::Paragraphs | Spike::Empty, _) => None,
    };
    let probe = Probe::new(args.probe.clone(), main_started, loader.is_some());
    match args.startup {
        Startup::Application => run_application(&args, loader, probe),
        Startup::PlainInit => run_plain(&args, loader, &probe),
    }
}

fn load(path: &Path) -> Result<Vec<String>, LoadError> {
    let bytes = std::fs::read(path).map_err(|error| LoadError::Read(error.kind()))?;
    let decoded = gbk_markdown_core::decode(&bytes);
    Ok(spike::paragraphs(&decoded.text))
}

fn run_application(args: &Args, loader: Option<Loader>, probe: Rc<Probe>) -> ExitCode {
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let loader = RefCell::new(loader);
    let failed = Rc::new(RefCell::new(false));
    let title = window_title(args);
    app.connect_activate(glib::clone!(
        #[strong]
        failed,
        move |app| {
            probe.gtk_ready();
            let window = gtk::ApplicationWindow::new(app);
            if show(
                window.upcast_ref(),
                &title,
                loader.borrow_mut().take(),
                &probe,
            )
            .is_err()
            {
                failed.replace(true);
                app.quit();
            }
        }
    ));
    // Our own arguments are already parsed; GApplication gets none.
    let code = app.run_with_args::<&str>(&[]);
    if *failed.borrow() || code != glib::ExitCode::SUCCESS {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn run_plain(args: &Args, loader: Option<Loader>, probe: &Rc<Probe>) -> ExitCode {
    if gtk::init().is_err() {
        report_error("cannot start GTK (is there a display?)");
        return ExitCode::FAILURE;
    }
    probe.gtk_ready();
    let window = gtk::Window::new();
    let main_loop = glib::MainLoop::new(None, false);
    window.connect_close_request(glib::clone!(
        #[strong]
        main_loop,
        move |_window| {
            main_loop.quit();
            glib::Propagation::Proceed
        }
    ));
    if show(&window, &window_title(args), loader, probe).is_err() {
        return ExitCode::FAILURE;
    }
    main_loop.run();
    ExitCode::SUCCESS
}

/// Fills and presents the window. On failure the error has been reported.
fn show(
    window: &gtk::Window,
    title: &str,
    loader: Option<Loader>,
    probe: &Rc<Probe>,
) -> Result<(), ()> {
    window.set_title(Some(title));
    window.set_default_size(900, 1000);
    if let Some(loader) = loader {
        let paragraphs = loader
            .join()
            .unwrap_or(Err(LoadError::LoaderPanicked))
            .map_err(report_error)?;
        let view = DocumentView::new(paragraphs, Rc::clone(probe));
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&view)
            .build();
        probe.set_scroll_target(scrolled.vadjustment());
        window.set_child(Some(&scrolled));
    }
    probe.attach(window);
    window.present();
    probe.presented();
    Ok(())
}

fn window_title(args: &Args) -> String {
    args.file.as_deref().and_then(Path::file_name).map_or_else(
        || "gbk-markdown".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        report_error(format_args!("internal error: {info}"));
    }));
}

/// Writes an error for the person running the viewer.
pub(crate) fn report_error(message: impl Display) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "if stderr cannot be written there is nowhere left to report that"
    )]
    let _ = writeln!(std::io::stderr().lock(), "gbk-markdown: {message}");
}
