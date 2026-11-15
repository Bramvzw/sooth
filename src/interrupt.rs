//! Being asked to stop: Ctrl-C at a terminal, or a SIGTERM from CI, a
//! timeout wrapper or `kill`.
//!
//! Left alone, either signal ends sooth on the spot: the runs that finished
//! are never reported, a runner that was not signalled itself keeps going,
//! and sooth's temp files stay behind. So sooth takes the signal instead,
//! as a request: the run loop stops, the report covers what finished, and
//! every guard is dropped on the way out (see `DECISIONS.md`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Whether sooth has been asked to stop. Cheap to clone and to ask.
#[derive(Clone)]
pub struct Interrupt(Arc<AtomicBool>);

impl Interrupt {
    /// Start listening. From here on a first SIGINT or SIGTERM no longer
    /// ends sooth; it is recorded, for [`Interrupt::requested`] to report.
    /// A second one, while the first is being wound down, ends sooth the
    /// way a signal always did: asking twice means now.
    ///
    /// A command sooth spawns starts with the default action for both: a
    /// handler is not inherited across `exec`. That differs from before in
    /// one case. Where sooth itself was started with the signal ignored — a
    /// `sooth run … &` in a script without job control — it used to ignore
    /// a Ctrl-C and pass that on to its runner; now both stop. The old
    /// disposition cannot be asked for without `unsafe`.
    pub fn listen() -> Self {
        let requested = Arc::new(AtomicBool::new(false));
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            // In this order: the default action is taken only if the flag
            // was already set, by the signal before this one. A failed
            // registration leaves the signal's old meaning in place.
            let _ = signal_hook::flag::register_conditional_default(signal, Arc::clone(&requested));
            let _ = signal_hook::flag::register(signal, Arc::clone(&requested));
        }
        Self(requested)
    }

    /// A handle nobody can trigger, for code that runs without signals.
    #[cfg(test)]
    pub fn never() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// A handle that has already been triggered.
    #[cfg(test)]
    pub fn already() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    /// Whether a stop has been asked for.
    pub fn requested(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}
