//! Notices (docs/plan.md §4.5, "As built in M6"): a message at the bottom of the view for a few
//! seconds, and on stderr. Drawn by the view like its scroll indicator; the layout is made when
//! the notice is set, never inside a frame.

use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk, pango};

use super::{DocumentView, OpenError};
use crate::loading::LoadError;
use crate::output::report_error;
use crate::units;

/// How long a notice stays.
const NOTICE_FOR: Duration = Duration::from_secs(6);

/// What a notice says.
#[derive(Debug, thiserror::Error)]
pub(in crate::document_view) enum Message {
    #[error(transparent)]
    Open(#[from] OpenError),
    #[error("the file was removed; this is its last version")]
    Removed,
    #[error("the file changed but could not be read again: {0}")]
    Reload(LoadError),
    #[error("the file changed but could not be read again ({0:?})")]
    Spawn(std::io::ErrorKind),
    #[error("changes to the file will not show by themselves ({0}); Ctrl+R reloads it")]
    Unwatched(String),
    #[error("no application to edit text with (set one for plain text files)")]
    NoEditor,
    #[error("cannot open the file in {0}: {1}")]
    Editor(String, String),
}

/// A notice on show: laid out when it was set, and the timer that hides it.
#[derive(Debug)]
pub(in crate::document_view) struct Shown {
    layout: pango::Layout,
    hide: glib::SourceId,
}

impl DocumentView {
    /// Shows `message` at the bottom of the view for `NOTICE_FOR`, and on stderr.
    pub(in crate::document_view) fn notice(&self, message: &Message) {
        report_error(message);
        let context = self.obj().pango_context();
        let width = i64::from(self.obj().width());
        let layout = {
            let document = self.document.borrow();
            let Some(doc) = document.as_ref() else {
                return;
            };
            doc.probe.notice_shown();
            let probe = Rc::clone(&doc.probe);
            let width = width - 4 * doc.margin();
            doc.typesetter(context)
                .caption(&message.to_string(), width, &mut |bytes| {
                    probe.layout_created(bytes);
                })
        };
        let hide = glib::timeout_add_local_once(
            NOTICE_FOR,
            glib::clone!(
                #[weak(rename_to = view)]
                self.obj(),
                move || {
                    // Its timer has fired: dropping it is all that is left.
                    view.imp().notice.take();
                    view.queue_draw();
                }
            ),
        );
        if let Some(old) = self.notice.replace(Some(Shown { layout, hide })) {
            old.hide.remove();
        }
        self.obj().queue_draw();
    }

    /// Takes a notice down before its time (the file it was about is back).
    pub(in crate::document_view) fn clear_notice(&self) {
        if let Some(old) = self.notice.take() {
            old.hide.remove();
            self.obj().queue_draw();
        }
    }

    /// The notice, if any: its text in `text` on a box of `fill`, centred near the bottom.
    pub(in crate::document_view) fn draw_notice(
        &self,
        snapshot: &gtk::Snapshot,
        (fill, text): (gtk::gdk::RGBA, gtk::gdk::RGBA),
    ) {
        let notice = self.notice.borrow();
        let Some(Shown { layout, .. }) = notice.as_ref() else {
            return;
        };
        let (width, height) = layout.pixel_size();
        let (width, height) = (f64::from(width), f64::from(height));
        let pad = height / 2.0;
        let x = (f64::from(self.obj().width()) - width) / 2.0;
        let y = f64::from(self.obj().height()) - height - 3.0 * pad;
        let rect = graphene::Rect::new(
            units::to_f32(x - 2.0 * pad),
            units::to_f32(y - pad),
            units::to_f32(width + 4.0 * pad),
            units::to_f32(height + 2.0 * pad),
        );
        snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, units::to_f32(pad)));
        snapshot.append_color(&fill, &rect);
        snapshot.pop();
        snapshot.save();
        snapshot.translate(&graphene::Point::new(units::to_f32(x), units::to_f32(y)));
        snapshot.append_layout(layout, &text);
        snapshot.restore();
    }
}
