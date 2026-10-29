//! What the window holds, and what it takes: a document's view or, with no file, the welcome
//! (the logo, and how to open a file); a file dropped on either, or chosen with Ctrl+O, opens in
//! it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::document_view::{self, DocumentView};
use crate::output::report_error;
use crate::probe::{self, Probe};

/// The logo, for light and dark backgrounds (docs/logo; its text is outlines, so no font is
/// needed).
const LOGO_LIGHT: &[u8] = include_bytes!("../../../docs/logo/galley-light.svg");
const LOGO_DARK: &[u8] = include_bytes!("../../../docs/logo/galley-dark.svg");
/// The logo's size in the welcome, in logical pixels (its own proportions).
const LOGO_WIDTH: i32 = 360;
const LOGO_HEIGHT: i32 = 104;

/// The window's content.
#[derive(Debug)]
pub(crate) struct Content {
    window: gtk::Window,
    /// The document's view: none until a file is opened in the welcome.
    view: RefCell<Option<DocumentView>>,
    /// For a view made for a file opened in the welcome.
    offline: bool,
    zoom: f64,
    /// The options other windows inherit (`--offline`, `--theme`).
    viewer_args: Vec<String>,
    probe: Rc<Probe>,
}

impl Content {
    /// Makes the window take dropped files and Ctrl+O; it shows nothing until `show_view` or
    /// `show_welcome`.
    pub(crate) fn new(
        window: &gtk::Window,
        offline: bool,
        zoom: f64,
        viewer_args: Vec<String>,
        probe: &Rc<Probe>,
    ) -> Rc<Self> {
        let content = Rc::new(Self {
            window: window.clone(),
            view: RefCell::new(None),
            offline,
            zoom,
            viewer_args,
            probe: Rc::clone(probe),
        });
        content.accept_drops();
        content.add_open_shortcut();
        let weak = Rc::downgrade(&content);
        probe.set_opener(probe::Opener(Box::new(move |paths| {
            if let Some(content) = weak.upgrade() {
                content.open(paths);
            }
        })));
        content
    }

    /// Shows a document's view.
    pub(crate) fn show_view(&self, view: &DocumentView) {
        // The view draws its own scroll indicator (render/indicator.rs says why).
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .child(view)
            .build();
        self.probe.set_scroll_target(scrolled.vadjustment());
        let (looking, clicking, stating, counting, colouring, pressing) = (
            view.clone(),
            view.clone(),
            view.clone(),
            view.clone(),
            view.clone(),
            view.clone(),
        );
        let (menus, copying, buttons) = (view.clone(), view.clone(), view.clone());
        self.probe.set_view(probe::ViewProbe {
            link_at: Box::new(move |x, y| looking.link_label_at(x, y)),
            click_at: Box::new(move |x, y| clicking.probe_click(x, y)),
            state: Box::new(move || stating.probe_state()),
            images: Box::new(move || counting.probe_images()),
            highlights: Box::new(move || colouring.probe_highlights()),
            press: Box::new(move |key| pressing.probe_press(key)),
            menu_at: Box::new(move |x, y| menus.probe_menu(x, y)),
            copy: Box::new(move |index| copying.probe_copy(index)),
            copy_button: Box::new(move || buttons.probe_copy_button()),
        });
        self.window.set_child(Some(&scrolled));
        self.view.replace(Some(view.clone()));
    }

    /// Shows the logo, and how to open a file.
    pub(crate) fn show_welcome(&self) {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .width_request(LOGO_WIDTH)
            .height_request(LOGO_HEIGHT)
            .alternative_text("Galley")
            .build();
        // The logo for the background the theme gives, known once the window is styled.
        picture.connect_map(|picture| {
            let dark = crate::render::text::Palette::for_foreground(picture.color()).dark;
            let logo = if dark { LOGO_DARK } else { LOGO_LIGHT };
            picture.set_paintable(Some(&gtk::Svg::from_bytes(&glib::Bytes::from_static(logo))));
        });
        let hint = gtk::Label::new(Some(
            "Drop a Markdown file here, or press Ctrl+O to open one.",
        ));
        hint.add_css_class("dim-label");
        hint.set_wrap(true);
        hint.set_justify(gtk::Justification::Center);
        let welcome = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .margin_start(16)
            .margin_end(16)
            .build();
        welcome.append(&picture);
        welcome.append(&hint);
        self.window.set_child(Some(&welcome));
    }

