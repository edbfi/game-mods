// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two short tones the trainer plays when a cheat turns on or off.
//!
//! Each tone is a complete WAVE file built in memory: a sine with a sin² envelope, so it starts and ends at
//! silence and never clicks. On Windows, `play` hands it to the system player.
#![forbid(unsafe_code)]

/// Samples per second.
const RATE: u32 = 22_050;
/// Samples in one tone: a sixth of a second.
const SAMPLES: usize = 3_675;
/// The loudest sample.
const AMPLITUDE: f64 = 2_300.0;
/// Bytes in the WAVE header before the samples.
const HEADER: usize = 44;

/// Which tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    /// 660 Hz: something turned on.
    Rise,
    /// 440 Hz: something turned off.
    Fall,
}

impl Tone {
    /// The pitch in hertz.
    #[must_use]
    pub const fn frequency(self) -> u32 {
        match self {
            Self::Rise => 660,
            Self::Fall => 440,
        }
    }
}

/// A complete RIFF/WAVE file of `tone`: 22,050 Hz, mono, 16-bit PCM, a sixth of a second (3,675 samples), a
/// sine at the tone's frequency with amplitude 2,300 and a sin² envelope over the whole sound.
#[must_use]
pub fn wav(tone: Tone) -> Vec<u8> {
    let data = u32::try_from(SAMPLES * 2).unwrap_or(u32::MAX);
    let mut bytes = Vec::with_capacity(HEADER + SAMPLES * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 2).to_le_bytes()); // bytes per second
    bytes.extend_from_slice(&2u16.to_le_bytes()); // bytes per sample
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    let step = std::f64::consts::TAU * f64::from(tone.frequency()) / f64::from(RATE);
    let last = (SAMPLES - 1) as f64;
    for n in 0..SAMPLES {
        let at = n as f64;
        let envelope = (std::f64::consts::PI * at / last).sin().powi(2);
        let sample = (AMPLITUDE * envelope * (step * at).sin()).round();
        // A rounded sample within ±2,300 converts exactly.
        let sample = sample as i16;
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

/// Plays `tone` without waiting, cutting off the one playing. Errors are ignored: a missing sound must not
/// stop the trainer.
#[cfg(windows)]
pub fn play(tone: Tone) {
    use std::sync::OnceLock;
    // The player reads the buffer while the sound plays, after this returns, so each lives for the process.
    static RISE: OnceLock<Vec<u8>> = OnceLock::new();
    static FALL: OnceLock<Vec<u8>> = OnceLock::new();
    let cell = match tone {
        Tone::Rise => &RISE,
        Tone::Fall => &FALL,
    };
    crate::win32::sound::play(cell.get_or_init(|| wav(tone)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(wav: &[u8]) -> Vec<i16> {
        wav[HEADER..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| i16::from_le_bytes(pair))
            .collect()
    }

    fn u32_at(wav: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(wav[at..at + 4].try_into().unwrap())
    }

    fn u16_at(wav: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(wav[at..at + 2].try_into().unwrap())
    }

    #[test]
    fn the_header_describes_mono_16_bit_pcm_at_22050_hz() {
        for tone in [Tone::Rise, Tone::Fall] {
            let wav = wav(tone);
            assert_eq!(wav.len(), HEADER + 3_675 * 2);
            assert_eq!(&wav[0..4], b"RIFF");
            assert_eq!(u32_at(&wav, 4) as usize, wav.len() - 8);
            assert_eq!(&wav[8..16], b"WAVEfmt ");
            assert_eq!(u32_at(&wav, 16), 16);
            assert_eq!(u16_at(&wav, 20), 1, "PCM");
            assert_eq!(u16_at(&wav, 22), 1, "mono");
            assert_eq!(u32_at(&wav, 24), 22_050);
            assert_eq!(u32_at(&wav, 28), 44_100);
            assert_eq!(u16_at(&wav, 32), 2);
            assert_eq!(u16_at(&wav, 34), 16);
            assert_eq!(&wav[36..40], b"data");
            assert_eq!(u32_at(&wav, 40), 3_675 * 2);
        }
    }

    #[test]
    fn a_tone_starts_and_ends_silent_and_stays_within_its_amplitude() {
        for tone in [Tone::Rise, Tone::Fall] {
            let samples = samples(&wav(tone));
            assert_eq!(samples.len(), 3_675);
            assert_eq!(samples[0], 0);
            assert_eq!(samples[samples.len() - 1], 0);
            let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!(peak <= 2_300, "{peak}");
            assert!(peak > 2_000, "the middle is near full amplitude: {peak}");
        }
    }

    #[test]
    fn each_tone_has_its_pitch() {
        for (tone, hertz) in [(Tone::Rise, 660.0), (Tone::Fall, 440.0)] {
            let signs: Vec<bool> = samples(&wav(tone))
                .into_iter()
                .filter(|&s| s != 0)
                .map(|s| s > 0)
                .collect();
            let crossings = signs.windows(2).filter(|pair| pair[0] != pair[1]).count();
            // Two crossings per cycle, over a sixth of a second.
            let expected: f64 = 2.0 * hertz / 6.0;
            assert!(
                (crossings as f64 - expected).abs() <= 3.0,
                "{tone:?}: {crossings} crossings, expected about {expected}"
            );
        }
    }
}
