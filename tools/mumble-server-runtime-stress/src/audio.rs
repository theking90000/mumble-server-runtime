use std::num::NonZeroUsize;
use std::path::Path;

use anyhow::{Context, Result, ensure};

/// Duration represented by one prepared Opus packet.
///
/// REF: references/mumble/src/mumble/AudioInput.h:iFrameSize.
pub const OPUS_FRAME_DURATION: std::time::Duration = std::time::Duration::from_millis(10);

/// A pre-encoded clip made of fixed-size, raw Opus packets.
#[derive(Debug)]
pub struct VoiceClip {
    bytes: Vec<u8>,
    frame_bytes: NonZeroUsize,
    frames: usize,
}

impl VoiceClip {
    pub fn load(path: &Path, frame_bytes: NonZeroUsize) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading Opus packet file {}", path.display()))?;
        Self::from_bytes(bytes, frame_bytes)
            .with_context(|| format!("invalid Opus packet file {}", path.display()))
    }

    fn from_bytes(bytes: Vec<u8>, frame_bytes: NonZeroUsize) -> Result<Self> {
        ensure!(!bytes.is_empty(), "the file is empty");
        ensure!(
            bytes.len().is_multiple_of(frame_bytes.get()),
            "{} bytes cannot be divided into {}-byte Opus packets",
            bytes.len(),
            frame_bytes
        );
        let frames = bytes.len() / frame_bytes;
        Ok(Self {
            bytes,
            frame_bytes,
            frames,
        })
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes.get()
    }

    /// Return the next raw packet, wrapping around when the clip ends.
    ///
    /// REF: references/mumble/src/MumbleUDP.proto:Audio.opus_data.
    pub fn packet(&self, index: usize) -> Option<&[u8]> {
        let frame = index % self.frames;
        let start = frame.checked_mul(self.frame_bytes.get())?;
        let end = start.checked_add(self.frame_bytes.get())?;
        self.bytes.get(start..end)
    }

    pub fn next_index(&self, index: usize) -> usize {
        if index >= self.frames.saturating_sub(1) {
            0
        } else {
            index + 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_size_packets_loop_without_copying() -> Result<()> {
        let clip = VoiceClip::from_bytes(
            vec![1, 2, 3, 4, 5, 6],
            NonZeroUsize::new(2).context("two is nonzero")?,
        )?;

        assert_eq!(clip.frames(), 3);
        assert_eq!(clip.packet(0), Some(&[1, 2][..]));
        assert_eq!(clip.packet(2), Some(&[5, 6][..]));
        assert_eq!(clip.packet(3), Some(&[1, 2][..]));
        assert_eq!(clip.next_index(2), 0);
        Ok(())
    }

    #[test]
    fn empty_and_partial_packets_are_rejected() -> Result<()> {
        let frame_bytes = NonZeroUsize::new(2).context("two is nonzero")?;

        assert!(VoiceClip::from_bytes(Vec::new(), frame_bytes).is_err());
        assert!(VoiceClip::from_bytes(vec![1, 2, 3], frame_bytes).is_err());
        Ok(())
    }
}
