//! Portable session archive: native metadata and every decoded audio source, without paths
//! outside the archive. No filesystem access is used when reading or writing this format.
use cloud_zip as zip;
use soundcraft_audio_io::AudioBuffer;
use soundcraft_model::{Session, SourceAudio};
use std::{
    collections::BTreeSet,
    io::{Cursor, Read, Write},
    sync::Arc,
};

const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JSON: u64 = 8 * 1024 * 1024;
const MAX_SOURCES: usize = 1024;
const MANIFEST: &str = "session.json";

fn validate(s: &Session) -> Result<(), String> {
    if !s.videos.is_empty() {
        return Err("Portable sessions do not yet support linked video. Remove the video before saving to cloud.".into());
    }
    if s.sources.len() > MAX_SOURCES {
        return Err("Too many audio sources".into());
    }
    let mut ids = BTreeSet::new();
    for source in &s.sources {
        if !ids.insert(source.id) {
            return Err("Duplicate audio source ID".into());
        }
    }
    for clip in s.tracks.iter().flat_map(|t| &t.playlists).flat_map(|p| &p.clips) {
        if clip.video_source().is_some() {
            return Err("Portable sessions do not yet support linked video".into());
        }
        if clip.source().is_some_and(|id| !ids.contains(&id)) {
            return Err("A clip references missing audio".into());
        }
    }
    Ok(())
}

/// Keep float samples, clip edits, MIDI, effects and mixer settings in one portable file.
pub fn encode(session: &Session) -> Result<Vec<u8>, String> {
    validate(session)?;
    let mut s = session.clone();
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    // Store avoids decompression bombs and keeps our exact encoded/decoded size bound simple.
    let options = zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut total = 0u64;
    for src in &mut s.sources {
        let audio = s.pool.get(src.id).ok_or_else(|| format!("Audio is missing: {}", src.name))?;
        let buffer = &audio.buffer;
        if buffer.channels.is_empty()
            || buffer.channels.len() > 64
            || buffer.sample_rate == 0
            || buffer.channels.iter().any(|ch| ch.len() != buffer.frames())
        {
            return Err(format!("Invalid audio buffer: {}", src.name));
        }
        let size = (buffer.frames() as u64).saturating_mul(buffer.channels.len() as u64).saturating_mul(4).saturating_add(512);
        total = total.saturating_add(size);
        if total > MAX_BYTES {
            return Err("Portable session exceeds 64 MiB".into());
        }
        // WAV export clips samples to [-1, 1]. Native project storage must preserve
        // headroom too, so store planar IEEE f32 little-endian samples directly.
        if buffer.channels.iter().flatten().any(|sample| !sample.is_finite()) {
            return Err("Audio contains non-finite samples".into());
        }
        let bytes: Vec<u8> = buffer.channels.iter().flatten().flat_map(|sample| sample.to_le_bytes()).collect();
        src.path = format!("audio/{}.f32", src.id.0);
        src.channels = buffer.channels.len() as u16;
        src.frames = buffer.frames() as u64;
        src.sample_rate = buffer.sample_rate;
        src.unsaved = false;
        zip.start_file(&src.path, options).map_err(|e| e.to_string())?;
        zip.write_all(&bytes).map_err(|e| e.to_string())?;
    }
    let json = s.to_json().map_err(|e| e.to_string())?;
    if json.len() as u64 > MAX_JSON || total.saturating_add(json.len() as u64) > MAX_BYTES {
        return Err("Portable session exceeds size limits".into());
    }
    zip.start_file(MANIFEST, options).map_err(|e| e.to_string())?;
    zip.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
    let bytes = zip.finish().map_err(|e| e.to_string())?.into_inner();
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Portable session exceeds 64 MiB".into());
    }
    Ok(bytes)
}

fn entry(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut file = zip.by_name(name).map_err(|e| format!("Missing archive entry {name}: {e}"))?;
    if file.size() > limit {
        return Err("Archive entry exceeds size limit".into());
    }
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Archive entry exceeds size limit".into());
    }
    Ok(bytes)
}

