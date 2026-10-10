//! Cloud uses a portable native session containing the source audio, not a flattened mix.
use sovereign_cloud_ui::{DocumentState, Editor, Snapshot};
pub struct Adapter<'a>(pub &'a mut soundcraft_ui_egui::SoundApp);
impl Editor for Adapter<'_> {
    fn document(&self) -> Option<DocumentState> {
        let e = &self.0.engine;
        Some(DocumentState {
            id: e.document_id(),
            revision: e.revision,
            name: e.session().name.clone(),
            dirty: e.is_dirty() || e.transport.recording,
        })
    }
    fn capture(&self) -> Result<Snapshot, String> {
        if self.0.engine.transport.recording {
            return Err("Stop recording before saving to cloud".into());
        }
        let document = self.document().ok_or("No session is open")?;
        let session = self.0.engine.session_arc();
        Ok(Snapshot { document, encode: Box::new(move || soundcraft_engine::portable::encode(&session)) })
    }
    fn saved(&mut self, id: u64, revision: u64, _bytes: &[u8]) -> Result<Option<u64>, String> {
        if !self.0.engine.transport.recording && self.document().is_some_and(|d| d.id == id && d.revision == revision) {
            self.0.engine.mark_clean();
            return Ok(Some(revision));
        }
        Ok(None)
    }
    fn open(&mut self, _name: &str, bytes: &[u8]) -> Result<DocumentState, String> {
        if self.0.engine.transport.recording {
            return Err("Stop recording before opening a cloud file".into());
        }
        let session = soundcraft_engine::portable::decode(bytes)?;
        if let Some(player) = &self.0.player {
            player.stop();
        }
        self.0.engine.replace_session(session);
        self.0.engine.path = None;
        // The next UI logic frame updates playback and waveform caches from the new revision.
        self.document().ok_or_else(|| "Could not open the audio session".into())
    }
    fn replaces_unsaved(&self) -> bool {
        self.0.engine.is_dirty() || self.0.engine.transport.recording
    }
}
