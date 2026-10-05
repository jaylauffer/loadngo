//! The effect-mixing core both mobile backends share: clips decoded once to
//! interleaved stereo at the output rate, and the voices a platform audio
//! callback sums into its buffer. iOS (RemoteIO) and Android (AAudio) differ
//! only in how the callback is started and where clips are decoded; see
//! `docs/GAME_AUDIO_RUNTIME.md`.
// Desktop compiles this only for its tests.
#![cfg_attr(not(any(target_os = "ios", target_os = "android")), allow(dead_code))]

use std::io::Cursor;
use std::sync::Arc;

use lewton::inside_ogg::OggStreamReader;

/// Both mobile outputs run at 48 kHz stereo; every shipped effect already is.
pub(crate) const OUTPUT_SAMPLE_RATE: f64 = 48_000.0;

/// A whole effect, decoded once and kept. Interleaved stereo at the output
/// rate, so the callback only has to add and scale.
#[derive(Clone)]
pub(crate) struct CachedClip {
    pub(crate) samples: Arc<Vec<f32>>,
}

/// Decodes an Ogg Vorbis effect from memory. `name` only labels errors.
pub(crate) fn decode_clip_bytes(name: &str, bytes: Vec<u8>) -> Result<CachedClip, String> {
    let reader = OggStreamReader::new(Cursor::new(bytes))
        .map_err(|err| format!("Failed to read SFX {name}: {err}"))?;
    decode_clip_reader(name, reader)
}

/// Decodes an Ogg Vorbis effect from a file. iOS only; Android reads through
/// the proactor and calls `decode_clip_bytes`.
#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub(crate) fn decode_clip_file(path: &str) -> Result<CachedClip, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("Missing SFX {path}: {err}"))?;
    decode_clip_bytes(path, bytes)
}

fn decode_clip_reader<R: std::io::Read + std::io::Seek>(
    name: &str,
    mut reader: OggStreamReader<R>,
) -> Result<CachedClip, String> {
    let source_rate = f64::from(reader.ident_hdr.audio_sample_rate);
    let source_channels = usize::from(reader.ident_hdr.audio_channels).max(1);
    let mut interleaved: Vec<f32> = Vec::new();
    while let Some(packet) = reader
        .read_dec_packet_itl()
        .map_err(|err| format!("Failed to decode SFX {name}: {err}"))?
    {
        for frame in packet.chunks(source_channels) {
            let left = f32::from(frame[0]) / 32_768.0;
            let right = if source_channels > 1 {
                f32::from(frame[1]) / 32_768.0
            } else {
                left
            };
            interleaved.push(left);
            interleaved.push(right);
        }
    }
    if interleaved.is_empty() {
        return Err(format!("SFX {name} contains no audio samples"));
    }
    // Effects ship at 48 kHz already; resample only if that ever changes.
    let samples = if (source_rate - OUTPUT_SAMPLE_RATE).abs() < f64::EPSILON {
        interleaved
    } else {
        resample_interleaved(&interleaved, source_rate / OUTPUT_SAMPLE_RATE)
    };
    Ok(CachedClip {
        samples: Arc::new(samples),
    })
}

fn resample_interleaved(input: &[f32], ratio: f64) -> Vec<f32> {
    let frames = input.len() / 2;
    if frames < 2 {
        return input.to_vec();
    }
    let mut out = Vec::with_capacity(((frames as f64 / ratio) as usize + 1) * 2);
    let mut position = 0.0f64;
    while (position.floor() as usize) + 1 < frames {
        let index = position.floor() as usize;
        let fraction = (position - position.floor()) as f32;
        for channel in 0..2 {
            let a = input[index * 2 + channel];
            let b = input[(index + 1) * 2 + channel];
            out.push(a + (b - a) * fraction);
        }
        position += ratio;
    }
    out
}

pub(crate) struct ActiveVoice {
    pub(crate) samples: Arc<Vec<f32>>,
    pub(crate) cursor: usize,
    pub(crate) left: f32,
    pub(crate) right: f32,
    pub(crate) looped: bool,
}

