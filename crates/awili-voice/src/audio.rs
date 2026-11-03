//! Pure audio helpers shared by the (mac-only) capture path: channel downmix and
//! a *streaming* linear resampler to whisper's 16 kHz. Kept out of `capture.rs`
//! so the rate math is unit-tested on this Linux host even though cpal capture is
//! not built here.
//!
//! The resampler is stateful on purpose (review slice 3, SF2): CoreAudio delivers
//! variable-sized callbacks, and resampling each callback independently rounds
//! the output length and clamps interpolation at every chunk edge — losing
//! fractional phase and repeating/flattening boundary samples. [`StreamResampler`]
//! carries the fractional source position and the previous frame across calls, so
//! a stream fed in arbitrary chunk sizes produces the same samples as one fed
//! whole.

/// Downmix `channels`-interleaved samples to mono by averaging each frame.
pub fn downmix_to_mono(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    data.chunks_exact(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32)
        .collect()
}

/// Downmix `channels`-interleaved samples to mono into a caller-owned buffer,
/// converting each raw sample with `conv`. Appends to `out` without allocating
/// once `out` has warmed up its capacity — safe for the real-time audio callback
/// (review SF2: no fresh `Vec` per callback).
pub fn downmix_convert_into<T: Copy>(
    data: &[T],
    channels: usize,
    conv: impl Fn(T) -> f32,
    out: &mut Vec<f32>,
) {
    if channels == 0 {
        return;
    }
    if channels == 1 {
        out.extend(data.iter().map(|&s| conv(s)));
        return;
    }
    out.extend(
        data.chunks_exact(channels)
            .map(|frame| frame.iter().map(|&s| conv(s)).sum::<f32>() / channels as f32),
    );
}

/// A streaming linear-interpolation resampler that keeps fractional phase and the
/// previous input sample across [`process`](Self::process) calls, so feeding the
/// stream in arbitrary chunks yields one continuous output (review SF2).
///
/// Output sample `n` is taken at source position `n * in_rate / out_rate`,
/// linearly interpolated between the two neighbouring input samples — including
/// across a chunk boundary, where the left neighbour is the last sample of the
/// previous chunk.
#[derive(Clone, Debug)]
pub struct StreamResampler {
    /// Source samples advanced per output sample (`in_rate / out_rate`).
    step: f64,
    /// Absolute source position (in input-sample units) of the next output.
    next_pos: f64,
    /// Absolute index of the next input sample to consume.
    idx: u64,
    /// The previous input sample (left interpolation neighbour at a chunk edge).
    prev: f32,
}

