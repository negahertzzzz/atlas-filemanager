//! Poison recovery for std locks.
//!
//! Release builds unwind on panic so a failing worker does not take the app
//! down. A worker that panics while holding a `Mutex` poisons it; without
//! recovery every later `lock().unwrap()` would re-panic (often on the UI
//! thread) and every `if let Ok(..) = lock()` would silently stop working.
//! The guarded data is plain state, so continuing with it is the safer choice.

use std::{convert::Infallible, sync::PoisonError};

/// `lock().or_else(recover)` yields the guard whether or not the lock is
/// poisoned, keeping `Result`-shaped call sites (`if let Ok`, `.ok()`) intact.
#[inline]
pub fn recover<G>(error: PoisonError<G>) -> Result<G, Infallible> {
    Ok(error.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    #[test]
    fn poisoned_mutex_still_yields_its_data() {
        let shared = Arc::new(Mutex::new(7));
        let worker = Arc::clone(&shared);
        let _ = std::thread::spawn(move || {
            let _guard = worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            panic!("poison");
        })
        .join();
        assert!(shared.is_poisoned());
        assert_eq!(*shared.lock().or_else(super::recover).unwrap(), 7);
    }
}