/// Decode and validate the entire session before the editor replaces any open work.
pub fn decode(bytes: &[u8]) -> Result<Session, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Portable session exceeds 64 MiB".into());
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    if zip.len() > MAX_SOURCES + 1 {
        return Err("Too many archive entries".into());
    }
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let file = zip.by_index(i).map_err(|e| e.to_string())?;
        if !names.insert(file.name().to_string()) {
            return Err("Duplicate archive entry".into());
        }
        total = total.saturating_add(file.size());
        if total > MAX_BYTES {
            return Err("Unpacked session exceeds 64 MiB".into());
        }
    }
    let json = entry(&mut zip, MANIFEST, MAX_JSON)?;
    let mut session = Session::from_json(std::str::from_utf8(&json).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    validate(&session)?;
    if names.len() != session.sources.len() + 1 {
        return Err("Unexpected archive entries".into());
    }
    let mut decoded_bytes = 0u64;
    for source in &mut session.sources {
        let path = format!("audio/{}.f32", source.id.0);
        if source.path != path {
            return Err("Audio must refer to its archive entry".into());
        }
        let size = source.frames.checked_mul(u64::from(source.channels)).and_then(|n| n.checked_mul(4)).ok_or("Invalid audio length")?;
        decoded_bytes = decoded_bytes.saturating_add(size);
        if decoded_bytes > MAX_BYTES || source.channels == 0 || source.channels > 64 || source.sample_rate == 0 {
            return Err("Decoded audio exceeds size limits".into());
        }
        let data = entry(&mut zip, &path, size)?;
        if data.len() as u64 != size {
            return Err("Audio does not match its session metadata".into());
        }
        let mut samples = data.as_chunks::<4>().0.iter().map(|bytes| f32::from_le_bytes(*bytes));
        let frames = usize::try_from(source.frames).map_err(|_| "Invalid audio length")?;
        let channels: Vec<Vec<f32>> = (0..source.channels).map(|_| samples.by_ref().take(frames).collect()).collect();
        if channels.iter().flatten().any(|sample| !sample.is_finite()) {
            return Err("Audio contains non-finite samples".into());
        }
        let buffer = AudioBuffer { sample_rate: source.sample_rate, channels };
        source.unsaved = true; // A future local save must write the bundled source to disk.
        session.pool.insert(source.id, Arc::new(SourceAudio::new(buffer)));
    }
    Ok(session)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn document_identity_survives_edits_but_not_new_or_open() {
        let mut engine = crate::Engine::default();
        let first = engine.document_id();
        engine.session_mut().name = "Changed".into();
        assert_eq!(engine.document_id(), first);
        engine.replace_session(Session::default());
        assert_ne!(engine.document_id(), first);
    }
    #[test]
    fn portable_round_trip_keeps_audio_and_editable_session() {
        let engine = crate::demo::demo_engine();
        let session = engine.session();
        let bytes = encode(session).unwrap();
        let restored = decode(&bytes).unwrap();
        assert_eq!(restored.tracks, session.tracks);
        assert_eq!(restored.busses, session.busses);
        assert_eq!(restored.tempo, session.tempo);
        assert_eq!(restored.sources.len(), session.sources.len());
        for source in &session.sources {
            assert_eq!(restored.pool.get(source.id).unwrap().buffer, session.pool.get(source.id).unwrap().buffer);
        }
    }
    #[test]
    fn portable_audio_preserves_float_headroom_exactly() {
        let mut session = Session::default();
        let buffer = AudioBuffer { sample_rate: 48_000, channels: vec![vec![2.5, -3.25, 0.0, -0.0, f32::MIN_POSITIVE]] };
        let id = crate::io::add_source(&mut session, "Headroom", buffer.clone(), None, soundcraft_audio_io::FileFormat::Wav);
        let decoded = decode(&encode(&session).unwrap()).unwrap();
        let actual = &decoded.pool.get(id).unwrap().buffer;
        assert_eq!(
            actual.channels[0].iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
            buffer.channels[0].iter().map(|s| s.to_bits()).collect::<Vec<_>>()
        );
    }
    #[test]
    fn missing_audio_is_not_silently_saved() {
        let engine = crate::demo::demo_engine();
        let mut session = engine.session().clone();
        session.pool = Default::default();
        assert!(encode(&session).is_err());
    }
    #[test]
    fn corrupt_external_and_hostile_length_references_are_rejected() {
        assert!(decode(b"not a project").is_err());
        for hostile_length in [false, true] {
            let mut session = Session::default();
            crate::io::add_source(&mut session, "One", AudioBuffer::new(48_000, 1, 1), None, soundcraft_audio_io::FileFormat::Wav);
            let source = &mut session.sources[0];
            source.path = if hostile_length { format!("audio/{}.f32", source.id.0) } else { "../../outside.f32".into() };
            if hostile_length {
                source.frames = u64::MAX;
            }
            let path = source.path.clone();
            let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
            let options = zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zip.start_file(&path, options).unwrap();
            zip.write_all(&0.0f32.to_le_bytes()).unwrap();
            zip.start_file(MANIFEST, options).unwrap();
            zip.write_all(session.to_json().unwrap().as_bytes()).unwrap();
            assert!(decode(&zip.finish().unwrap().into_inner()).is_err());
        }
    }
}
