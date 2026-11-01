//! Sum voices in f32, then apply volume and saturate once at the PCM16 boundary.
//! Mixing directly into i16 lets simultaneous loud voices wrap their polarity.
use std::collections::VecDeque;

// Stereo-linked, 5 ms lookahead peak limiter. The gain ramp is computed from
// future peaks, rather than flattening the waveform at the PCM16 boundary.
// A 100 ms release avoids gain jumps between retro_run blocks. Only the core's
// output is changed; Flash sound positions and SoundMixer data stay untouched.
pub struct Output {
    pending: VecDeque<[f32; 2]>,
    minima: VecDeque<(u64, f64)>,
    index: u64,
    delay: usize,
    gain: f64,
    release: f64,
    peak: f32,
    over: u64,
    limited: u64,
    min_gain: f64,
    blocks: u32,
    mix_ms: f64,
    peak_mix_ms: f64,
}
impl Default for Output {
    fn default() -> Self {
        Self::new(48000)
    }
}
impl Output {
    pub fn new(rate: u32) -> Self {
        let delay = (rate as usize / 200).max(1);
        Self {
            pending: VecDeque::with_capacity(delay + 1),
            minima: VecDeque::with_capacity(delay + 1),
            index: 0,
            delay,
            gain: 1.,
            release: 1. / (rate as f64 * 0.1),
            peak: 0.,
            over: 0,
            limited: 0,
            min_gain: 1.,
            blocks: 0,
            mix_ms: 0.,
            peak_mix_ms: 0.,
        }
    }
    pub fn convert(&mut self, samples: &[f32], volume: f32) -> Vec<i16> {
        let mut output = Vec::with_capacity(samples.len());
        let attack = 1. / self.delay as f64;
        for frame in samples.as_chunks::<2>().0 {
            let clean = |x: f32| {
                let v = x * volume;
                if v.is_finite() { v } else { 0. }
            };
            let frame = [clean(frame[0]), clean(frame[1])];
            let peak = frame[0].abs().max(frame[1].abs());
            self.peak = self.peak.max(peak);
            self.over += frame.iter().filter(|x| x.abs() > 1.).count() as u64;
            // 0.98 leaves room for frontend resampling. Quiet signals are unity
            // gain; attenuation is applied only when a peak crosses this ceiling.
            let required = if peak > 0.98 { 0.98 / peak as f64 } else { 1. };
            let bound = required + attack * self.index as f64;
            while self.minima.back().is_some_and(|(_, v)| *v >= bound) {
                self.minima.pop_back();
            }
            self.minima.push_back((self.index, bound));
            self.pending.push_back(frame);
            if self.pending.len() > self.delay {
                let at = self.index - self.delay as u64;
                while self.minima.front().is_some_and(|(i, _)| *i < at) {
                    self.minima.pop_front();
                }
                let desired = (self.minima.front().unwrap().1 - attack * at as f64).clamp(0., 1.);
                self.gain = desired.min(self.gain + self.release);
                self.min_gain = self.min_gain.min(self.gain);
                self.limited += u64::from(self.gain < 0.9999);
                let frame = self.pending.pop_front().unwrap();
                output.extend(frame.map(|x| quantize((x as f64 * self.gain) as f32)));
            } else {
                output.extend([0, 0]);
            }
            self.index += 1;
        }
        output
    }
    pub fn report(&mut self, mix_ms: f64) -> Option<String> {
        self.blocks += 1;
        self.mix_ms += mix_ms;
        self.peak_mix_ms = self.peak_mix_ms.max(mix_ms);
        if self.blocks < 300 {
            return None;
        }
        let message = format!(
            "audio 300 frames: mixed_peak={:.3} over_range_samples={} limited_stereo_frames={} minimum_gain={:.3} mix_convert={:.2}ms peak_mix_convert={:.2}ms",
            self.peak,
            self.over,
            self.limited,
            self.min_gain,
            self.mix_ms / self.blocks as f64,
            self.peak_mix_ms
        );
        self.blocks = 0;
        self.peak = 0.;
        self.over = 0;
        self.limited = 0;
        self.min_gain = 1.;
        self.mix_ms = 0.;
        self.peak_mix_ms = 0.;
        Some(message)
    }
}
fn quantize(sample: f32) -> i16 {
    let sample = if sample.is_finite() {
        sample.clamp(-1., 1.)
    } else {
        0.
    };
    (sample * if sample < 0. { 32768. } else { 32767. }).round() as i16
}

