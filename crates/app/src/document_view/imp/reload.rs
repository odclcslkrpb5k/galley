//! Reloading the file when it changes (docs/plan.md §4.5, "As built in M6").
//!
//! A `gio::FileMonitor` on the file (its target, if it is a symlink: a monitor on a link sees
//! nothing when the target is written) starts after the first frame. Change events schedule a
//! reload (`delay`): soon after the events that end a save, later after a bare write (a slow
//! writer may still be at it), and never later than `MAX_WAIT` after the first change, so a file
//! rewritten continuously still reloads. The file is read and parsed on a thread with the
//! startup loader, then shown through `set_document` with the same zoom and the same paint at
//! the top. An empty file where there was text is read once more before it is shown (a writer
//! that truncates first), and a file gone is noticed, its last version left on show.

use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use super::DocumentView;
use super::notice::Message;
use crate::document_view::{Opened, Start};
use crate::loading::{self, LoadError, Loaded};

/// After an event that ends a save (closed, created, renamed in, deleted).
const SETTLE: Duration = Duration::from_millis(200);
/// After a bare write: the writer may still be writing (`./generate > README.md` wrote, then
/// paused half a second, in the M6 review).
const SETTLE_WRITING: Duration = Duration::from_secs(2);
/// The most a reload waits after the first change it is for.
const MAX_WAIT: Duration = Duration::from_secs(1);
/// How long before an empty file that replaced text is read again.
const CONFIRM_EMPTY: Duration = Duration::from_millis(500);
/// How often the reading thread is checked for being done.
const READ_POLL: Duration = Duration::from_millis(16);

/// The view's watching and reloading.
#[derive(Debug, Default)]
pub(in crate::document_view) struct Reload {
    /// Started after the first frame, once.
    watched: bool,
    monitor: Option<gio::FileMonitor>,
    /// The reload waiting for changes to settle, and when the first of them came.
    soon: Option<(glib::SourceId, Instant)>,
    /// The file being read, and the poll waiting for it.
    reading: Option<(JoinHandle<Result<Loaded, LoadError>>, glib::SourceId)>,
    /// The file changed while being read: read it once more after.
    again: bool,
    /// The last read was empty where there was text: being read once more.
    confirming: bool,
}

/// What a change event means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    /// A save has ended (closed, created, renamed in), or the file is gone.
    Done,
    /// Written; more may come.
    Writing,
}

fn change(event: gio::FileMonitorEvent) -> Option<Change> {
    match event {
        gio::FileMonitorEvent::ChangesDoneHint
        | gio::FileMonitorEvent::Created
        | gio::FileMonitorEvent::MovedIn
        | gio::FileMonitorEvent::Renamed
        | gio::FileMonitorEvent::Deleted
        | gio::FileMonitorEvent::MovedOut => Some(Change::Done),
        gio::FileMonitorEvent::Changed => Some(Change::Writing),
        // Not changes of content; nor, presumably, any event gio adds later.
        gio::FileMonitorEvent::AttributeChanged
        | gio::FileMonitorEvent::PreUnmount
        | gio::FileMonitorEvent::Unmounted
        | gio::FileMonitorEvent::Moved
        | _ => None,
    }
}

/// How long until the reload, after a change of kind `change` at `now`, the first change it
/// waits for having come at `first`.
fn delay(change: Change, first: Instant, now: Instant) -> Duration {
    let settle = match change {
        Change::Done => SETTLE,
        Change::Writing => SETTLE_WRITING,
    };
    settle.min((first + MAX_WAIT).saturating_duration_since(now))
}

