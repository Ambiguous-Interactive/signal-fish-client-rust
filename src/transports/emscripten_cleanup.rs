//! Target-independent ownership state for the Emscripten callback allocation.
//!
//! Keeping this state machine free of FFI lets the ordinary workspace test
//! suite exercise close/delete outcomes when the Emscripten target is absent.

#[derive(Debug)]
pub(super) struct CleanupState {
    native_closed: bool,
    close_attempted: bool,
    callbacks_registered: bool,
}

/// Non-forgeable proof that a live callback registration transitioned to a
/// successfully deleted socket after native close was attempted or observed.
pub(super) struct ReclaimAuthorization(());

/// Non-forgeable proof that native close was attempted or peer close observed.
pub(super) struct DeleteAuthorization(());

impl CleanupState {
    pub(super) const fn new() -> Self {
        Self {
            native_closed: false,
            close_attempted: false,
            callbacks_registered: true,
        }
    }

    pub(super) const fn needs_close(&self) -> bool {
        self.callbacks_registered && !self.native_closed
    }

    pub(super) const fn needs_delete(&self) -> bool {
        self.callbacks_registered
    }

    pub(super) fn record_peer_close(&mut self) {
        self.close_attempted = true;
        self.native_closed = true;
    }

    pub(super) fn record_close_result(&mut self, succeeded: bool) {
        self.close_attempted = true;
        if succeeded {
            self.native_closed = true;
        }
    }

    pub(super) const fn delete_authorization(&self) -> Option<DeleteAuthorization> {
        if self.close_attempted && self.callbacks_registered {
            Some(DeleteAuthorization(()))
        } else {
            None
        }
    }

    /// Records deletion and returns a one-shot reclamation authorization only
    /// when close was attempted and live callbacks were successfully removed.
    pub(super) fn record_delete_result(
        &mut self,
        _authorization: DeleteAuthorization,
        succeeded: bool,
    ) -> Option<ReclaimAuthorization> {
        if succeeded && self.close_attempted && self.callbacks_registered {
            self.callbacks_registered = false;
            self.native_closed = true;
            Some(ReclaimAuthorization(()))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CleanupState;

    #[test]
    fn close_delete_outcome_matrix_preserves_callback_ownership() {
        for close_succeeds in [false, true] {
            for delete_succeeds in [false, true] {
                let mut state = CleanupState::new();
                assert!(state.needs_close());
                assert!(state.needs_delete());

                state.record_close_result(close_succeeds);
                assert_eq!(state.needs_close(), !close_succeeds);

                let reclaim = state
                    .delete_authorization()
                    .and_then(|authorization| {
                        state.record_delete_result(authorization, delete_succeeds)
                    })
                    .is_some();
                assert_eq!(reclaim, delete_succeeds);
                assert_eq!(state.needs_delete(), !delete_succeeds);
                assert_eq!(state.needs_close(), !close_succeeds && !delete_succeeds);
            }
        }
    }

    #[test]
    fn failed_operations_remain_retryable() {
        let mut state = CleanupState::new();
        state.record_close_result(false);
        assert!(state.needs_close());
        assert!(state
            .delete_authorization()
            .and_then(|authorization| state.record_delete_result(authorization, false))
            .is_none());
        assert!(state.needs_delete());

        state.record_close_result(true);
        assert!(!state.needs_close());
        assert!(state
            .delete_authorization()
            .and_then(|authorization| state.record_delete_result(authorization, true))
            .is_some());
        assert!(!state.needs_delete());
    }

    #[test]
    fn peer_close_skips_close_but_still_requires_deletion() {
        let mut state = CleanupState::new();
        state.record_peer_close();
        assert!(!state.needs_close());
        assert!(state.needs_delete());
        assert!(state
            .delete_authorization()
            .and_then(|authorization| state.record_delete_result(authorization, true))
            .is_some());
    }

    #[test]
    fn successful_deletion_authorizes_reclamation_exactly_once() {
        let mut state = CleanupState::new();
        state.record_close_result(true);
        assert!(state
            .delete_authorization()
            .and_then(|authorization| state.record_delete_result(authorization, true))
            .is_some());
        assert!(state.delete_authorization().is_none());
        assert!(!state.needs_close());
        assert!(!state.needs_delete());
    }

    #[test]
    fn delete_without_close_never_authorizes_reclamation() {
        let state = CleanupState::new();
        assert!(state.delete_authorization().is_none());
        assert!(state.needs_close());
        assert!(state.needs_delete());
    }
}
