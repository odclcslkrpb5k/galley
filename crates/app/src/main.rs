//! gbk-markdown: a lightweight Markdown viewer.

mod args;
mod document_view;
mod images;
mod links;
mod output;
mod probe;
mod render;
mod renderer;
mod units;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use gtk::prelude::*;
use gtk::{gio, glib};

use args::{Args, Mode, Startup};
use document_view::DocumentView;
use gbk_markdown_core::units::LeafId;
use output::report_error;
use probe::Probe;
use renderer::EnvChange;

const APP_ID: &str = "io.github.odclcslkrpb5k.GbkMarkdown";
/// The window's default size, in logical pixels.
const DEFAULT_WIDTH: i32 = 900;
const DEFAULT_HEIGHT: i32 = 1000;

/// Why the file could not be shown.
#[derive(Debug, thiserror::Error)]
enum LoadError {
    #[error("cannot read the file ({0})")]
    Read(std::io::ErrorKind),
    #[error("the file loader stopped unexpectedly")]
    LoaderPanicked,
}

/// What the loader thread makes of the file.
struct Loaded {
    paints: Vec<render::paint::LeafPaint>,
    /// Anchor names and the leaves they name.
    anchors: HashMap<String, LeafId>,
    /// Whether each `<details>` starts open.
    details: Vec<bool>,
    images: Vec<render::image::ImageInfo>,
}

type Loader = JoinHandle<Result<Loaded, LoadError>>;

/// What `show` needs of the arguments.
#[derive(Debug, Clone)]
struct Showing {
    file: Option<PathBuf>,
    anchor: Option<String>,
    offline: bool,
}

impl From<&Args> for Showing {
    fn from(args: &Args) -> Self {
        Self {
            file: args.file.clone(),
            anchor: args.anchor.clone(),
            offline: args.offline,
        }
    }
}

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
    // Before any other thread exists: see `set_environment`.
    set_environment(renderer::env_changes(
        args.renderer,
        std::env::var("GSK_RENDERER").ok().as_deref(),
        std::env::var("GDK_DISABLE").ok().as_deref(),
    ));
    // Read and parse the file on a worker while GTK starts and the window is mapped
    // (docs/plan.md §4.4).
    let loader: Option<Loader> = match (args.mode, args.file.clone()) {
        (Mode::Document, Some(path)) => Some(thread::spawn(move || load(&path))),
        (Mode::Document | Mode::EmptyWindow, _) => None,
    };
    let probe = Probe::new(args.probe.clone(), main_started, loader.is_some());
    match args.startup {
        Startup::Application => run_application(&args, loader, probe),
        Startup::PlainInit => run_plain(&args, loader, &probe),
    }
}

/// Sets GTK's renderer through the environment, which GTK reads when it starts.
///
/// The program's one `unsafe` (a gate check keeps it the only one, docs/plan.md §4.4): changing
/// the environment is unsound while another thread may read it. Called first in `main`, before
/// the loader thread is spawned and before GTK (which starts threads of its own) is initialised,
/// so only this thread exists.
#[expect(
    unsafe_code,
    reason = "set_var is unsafe in edition 2024; GTK reads its renderer only from the environment"
)]
fn set_environment(changes: Vec<EnvChange>) {
    for change in changes {
        match change {
            // SAFETY: only this thread exists (see above), so nothing reads the environment
            // concurrently.
            EnvChange::Set(name, value) => unsafe { std::env::set_var(name, value) },
            // SAFETY: as above.
            EnvChange::Remove(name) => unsafe { std::env::remove_var(name) },
        }
    }
}

/// Reads, parses and paints the document: everything up to Pango, off the main thread.
fn load(path: &Path) -> Result<Loaded, LoadError> {
    let bytes = std::fs::read(path).map_err(|error| LoadError::Read(error.kind()))?;
    let doc = gbk_markdown_core::load(&bytes);
    let paints = render::paint::paint_document(&doc);
    let anchors = render::paint::anchor_leaves(&doc);
    let details = render::paint::details_open(&doc);
    let images = render::paint::image_infos(&doc);
    Ok(Loaded {
        paints,
        anchors,
        details,
        images,
    })
}

