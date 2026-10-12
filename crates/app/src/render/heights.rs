//! Exact leaf heights, measured off the main thread (docs/plan.md §4.5).
//!
//! Pango layouts cannot cross threads, but heights are numbers. The worker has its own
//! `pangocairo` font map and a context set up like the widget's (`TextSetup`), lays out each
//! leaf the widget's way (`text::layout`), and sends heights back in batches. A new job (a new
//! width) supersedes the old one at the next batch.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use gtk::prelude::FontMapExt as _;

use super::paint::LeafPaint;
use super::text::{self, Palette, TextSetup};

/// Leaves per batch: small enough to switch jobs quickly, large enough to keep sends cheap.
const BATCH: usize = 128;

#[derive(Debug, Clone)]
pub(crate) struct Job {
    pub(crate) generation: u64,
    pub(crate) view_text_width: i64,
    pub(crate) paints: Arc<Vec<LeafPaint>>,
    pub(crate) setup: TextSetup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Batch {
    pub(crate) generation: u64,
    /// The first leaf of `heights`.
    pub(crate) first: usize,
    /// Full leaf heights (text, padding and gaps), in logical pixels.
    pub(crate) heights: Vec<i64>,
    /// Whether this is the job's last batch.
    pub(crate) done: bool,
}

#[derive(Debug)]
pub(crate) struct Worker {
    jobs: Sender<Job>,
    batches: Receiver<Batch>,
}

impl Worker {
    pub(crate) fn spawn() -> Self {
        let (jobs, job_receiver) = mpsc::channel::<Job>();
        let (batch_sender, batches) = mpsc::channel::<Batch>();
        thread::spawn(move || run(&job_receiver, &batch_sender));
        Self { jobs, batches }
    }

    /// Starts measuring; any job in progress is abandoned at its next batch.
    pub(crate) fn submit(&self, job: Job) {
        // A send fails only if the worker thread has ended; heights then stay estimates.
        drop(self.jobs.send(job));
    }

    /// The batches ready now.
    pub(crate) fn ready(&self) -> Vec<Batch> {
        self.batches.try_iter().collect()
    }
}

fn run(jobs: &Receiver<Job>, batches: &Sender<Batch>) {
    let font_map = pangocairo::FontMap::new();
    let context = font_map.create_context();
    let mut next = jobs.recv().ok();
    while let Some(job) = next.take() {
        job.setup.apply(&context);
        let metrics = job.setup.metrics();
        let palette = Palette::default();
        let mut first = 0;
        let total = job.paints.len();
        loop {
            // A newer job replaces this one.
            match jobs.try_recv() {
                Ok(newer) => {
                    next = Some(newer);
                    break;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return,
            }
            let end = (first + BATCH).min(total);
            let heights = job
                .paints
                .get(first..end)
                .unwrap_or_default()
                .iter()
                .map(|paint| {
                    let width = metrics.text_width(paint, job.view_text_width);
                    let layout =
                        text::layout(&context, job.setup, palette, paint, width, &mut |_bytes| {});
                    metrics.leaf_height(paint, i64::from(layout.pixel_size().1))
                })
                .collect();
            let done = end >= total;
            if batches
                .send(Batch {
                    generation: job.generation,
                    first,
                    heights,
                    done,
                })
                .is_err()
            {
                return;
            }
            if done {
                next = jobs.recv().ok();
                break;
            }
            first = end;
        }
    }
}
