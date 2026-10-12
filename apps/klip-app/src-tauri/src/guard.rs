//! The app's own threads catch panics (AGENTS rule 3): one panicking handler must not
//! stop every later engine event, or kill a timer, without a word. The panic hook in
//! `logging` has already logged the message; this says where it was caught.

use std::panic::{AssertUnwindSafe, catch_unwind};

/// Run `f`; a panic is logged with `what` and becomes `None`.
pub fn guarded<T>(what: &str, f: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Some(v),
        Err(_) => {
            tracing::error!(what, "a panic was caught; going on");
            None
        }
    }
}

/// Call `each` for every item; a panic in one call is caught and the loop goes on.
pub fn pump<I>(items: impl IntoIterator<Item = I>, what: &str, mut each: impl FnMut(&I)) {
    for item in items {
        guarded(what, || each(&item));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]

    use super::*;

    /// Regression (IMPROVEMENTS B4): the events loop had no guard, so one panic in
    /// `tray::update` or `overlay.on_event` ended it and the UI got no more events.
    #[test]
    fn the_pump_goes_on_after_a_panic() {
        let mut seen = Vec::new();
        pump(0..4, "test", |i| {
            seen.push(*i);
            if *i == 1 {
                panic!("handler failed on purpose");
            }
        });
        assert_eq!(seen, vec![0, 1, 2, 3]);
    }

    #[test]
    fn guarded_returns_the_value_or_none() {
        assert_eq!(guarded("ok", || 7), Some(7));
        assert_eq!(guarded("boom", || -> i32 { panic!("on purpose") }), None);
    }
}
