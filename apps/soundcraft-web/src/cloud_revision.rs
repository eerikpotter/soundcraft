//! Cloud revisions track document edits independently of local saves and navigation.
use soundcraft_engine::Engine;
use soundcraft_model::Session;
use std::{cell::RefCell, sync::Arc};

struct Observed {
    document_id: u64,
    session: Arc<Session>,
    revision: u64,
}

#[derive(Default)]
pub struct RevisionTracker(RefCell<Option<Observed>>);

impl RevisionTracker {
    pub fn observe(&self, engine: &Engine) -> u64 {
        let mut observed = self.0.borrow_mut();
        let session = engine.session_arc();
        if let Some(previous) = observed.as_mut()
            && previous.document_id == engine.document_id()
        {
            if !Arc::ptr_eq(&previous.session, &session) {
                if !engine.same_document(&previous.session) {
                    previous.revision = previous.revision.wrapping_add(1);
                }
                previous.session = session;
            }
            return previous.revision;
        }
        *observed = Some(Observed { document_id: engine.document_id(), session, revision: 0 });
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_revision_ignores_navigation_but_survives_local_save() {
        let mut engine = Engine::default();
        let tracker = RevisionTracker::default();
        let initial = tracker.observe(&engine);
        let redraw_revision = engine.revision;
        engine.session_mut().edit.playhead = 48000;
        assert_ne!(engine.revision, redraw_revision);
        assert_eq!(tracker.observe(&engine), initial);
        engine.session_mut().name = "Edited session".into();
        // A local download must not hide an edit from cloud autosave.
        engine.mark_clean();
        let captured = tracker.observe(&engine);
        assert_ne!(captured, initial);
        engine.session_mut().edit.playhead = 96000;
        assert_eq!(tracker.observe(&engine), captured);
        engine.session_mut().name = "Edit during upload".into();
        assert_ne!(tracker.observe(&engine), captured);
    }

    #[test]
    fn cloud_revision_restarts_for_a_new_document_identity() {
        let mut engine = Engine::default();
        let tracker = RevisionTracker::default();
        let initial = tracker.observe(&engine);
        let id = engine.document_id();
        engine.session_mut().name = "First document edit".into();
        assert_ne!(tracker.observe(&engine), initial);
        engine.replace_session(Session::default());
        assert_ne!(engine.document_id(), id);
        assert_eq!(tracker.observe(&engine), initial);
    }
}