#[cfg(test)]
pub fn pcm16(samples: &[f32], volume: f32) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| {
            let sample = sample * volume;
            if !sample.is_finite() {
                return 0;
            }
            let sample = sample.clamp(-1., 1.);
            (sample * if sample < 0. { 32768. } else { 32767. }).round() as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruffle_core::backend::audio::AudioMixer;
    fn voices(amplitude: i16) -> Vec<f32> {
        voices_count(amplitude, 2)
    }
    fn voices_count(amplitude: i16, count: usize) -> Vec<f32> {
        let mut mixer = AudioMixer::new(2, 44100);
        let data = amplitude.to_le_bytes().repeat(400);
        let sound = swf::Sound {
            id: 1,
            format: swf::SoundFormat {
                compression: swf::AudioCompression::Uncompressed,
                sample_rate: 44100,
                is_stereo: true,
                is_16_bit: true,
            },
            num_samples: 200,
            data: &data,
        };
        let handle = mixer.register_sound(&sound).unwrap();
        let settings = swf::SoundInfo {
            event: swf::SoundEvent::Event,
            in_sample: None,
            out_sample: None,
            num_loops: 1,
            envelope: None,
        };
        for _ in 0..count {
            mixer.start_sound(handle, &settings).unwrap();
        }
        let mut output = vec![0.; 40];
        mixer.mix(&mut output);
        output
    }
    #[test]
    fn loud_positive_voices_do_not_wrap_negative() {
        let output = voices(20000);
        assert!(output[4..].iter().all(|v| *v > 1.));
        assert!(pcm16(&output, 1.)[4..].iter().all(|v| *v == i16::MAX));
    }
    #[test]
    fn loud_negative_voices_do_not_wrap_positive() {
        let output = voices(-20000);
        assert!(output[4..].iter().all(|v| *v < -1.));
        assert!(pcm16(&output, 1.)[4..].iter().all(|v| *v == i16::MIN));
    }
    #[test]
    fn volume_is_applied_before_saturation() {
        let output = voices(20000);
        assert!(
            pcm16(&output, 0.5)[4..]
                .iter()
                .all(|v| (*v as i32 - 20000).abs() <= 1)
        );
    }
    #[test]
    fn nonfinite_samples_are_silent() {
        assert_eq!(
            pcm16(&[f32::NAN, f32::INFINITY, f32::NEG_INFINITY], 1.),
            [0, 0, 0]
        );
    }
    #[test]
    fn lookahead_keeps_quiet_audio_exact_after_fixed_delay() {
        let input: Vec<f32> = (0..1200).map(|i| ((i as f32) * 0.03).sin() * 0.4).collect();
        let mut limiter = Output::default();
        let mut padded = input.clone();
        padded.extend(vec![0.; 480]);
        let out = limiter.convert(&padded, 1.);
        assert_eq!(&out[..480], &vec![0; 480]);
        assert_eq!(&out[480..], pcm16(&input, 1.));
    }
    #[test]
    fn sixty_four_overlapping_voices_stay_below_clipping_and_preserve_stereo() {
        let mut limiter = Output::default();
        let mut input = Vec::new();
        for i in 0..4800 {
            let v = (i as f32 * 0.07).sin() * 64.;
            input.extend([v, v * 0.25]);
        }
        input.extend(vec![0.; 480]);
        let out = limiter.convert(&input, 1.);
        assert!(out.iter().all(|v| (*v as i32).abs() <= 32113));
        for pair in out.as_chunks::<2>().0 {
            assert!((pair[0] as i32 - 4 * pair[1] as i32).abs() <= 4);
        }
        assert!(limiter.min_gain < 0.02);
    }
    #[test]
    fn limiter_is_continuous_across_audio_block_boundaries() {
        let input: Vec<f32> = (0..16000)
            .map(|i| (i as f32 * 0.017).sin() * if i > 4000 && i < 11000 { 3. } else { 0.2 })
            .collect();
        let full = Output::default().convert(&input, 1.);
        let mut limiter = Output::default();
        let mut blocks = Vec::new();
        for block in input.chunks(1600) {
            blocks.extend(limiter.convert(block, 1.));
        }
        assert_eq!(full, blocks);
    }
    #[test]
    fn volume_is_applied_before_limiter_and_invalid_audio_is_silent() {
        let mut limiter = Output::default();
        let mut input = vec![2.; 1600];
        input.extend(vec![0.; 480]);
        let out = limiter.convert(&input, 0.25);
        assert!(out[480..2080].iter().all(|x| *x == 16384));
        assert!((limiter.min_gain - 1.).abs() < 1e-9);
        let mut limiter = Output::default();
        let out = limiter.convert(&vec![f32::NAN; 1600], 1.);
        assert!(out.iter().all(|x| *x == 0));
    }
    #[test]
    fn actual_mixer_with_sixty_four_voices_does_not_clip_at_the_pcm16_boundary() {
        for amplitude in [-20000, 20000] {
            let mut mixed = voices_count(amplitude, 64);
            assert!(mixed[4..].iter().all(|x| x.abs() > 30.));
            let count = mixed.len();
            mixed.extend(vec![0.; 480]);
            let out = Output::default().convert(&mixed, 1.);
            for sample in &out[484..480 + count] {
                assert!((*sample as i32).abs() < i16::MAX as i32);
                assert_eq!(sample.signum(), amplitude.signum());
            }
        }
    }
}