/// The voices a callback mixes, keyed by `SfxVoiceId` value. Reserved up
/// front so admitting a voice under the callback's lock never allocates.
pub(crate) struct Voices {
    pub(crate) list: Vec<(u64, ActiveVoice)>,
}

impl Default for Voices {
    fn default() -> Self {
        Self::with_capacity(64)
    }
}

impl Voices {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            list: Vec::with_capacity(capacity),
        }
    }

    /// Adds every live voice into `out` (interleaved stereo), dropping the
    /// ones that finish. Allocation-free, so it is safe in an audio callback.
    pub(crate) fn mix(&mut self, out: &mut [f32]) {
        self.list.retain_mut(|(_, voice)| {
            let mut index = 0;
            while index + 1 < out.len() {
                if voice.cursor + 1 >= voice.samples.len() {
                    if !voice.looped {
                        return false;
                    }
                    voice.cursor = 0;
                }
                out[index] += voice.samples[voice.cursor] * voice.left;
                out[index + 1] += voice.samples[voice.cursor + 1] * voice.right;
                voice.cursor += 2;
                index += 2;
            }
            true
        });
    }

    pub(crate) fn contains(&self, id: u64) -> bool {
        self.list.iter().any(|(voice, _)| *voice == id)
    }

    pub(crate) fn remove(&mut self, id: u64) {
        self.list.retain(|(voice, _)| *voice != id);
    }

    /// Used by Android's live mix-volume change; iOS leaves live voices alone.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub(crate) fn set_gains(&mut self, id: u64, left: f32, right: f32) {
        if let Some((_, voice)) = self.list.iter_mut().find(|(voice, _)| *voice == id) {
            voice.left = left;
            voice.right = right;
        }
    }
}

/// Pan law shared by every mobile backend: a linear left/right split.
pub(crate) fn stereo_volume(volume: f32, pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    (volume * (1.0 - pan.max(0.0)), volume * (1.0 + pan.min(0.0)))
}

#[cfg(test)]
mod tests {
    use super::{stereo_volume, ActiveVoice, Voices};
    use std::sync::Arc;

    fn voice(samples: &[f32], looped: bool) -> ActiveVoice {
        ActiveVoice {
            samples: Arc::new(samples.to_vec()),
            cursor: 0,
            left: 1.0,
            right: 0.5,
            looped,
        }
    }

    #[test]
    fn a_one_shot_voice_mixes_once_then_leaves() {
        let mut voices = Voices::with_capacity(4);
        voices.list.push((1, voice(&[0.25, 0.25, 0.5, 0.5], false)));
        let mut out = [0.0f32; 8];
        voices.mix(&mut out);
        assert_eq!(out, [0.25, 0.125, 0.5, 0.25, 0.0, 0.0, 0.0, 0.0]);
        assert!(!voices.contains(1));
    }

    #[test]
    fn a_looped_voice_wraps_and_stays() {
        let mut voices = Voices::with_capacity(4);
        voices.list.push((7, voice(&[0.1, 0.1], true)));
        let mut out = [0.0f32; 6];
        voices.mix(&mut out);
        assert!((out[4] - 0.1).abs() < f32::EPSILON);
        assert!(voices.contains(7));
        voices.set_gains(7, 0.0, 0.0);
        voices.remove(7);
        assert!(!voices.contains(7));
    }

    #[test]
    fn mixing_does_not_allocate_beyond_the_reserved_capacity() {
        let mut voices = Voices::with_capacity(24);
        let capacity = voices.list.capacity();
        for id in 0..24 {
            voices.list.push((id, voice(&[0.0; 64], false)));
        }
        let mut out = [0.0f32; 32];
        voices.mix(&mut out);
        assert_eq!(voices.list.capacity(), capacity);
    }

    #[test]
    fn pan_is_a_linear_split() {
        assert_eq!(stereo_volume(1.0, 0.0), (1.0, 1.0));
        assert_eq!(stereo_volume(1.0, 1.0), (0.0, 1.0));
        assert_eq!(stereo_volume(1.0, -1.0), (1.0, 0.0));
    }
}
