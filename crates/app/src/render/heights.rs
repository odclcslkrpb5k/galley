//! Exact leaf heights, measured off the main thread (docs/plan.md §4.5).
//!
//! Pango layouts cannot cross threads, but heights are numbers. The worker has its own
//! `pangocairo` font map and a context set up like the widget's (`TextSetup`), lays out each
//! leaf the widget's way (`Typesetter::measure`), and sends heights back in batches. A new job (a new
//! width) supersedes the old one at the next batch.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use gtk::prelude::FontMapExt as _;

use super::paint::LeafPaint;
use super::text::{ColumnWidths, Palette, TextSetup, Typesetter};

/// Most leaves per batch, and most time a batch may take: a newer job (a resize) waits for the
/// batch in progress, and a table piece is dozens of layouts, so a count alone could make that
/// hundreds of milliseconds. Large enough to keep sends cheap.
const BATCH: usize = 128;
const BATCH_TIME: Duration = Duration::from_millis(20);

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

/// The worker thread has ended (it panicked): heights stay estimates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the height worker has stopped; leaf heights stay estimated until laid out")]
pub(crate) struct WorkerGone;

impl Worker {
    pub(crate) fn spawn() -> Self {
        let (jobs, job_receiver) = mpsc::channel::<Job>();
        let (batch_sender, batches) = mpsc::channel::<Batch>();
        thread::spawn(move || run(&job_receiver, &batch_sender));
        Self { jobs, batches }
    }

    /// Starts measuring; any job in progress is abandoned at its next batch.
    pub(crate) fn submit(&self, job: Job) -> Result<(), WorkerGone> {
        self.jobs.send(job).map_err(|_job| WorkerGone)
    }

    /// The batches ready now.
    pub(crate) fn ready(&self) -> Result<Vec<Batch>, WorkerGone> {
        let mut ready = Vec::new();
        loop {
            match self.batches.try_recv() {
                Ok(batch) => ready.push(batch),
                Err(TryRecvError::Empty) => return Ok(ready),
                // Batches sent before the thread ended still count.
                Err(TryRecvError::Disconnected) if !ready.is_empty() => return Ok(ready),
                Err(TryRecvError::Disconnected) => return Err(WorkerGone),
            }
        }
    }
}

fn run(jobs: &Receiver<Job>, batches: &Sender<Batch>) {
    let font_map = pangocairo::FontMap::new();
    let context = font_map.create_context();
    let mut next = jobs.recv().ok();
    let mut columns = ColumnWidths::default();
    let mut setup = None;
    while let Some(job) = next.take() {
        job.setup.apply(&context);
        let metrics = job.setup.metrics();
        let typesetter = Typesetter {
            context: context.clone(),
            setup: job.setup,
            palette: Palette::default(),
        };
        // Natural column widths hold for as long as the text setup does.
        if setup.replace(job.setup) != Some(job.setup) {
            columns.clear();
        }
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
            let started = Instant::now();
            let mut heights = Vec::new();
            let mut end = first;
            while let Some(paint) = job.paints.get(end)
                && heights.len() < BATCH
                && (heights.is_empty() || started.elapsed() < BATCH_TIME)
            {
                // The worker's layouts are its own; the probe counts main-thread work only.
                let measured =
                    typesetter.measure(paint, job.view_text_width, &mut columns, &mut |_bytes| {});
                heights.push(metrics.leaf_height(paint, measured.height));
                end += 1;
            }
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