    /// Opens the first file in this window and the rest in windows of their own.
    fn open(&self, paths: Vec<PathBuf>) {
        let mut paths = paths.into_iter();
        let Some(first) = paths.next() else {
            return;
        };
        let view = self.view.borrow().clone();
        let view = view.unwrap_or_else(|| {
            // From the welcome: an empty document until the file is read.
            let view = DocumentView::new(
                document_view::Opened {
                    paints: Vec::new(),
                    anchors: HashMap::new(),
                    details: Vec::new(),
                    images: Vec::new(),
                    code: HashMap::new(),
                    code_texts: Vec::new(),
                    path: None,
                    start: None,
                    offline: self.offline,
                    zoom: self.zoom,
                    viewer_args: self.viewer_args.clone(),
                },
                Rc::clone(&self.probe),
            );
            self.show_view(&view);
            view
        });
        view.open(first);
        for path in paths {
            if let Err(error) = document_view::open_in_new_window(&path, &self.viewer_args) {
                report_error(error);
            }
        }
    }

    /// Files dropped on the window open.
    ///
    /// The window's controllers own the content (GTK drops them when the window is disposed).
    fn accept_drops(self: &Rc<Self>) {
        let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let content = Rc::clone(self);
        target.connect_drop(move |_target, value, _x, _y| {
            let Ok(files) = value.get::<gdk::FileList>() else {
                return false;
            };
            // Local files only: a file manager's other locations have no path to read.
            let paths: Vec<PathBuf> = files.files().iter().filter_map(gio::File::path).collect();
            if paths.is_empty() {
                return false;
            }
            content.open(paths);
            true
        });
        self.window.add_controller(target);
    }

    /// Ctrl+O chooses a file to open.
    fn add_open_shortcut(self: &Rc<Self>) {
        let content = Rc::clone(self);
        let action = gtk::CallbackAction::new(move |_widget, _args| {
            content.choose_file();
            glib::Propagation::Stop
        });
        let shortcut = gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>o"),
            Some(action),
        );
        let controller = gtk::ShortcutController::new();
        controller.add_shortcut(shortcut);
        self.window.add_controller(controller);
    }

    fn choose_file(self: &Rc<Self>) {
        let markdown = gtk::FileFilter::new();
        markdown.set_name(Some("Markdown"));
        markdown.add_mime_type("text/markdown");
        markdown.add_suffix("md");
        markdown.add_suffix("markdown");
        let all = gtk::FileFilter::new();
        all.set_name(Some("All files"));
        all.add_pattern("*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&markdown);
        filters.append(&all);
        let dialog = gtk::FileDialog::builder()
            .title("Open")
            .modal(true)
            .filters(&filters)
            .default_filter(&markdown)
            .build();
        // Beside the file shown, if there is one.
        let folder = self
            .view
            .borrow()
            .as_ref()
            .and_then(DocumentView::path)
            .and_then(|path| path.parent().map(gio::File::for_path));
        if let Some(folder) = folder {
            dialog.set_initial_folder(Some(&folder));
        }
        let weak: Weak<Self> = Rc::downgrade(self);
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |chosen| {
            // Cancelled or dismissed: an error, and nothing to do.
            let Some(path) = chosen.ok().and_then(|file| file.path()) else {
                return;
            };
            if let Some(content) = weak.upgrade() {
                content.open(vec![path]);
            }
        });
    }
}