fn run_application(args: &Args, loader: Option<Loader>, probe: Rc<Probe>) -> ExitCode {
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let loader = RefCell::new(loader);
    let failed = Rc::new(Cell::new(false));
    let title = window_title(args);
    let showing = Showing::from(args);
    app.connect_activate(glib::clone!(
        #[strong]
        failed,
        move |app| {
            probe.gtk_ready();
            let window = gtk::ApplicationWindow::new(app);
            let shown = show(window.upcast_ref(), &title, loader.take(), &showing, &probe);
            if let Err(error) = shown {
                report_error(error);
                failed.set(true);
                app.quit();
            }
        }
    ));
    // Our own arguments are already parsed; GApplication gets none.
    let code = app.run_with_args::<&str>(&[]);
    if failed.get() || code != glib::ExitCode::SUCCESS {
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
    if let Err(error) = show(
        &window,
        &window_title(args),
        loader,
        &Showing::from(args),
        probe,
    ) {
        report_error(error);
        return ExitCode::FAILURE;
    }
    main_loop.run();
    ExitCode::SUCCESS
}

/// Presents the window, then fills it.
///
/// Presenting first sends the map request, so the window manager frames and maps the window
/// while the loader finishes. Nothing paints before control returns to the main loop, so the
/// document is still in place for the first frame.
fn show(
    window: &gtk::Window,
    title: &str,
    loader: Option<Loader>,
    showing: &Showing,
    probe: &Rc<Probe>,
) -> Result<(), LoadError> {
    let Showing {
        file,
        anchor,
        offline,
    } = showing;
    window.set_title(Some(title));
    window.set_default_size(DEFAULT_WIDTH, DEFAULT_HEIGHT);
    probe.attach(window);
    window.present();
    probe.presented();
    if let Some(loader) = loader {
        // Fonts load on first use, 3-15 ms each; load them while the loader finishes.
        render::text::warm_up(window, &|| loader.is_finished(), &mut |bytes| {
            probe.layout_created(bytes);
        });
        let waited = Instant::now();
        let document = loader.join().unwrap_or(Err(LoadError::LoaderPanicked))?;
        probe.loader_joined(waited.elapsed());
        let start = anchor.as_deref().and_then(|name| {
            let leaf = document_view::anchor_leaf(&document.anchors, name);
            if leaf.is_none() {
                report_error(format_args!(
                    "no heading or anchor named {name:?}; showing the top"
                ));
            }
            leaf
        });
        let view = DocumentView::new(
            document_view::Opened {
                paints: document.paints,
                anchors: document.anchors,
                details: document.details,
                images: document.images,
                // Absolute, so `/path` links find the repository above it whatever the working
                // directory was.
                path: file
                    .as_deref()
                    .map(|file| std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf())),
                start,
                offline: *offline,
            },
            Rc::clone(probe),
        );
        // The view draws its own scroll indicator (render/indicator.rs says why).
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .child(&view)
            .build();
        probe.set_scroll_target(scrolled.vadjustment());
        let (looking, clicking, stating, counting) =
            (view.clone(), view.clone(), view.clone(), view.clone());
        probe.set_view(probe::ViewProbe {
            link_at: Box::new(move |x, y| looking.link_label_at(x, y)),
            click_at: Box::new(move |x, y| clicking.probe_click(x, y)),
            state: Box::new(move || stating.probe_state()),
            images: Box::new(move || counting.probe_images()),
        });
        window.set_child(Some(&scrolled));
        if probe.prepare_enabled() {
            view.prepare(f64::from(DEFAULT_WIDTH), f64::from(DEFAULT_HEIGHT));
        }
    }
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