impl StreamResampler {
    /// A resampler from `in_rate` to `out_rate`. Rates must be non-zero.
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        assert!(
            in_rate > 0 && out_rate > 0,
            "resampler rates must be non-zero"
        );
        Self {
            step: in_rate as f64 / out_rate as f64,
            next_pos: 0.0,
            idx: 0,
            prev: 0.0,
        }
    }

    /// Consume `input`, appending resampled output to `out`. State (fractional
    /// position + previous sample) carries over to the next call. `out` is
    /// appended to, not cleared — the callback clears and reuses it.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &x in input {
            let i = self.idx as f64;
            // The segment (idx-1 .. idx] is now fully known: interpolate every
            // output position that falls in it. The very first sample (idx 0) has
            // no left neighbour; positions at/before it snap to the sample itself
            // (frac clamps to 1.0 with prev initialised on the fly).
            if self.idx == 0 {
                self.prev = x;
            }
            while self.next_pos <= i {
                let frac = (self.next_pos - (i - 1.0)).clamp(0.0, 1.0) as f32;
                out.push(self.prev + (x - self.prev) * frac);
                self.next_pos += self.step;
            }
            self.prev = x;
            self.idx += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resample a whole buffer in one call (test convenience).
    fn resample_whole(input: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
        let mut r = StreamResampler::new(in_rate, out_rate);
        let mut out = Vec::new();
        r.process(input, &mut out);
        out
    }

    #[test]
    fn downmix_averages_stereo_frames() {
        // L/R interleaved: (0,1),(2,3) -> 0.5, 2.5
        assert_eq!(downmix_to_mono(&[0.0, 1.0, 2.0, 3.0], 2), vec![0.5, 2.5]);
        // Mono passes through untouched.
        assert_eq!(downmix_to_mono(&[1.0, 2.0], 1), vec![1.0, 2.0]);
    }

    #[test]
    fn downmix_convert_into_matches_allocating_version() {
        let data = [0.0f32, 1.0, 2.0, 3.0];
        let mut out = Vec::new();
        downmix_convert_into(&data, 2, |s| s, &mut out);
        assert_eq!(out, downmix_to_mono(&data, 2));
        // i16-style conversion path.
        let raw = [i16::MAX, i16::MAX, 0, 0];
        let mut out = Vec::new();
        downmix_convert_into(&raw, 2, |s| s as f32 / i16::MAX as f32, &mut out);
        assert_eq!(out, vec![1.0, 0.0]);
        // Zero channels: a defensive no-op, never a panic in the audio callback.
        let mut out = Vec::new();
        downmix_convert_into(&data, 0, |s| s, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn downsample_halves_length_for_2x_rate() {
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let out = resample_whole(&input, 32_000, 16_000);
        assert_eq!(out.len(), 50);
        assert!((out[0] - 0.0).abs() < 1e-3);
        // 2:1 with linear phase lands exactly on every other input sample.
        assert!((out[10] - 20.0).abs() < 1e-3);
    }

    #[test]
    fn same_rate_is_identity() {
        let input = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_whole(&input, 16_000, 16_000), input);
    }

    #[test]
    fn empty_input_produces_no_output() {
        assert!(resample_whole(&[], 48_000, 16_000).is_empty());
    }

    #[test]
    fn chunked_processing_matches_whole_buffer_exactly() {
        // The SF2 regression: variable-size callbacks must produce the identical
        // sample stream as one big buffer — no per-chunk phase reset, no repeated
        // or flattened boundary samples, no cumulative length drift. 44.1 kHz is
        // the nasty case (non-integer ratio to 16 kHz).
        let input: Vec<f32> = (0..4410).map(|i| (i as f32 * 0.01).sin()).collect();
        let whole = resample_whole(&input, 44_100, 16_000);

        // Feed the same input in irregular chunks like CoreAudio would.
        let mut r = StreamResampler::new(44_100, 16_000);
        let mut chunked = Vec::new();
        let sizes = [1usize, 7, 480, 113, 512, 3, 941, 256];
        let mut off = 0;
        let mut si = 0;
        while off < input.len() {
            let n = sizes[si % sizes.len()].min(input.len() - off);
            r.process(&input[off..off + n], &mut chunked);
            off += n;
            si += 1;
        }
        assert_eq!(chunked, whole);
    }

    #[test]
    fn output_count_tracks_total_input_not_per_chunk_rounding() {
        // One second at 48 kHz fed in 480-sample callbacks must come out within a
        // sample of 16 000 — per-chunk `round()` used to accumulate count error.
        let mut r = StreamResampler::new(48_000, 16_000);
        let mut out = Vec::new();
        let chunk = vec![0.25f32; 480];
        for _ in 0..100 {
            r.process(&chunk, &mut out);
        }
        let n = out.len() as i64;
        assert!((n - 16_000).abs() <= 1, "got {n} samples for 1 s of audio");
    }

    #[test]
    fn interpolates_across_chunk_boundary() {
        // 2x upsample of a ramp split mid-way: the boundary output must be the
        // average of the last sample of chunk A and the first of chunk B, not a
        // repeat of either (the old per-callback clamp produced a flat sample).
        let mut r = StreamResampler::new(8_000, 16_000);
        let mut out = Vec::new();
        r.process(&[0.0, 1.0], &mut out);
        r.process(&[2.0, 3.0], &mut out);
        // positions: 0, .5, 1, 1.5, 2, 2.5, 3 -> 0, .5, 1, 1.5(across), 2, 2.5, 3
        assert_eq!(out, vec![0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0]);
    }
}