impl DocumentView {
    /// Starts watching the file, once, after the first frame (setting up inotify is not free).
    pub(in crate::document_view) fn watch(&self) {
        let Some(path) = self.path.borrow().clone() else {
            return;
        };
        if std::mem::replace(&mut self.reload.borrow_mut().watched, true) {
            return;
        }
        let target = std::fs::canonicalize(&path).unwrap_or(path);
        let monitor = gio::File::for_path(&target)
            .monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE);
        match monitor {
            Ok(monitor) => {
                monitor.connect_changed(glib::clone!(
                    #[weak(rename_to = view)]
                    self.obj(),
                    move |_monitor, _file, _other, event| view.imp().file_changed(event)
                ));
                self.reload.borrow_mut().monitor = Some(monitor);
            }
            Err(error) => self.notice(&Message::Unwatched(error.to_string())),
        }
    }

    fn file_changed(&self, event: gio::FileMonitorEvent) {
        let Some(change) = change(event) else {
            return;
        };
        let now = Instant::now();
        let first = match self.reload.borrow_mut().soon.take() {
            Some((source, first)) => {
                source.remove();
                first
            }
            None => now,
        };
        let source = glib::timeout_add_local_once(
            delay(change, first, now),
            glib::clone!(
                #[weak(rename_to = view)]
                self.obj(),
                move || {
                    // It has fired: forget it, then reload.
                    view.imp().reload.borrow_mut().soon.take();
                    view.imp().reload();
                }
            ),
        );
        self.reload.borrow_mut().soon = Some((source, first));
    }

    /// Reads the file again on a thread (Ctrl+R, F5, or a change); `reload_poll` shows it.
    pub(in crate::document_view) fn reload(&self) {
        let Some(path) = self.path.borrow().clone() else {
            return;
        };
        if self.reload.borrow().reading.is_some() {
            self.reload.borrow_mut().again = true;
            return;
        }
        let reader = thread::Builder::new()
            .name("reload".to_owned())
            .spawn(move || loading::load(&path));
        let reader = match reader {
            Ok(reader) => reader,
            Err(error) => {
                self.notice(&Message::Spawn(error.kind()));
                return;
            }
        };
        let poll = glib::timeout_add_local(
            READ_POLL,
            glib::clone!(
                #[weak(rename_to = view)]
                self.obj(),
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || view.imp().reload_poll()
            ),
        );
        self.reload.borrow_mut().reading = Some((reader, poll));
    }

    fn reload_poll(&self) -> glib::ControlFlow {
        let finished = self
            .reload
            .borrow()
            .reading
            .as_ref()
            .is_some_and(|(reader, _poll)| reader.is_finished());
        if !finished {
            return glib::ControlFlow::Continue;
        }
        // The poll ends by returning Break, not by removing its source.
        let Some((reader, _poll)) = self.reload.borrow_mut().reading.take() else {
            return glib::ControlFlow::Break;
        };
        match reader.join().unwrap_or(Err(LoadError::LoaderPanicked)) {
            Ok(document) => self.loaded(document),
            Err(LoadError::Read(std::io::ErrorKind::NotFound)) => self.notice(&Message::Removed),
            Err(error) => self.notice(&Message::Reload(error)),
        }
        if std::mem::take(&mut self.reload.borrow_mut().again) {
            self.reload();
        }
        glib::ControlFlow::Break
    }

    /// Shows a reloaded document, unless it is empty where there was text and has not been read
    /// twice.
    fn loaded(&self, document: Loaded) {
        let had_text = self
            .document
            .borrow()
            .as_ref()
            .is_some_and(|doc| !doc.paints.is_empty());
        let confirmed = std::mem::take(&mut self.reload.borrow_mut().confirming);
        if document.paints.is_empty() && had_text && !confirmed {
            self.reload.borrow_mut().confirming = true;
            glib::timeout_add_local_once(
                CONFIRM_EMPTY,
                glib::clone!(
                    #[weak(rename_to = view)]
                    self.obj(),
                    move || view.imp().reload()
                ),
            );
            return;
        }
        self.clear_notice();
        self.replace_document(document);
    }

    /// Shows a reloaded document in place of the current one: same zoom, same options, and the
    /// paint (by position) at the top kept there.
    fn replace_document(&self, loaded: Loaded) {
        let kept = {
            let document = self.document.borrow();
            let Some(doc) = document.as_ref() else {
                return;
            };
            let top = self.viewport().map_or(0, |(top, _page)| top);
            let paint = doc.positions.anchor(top - doc.margin()).unwrap_or(0);
            (paint, std::rc::Rc::clone(&doc.probe))
        };
        let (paint, probe) = kept;
        let Loaded {
            paints,
            anchors,
            details,
            images,
            code,
        } = loaded;
        // Taken first: a borrow inside the call below would last through it, and
        // `set_document` replaces these.
        let path: Option<PathBuf> = self.path.borrow().clone();
        let viewer_args = self.viewer_args.borrow().clone();
        self.set_document(
            Opened {
                paints,
                anchors,
                details,
                images,
                code,
                path,
                start: Some(Start::Paint(paint)),
                offline: self.offline.get(),
                zoom: self.zoom.get(),
                viewer_args,
            },
            std::rc::Rc::clone(&probe),
        );
        probe.reloaded();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_reloads_soon_a_bare_write_later_and_neither_waits_long() {
        let first = Instant::now();
        let at = |ms| first + Duration::from_millis(ms);
        // The first change.
        assert_eq!(delay(Change::Done, first, first), SETTLE);
        assert_eq!(delay(Change::Writing, first, first), MAX_WAIT);
        // Later changes: at most MAX_WAIT after the first.
        assert_eq!(delay(Change::Done, first, at(300)), SETTLE);
        assert_eq!(
            delay(Change::Done, first, at(900)),
            Duration::from_millis(100)
        );
        assert_eq!(
            delay(Change::Writing, first, at(400)),
            Duration::from_millis(600)
        );
        assert_eq!(delay(Change::Writing, first, at(1500)), Duration::ZERO);
        // Which events are which.
        assert_eq!(
            change(gio::FileMonitorEvent::ChangesDoneHint),
            Some(Change::Done)
        );
        assert_eq!(change(gio::FileMonitorEvent::Renamed), Some(Change::Done));
        assert_eq!(change(gio::FileMonitorEvent::Deleted), Some(Change::Done));
        assert_eq!(change(gio::FileMonitorEvent::Created), Some(Change::Done));
        assert_eq!(change(gio::FileMonitorEvent::MovedIn), Some(Change::Done));
        assert_eq!(change(gio::FileMonitorEvent::MovedOut), Some(Change::Done));
        assert_eq!(
            change(gio::FileMonitorEvent::Changed),
            Some(Change::Writing)
        );
        assert_eq!(change(gio::FileMonitorEvent::AttributeChanged), None);
    }
}
