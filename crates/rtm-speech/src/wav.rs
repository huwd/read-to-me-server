// SPDX-License-Identifier: AGPL-3.0-or-later
//! 16-bit mono PCM WAV in memory: how synthesised audio travels and is stored.

use std::io::Cursor;

/// Mono 16-bit PCM audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    pub sample_rate: u32,
    pub samples: Vec<i16>,
}

impl Pcm {
    pub fn duration_ms(&self) -> u64 {
        self.samples.len() as u64 * 1000 / u64::from(self.sample_rate.max(1))
    }

    /// `ms` of silence at `sample_rate`.
    pub fn silence(sample_rate: u32, ms: u32) -> Vec<i16> {
        vec![0; (u64::from(sample_rate) * u64::from(ms) / 1000) as usize]
    }

    pub fn to_wav(&self) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: self.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut out = Cursor::new(Vec::with_capacity(44 + self.samples.len() * 2));
        {
            let mut writer = hound::WavWriter::new(&mut out, spec).expect("valid spec");
            let mut writer16 = writer.get_i16_writer(self.samples.len() as u32);
            for s in &self.samples {
                writer16.write_sample(*s);
            }
            writer16.flush().expect("writing to memory");
            writer.finalize().expect("writing to memory");
        }
        out.into_inner()
    }

    /// Read a WAV that must be 16-bit mono PCM.
    pub fn from_wav(bytes: &[u8]) -> Result<Pcm, WavError> {
        let reader = hound::WavReader::new(Cursor::new(bytes)).map_err(|_| WavError::Unreadable)?;
        let spec = reader.spec();
        if spec.channels != 1
            || spec.bits_per_sample != 16
            || spec.sample_format != hound::SampleFormat::Int
        {
            return Err(WavError::WrongFormat);
        }
        let samples = reader
            .into_samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| WavError::Unreadable)?;
        Ok(Pcm {
            sample_rate: spec.sample_rate,
            samples,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WavError {
    #[error("not a readable WAV file")]
    Unreadable,
    #[error("not 16-bit mono PCM")]
    WrongFormat,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_duration() {
        let pcm = Pcm {
            sample_rate: 24_000,
            samples: (0..24_000).map(|i| (i % 100) as i16).collect(),
        };
        assert_eq!(pcm.duration_ms(), 1000);
        assert_eq!(Pcm::from_wav(&pcm.to_wav()).unwrap(), pcm);
        assert_eq!(Pcm::silence(24_000, 150).len(), 3_600);
    }

    #[test]
    fn other_formats_and_junk_are_refused() {
        assert_eq!(Pcm::from_wav(b"RIFF nonsense"), Err(WavError::Unreadable));
        let stereo = {
            let spec = hound::WavSpec {
                channels: 2,
                sample_rate: 24_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut out = Cursor::new(Vec::new());
            let mut w = hound::WavWriter::new(&mut out, spec).unwrap();
            w.write_sample(0i16).unwrap();
            w.write_sample(0i16).unwrap();
            w.finalize().unwrap();
            out.into_inner()
        };
        assert_eq!(Pcm::from_wav(&stereo), Err(WavError::WrongFormat));
    }
}
