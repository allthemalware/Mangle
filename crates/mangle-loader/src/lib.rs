//! Mangle sample loading. Build-order step 4.
//!
//! * **Decode** — symphonia: wav, aiff, flac, ogg/vorbis, mp3, adpcm.
//! * **Resample** — in-house arbitrary-ratio windowed sinc (12 zero
//!   crossings, Blackman window, per-phase weight normalization). When
//!   downsampling the kernel stretches by the ratio, so it is a proper
//!   decimation lowpass — no aliasing of content above the new Nyquist.
//!   Offline quality, simple, dependency-free; worst case (300 s @ 96 kHz
//!   → 48 kHz) lands in single-digit seconds in release builds.
//! * **Hash** — BLAKE3 over the file bytes, truncated to the leading 8
//!   bytes to fit the state format's u64 (used for missing-file/relink
//!   verification, not security).
//!
//! Constraints from the design doc (§7): max 300 s per slot (longer files
//! are truncated with a warning), mono duplicates to both channels, more
//! than two channels keeps the first two.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use mangle_dsp::sample::{SampleBank, SampleSlot};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const MAX_SECONDS: f64 = 300.0;

// ------------------------------------------------------------- errors ---

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    Decode(String),
    NoAudioTrack,
    Empty,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "io error: {e}"),
            LoadError::Decode(e) => write!(f, "decode error: {e}"),
            LoadError::NoAudioTrack => write!(f, "no audio track"),
            LoadError::Empty => write!(f, "file decoded to zero samples"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<std::io::Error> for LoadError {
    fn from(e: std::io::Error) -> Self {
        LoadError::Io(e)
    }
}

// ----------------------------------------------------------- resample ---

/// Arbitrary-ratio windowed-sinc resampler for one channel.
///
/// Kernel: `sinc(cutoff · x) · blackman(x / half)` with the kernel
/// stretched by the ratio when decimating (so the lowpass lands below the
/// destination Nyquist), and each output sample normalized by its weight
/// sum for exact unity DC gain at every fractional phase.
pub fn resample_channel(src: &[f32], src_rate: f64, dst_rate: f64) -> Vec<f32> {
    if src.is_empty() {
        return Vec::new();
    }
    if (src_rate - dst_rate).abs() < 1e-9 {
        return src.to_vec();
    }
    let ratio = src_rate / dst_rate; // source samples per output sample
    let scale = ratio.max(1.0); // kernel stretch when decimating
    const ZEROS: f64 = 12.0;
    let half = (ZEROS * scale).ceil() as isize;
    let cutoff = 0.92 / scale;
    let out_len = ((src.len() as f64) / ratio).floor() as usize;
    let last = src.len() as isize - 1;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let t = i as f64 * ratio;
        let it = t.floor() as isize;
        let frac = t - it as f64;
        let mut acc = 0.0f64;
        let mut wsum = 0.0f64;
        let mut k = -half;
        while k <= half {
            let x = k as f64 - frac;
            let y = std::f64::consts::PI * x * cutoff;
            let sinc = if y.abs() < 1e-12 { 1.0 } else { y.sin() / y };
            let u = x / half as f64;
            let w = if u.abs() <= 1.0 {
                0.42 + 0.5 * (std::f64::consts::PI * u).cos()
                    + 0.08 * (2.0 * std::f64::consts::PI * u).cos()
            } else {
                0.0
            };
            let h = sinc * w;
            let idx = (it + k).clamp(0, last) as usize;
            acc += src[idx] as f64 * h;
            wsum += h;
            k += 1;
        }
        out.push(if wsum.abs() > 1e-12 { (acc / wsum) as f32 } else { 0.0 });
    }
    out
}

// --------------------------------------------------------------- hash ---

/// BLAKE3 of the file bytes, truncated to the leading 8 bytes (LE) to fit
/// the state format's u64.
pub fn hash_file(path: &Path) -> std::io::Result<u64> {
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hash = hasher.finalize();
    Ok(u64::from_le_bytes(hash.as_bytes()[..8].try_into().unwrap()))
}

// --------------------------------------------------------------- load ---

pub struct LoadedSample {
    pub slot: SampleSlot,
    pub content_hash: u64,
    /// Source rate before resampling, for display.
    pub src_rate: f32,
    pub warnings: Vec<String>,
}

/// Load, decode, truncate (300 s cap), resample to `target_rate`, hash.
pub fn load_sample(path: &Path, target_rate: f32) -> Result<LoadedSample, LoadError> {
    load_sample_with_cap(path, target_rate, MAX_SECONDS)
}

fn load_sample_with_cap(
    path: &Path,
    target_rate: f32,
    max_seconds: f64,
) -> Result<LoadedSample, LoadError> {
    let mut warnings = Vec::new();

    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| LoadError::Decode(e.to_string()))?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or(LoadError::NoAudioTrack)?;
    let track_id = track.id;
    let src_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| LoadError::Decode("missing sample rate".to_string()))? as f64;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| LoadError::Decode(e.to_string()))?;

    let max_frames = (src_rate * max_seconds) as usize;
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut n_ch = 0usize;
    let mut chans: Vec<Vec<f32>> = Vec::new();
    let mut truncated = false;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(LoadError::Decode(e.to_string())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(_)) => continue, // skip a corrupt packet
            Err(SymError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(e) => return Err(LoadError::Decode(e.to_string())),
        };
        if sample_buf.is_none() {
            let spec = *decoded.spec();
            n_ch = spec.channels.count().max(1);
            chans = vec![Vec::new(); n_ch];
            sample_buf = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
        }
        let buf = sample_buf.as_mut().unwrap();
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks_exact(n_ch) {
            for (c, v) in frame.iter().enumerate() {
                chans[c].push(*v);
            }
        }
        if chans[0].len() >= max_frames {
            for c in chans.iter_mut() {
                c.truncate(max_frames);
            }
            truncated = true;
            break;
        }
    }

    if chans.is_empty() || chans[0].is_empty() {
        return Err(LoadError::Empty);
    }
    if truncated {
        warnings.push(format!("truncated to {max_seconds:.0} s (slot cap)"));
    }
    if n_ch > 2 {
        warnings.push(format!("{n_ch} channels in file, using the first two"));
    }

    let l_src = std::mem::take(&mut chans[0]);
    let r_src = if n_ch >= 2 { std::mem::take(&mut chans[1]) } else { l_src.clone() };

    let l = resample_channel(&l_src, src_rate, target_rate as f64);
    let r = resample_channel(&r_src, src_rate, target_rate as f64);
    if l.is_empty() {
        return Err(LoadError::Empty);
    }
    let len = l.len();

    let content_hash = hash_file(path)?;

    Ok(LoadedSample {
        slot: SampleSlot {
            data: [l, r],
            sample_rate: target_rate,
            root_note: 60,
            loop_start: 0,
            loop_end: len,
            gain: 1.0,
            mono: n_ch == 1, // single-channel source duplicated to both sides
        },
        content_hash,
        src_rate: src_rate as f32,
        warnings,
    })
}

// ---------------------------------------------------------- multi-drop ---

pub struct FillResult {
    pub slot_index: usize,
    pub path: PathBuf,
    pub result: Result<u64, LoadError>,
}

/// Torsion multi-drop: fill slots from `start_slot` onward, one file per
/// slot. Failed files don't consume a slot.
pub fn fill_from(
    bank: &mut SampleBank,
    paths: &[PathBuf],
    start_slot: usize,
    target_rate: f32,
) -> Vec<FillResult> {
    let mut out = Vec::new();
    let mut slot = start_slot.min(5);
    for p in paths {
        if slot > 5 {
            break;
        }
        match load_sample(p, target_rate) {
            Ok(loaded) => {
                let hash = loaded.content_hash;
                bank.slots[slot] = Some(loaded.slot);
                out.push(FillResult { slot_index: slot, path: p.clone(), result: Ok(hash) });
                slot += 1;
            }
            Err(e) => {
                out.push(FillResult { slot_index: slot, path: p.clone(), result: Err(e) });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sine(freq: f64, rate: f64, secs: f64, amp: f32) -> Vec<f32> {
        let n = (rate * secs) as usize;
        (0..n)
            .map(|i| amp * (std::f64::consts::TAU * freq * i as f64 / rate).sin() as f32)
            .collect()
    }

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|x| (x * x) as f64).sum::<f64>() / v.len().max(1) as f64).sqrt() as f32
    }

    /// Minimal IEEE-float WAV writer (with the fact chunk float WAVs want).
    fn write_wav_f32(path: &Path, rate: u32, chans: &[Vec<f32>]) {
        let n = chans[0].len() as u32;
        let nch = chans.len() as u16;
        let data_len = n * nch as u32 * 4;
        let riff_len = 4 + (8 + 16) + (8 + 4) + (8 + data_len);
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&riff_len.to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&3u16.to_le_bytes()).unwrap(); // IEEE float
        f.write_all(&nch.to_le_bytes()).unwrap();
        f.write_all(&rate.to_le_bytes()).unwrap();
        f.write_all(&(rate * nch as u32 * 4).to_le_bytes()).unwrap();
        f.write_all(&(nch * 4).to_le_bytes()).unwrap();
        f.write_all(&32u16.to_le_bytes()).unwrap();
        f.write_all(b"fact").unwrap();
        f.write_all(&4u32.to_le_bytes()).unwrap();
        f.write_all(&n.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_len.to_le_bytes()).unwrap();
        for i in 0..n as usize {
            for c in chans {
                f.write_all(&c[i].to_le_bytes()).unwrap();
            }
        }
    }

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mangle_loader_{name}"))
    }

    // -------------------------------------------------- resampler unit ---

    #[test]
    fn resample_identity_when_rates_match() {
        let src = sine(440.0, 48_000.0, 0.1, 0.5);
        let out = resample_channel(&src, 48_000.0, 48_000.0);
        assert_eq!(out, src);
    }

    #[test]
    fn resample_upsample_snr() {
        let src = sine(1_000.0, 44_100.0, 1.0, 0.5);
        let out = resample_channel(&src, 44_100.0, 48_000.0);
        let expect = (src.len() as f64 * 48_000.0 / 44_100.0) as usize;
        assert!((out.len() as i64 - expect as i64).abs() <= 2, "len {}", out.len());
        let mut sig = 0.0f64;
        let mut err = 0.0f64;
        for i in 200..out.len() - 200 {
            let truth =
                0.5 * (std::f64::consts::TAU * 1_000.0 * i as f64 / 48_000.0).sin();
            sig += truth * truth;
            err += (out[i] as f64 - truth) * (out[i] as f64 - truth);
        }
        let snr = 10.0 * (sig / err.max(1e-30)).log10();
        assert!(snr > 55.0, "upsample SNR only {snr:.1} dB");
    }

    #[test]
    fn resample_downsample_filters_aliases() {
        // 30 kHz at 96 k is above the 24 kHz destination Nyquist: it must
        // be filtered out, not folded down.
        let hot = sine(30_000.0, 96_000.0, 0.5, 0.5);
        let out = resample_channel(&hot, 96_000.0, 48_000.0);
        let r = rms(&out[200..out.len() - 200]);
        assert!(r < 0.01, "alias leaked through: rms {r}");
        // …while in-band content passes intact.
        let ok = sine(1_000.0, 96_000.0, 0.5, 0.5);
        let out = resample_channel(&ok, 96_000.0, 48_000.0);
        let r = rms(&out[200..out.len() - 200]);
        let target = 0.5 / 2.0f32.sqrt();
        assert!((r - target).abs() < target * 0.05, "passband rms {r} vs {target}");
    }

    // ------------------------------------------------------- file path ---

    #[test]
    fn loads_stereo_wav_same_rate() {
        let path = tmp("stereo.wav");
        let l = sine(440.0, 48_000.0, 0.25, 0.5);
        let r = sine(660.0, 48_000.0, 0.25, 0.5);
        write_wav_f32(&path, 48_000, &[l.clone(), r.clone()]);
        let loaded = load_sample(&path, 48_000.0).expect("load");
        assert_eq!(loaded.slot.len(), l.len());
        assert_eq!(loaded.src_rate, 48_000.0);
        assert!(loaded.warnings.is_empty());
        // Float WAV decodes bit-exactly.
        assert!((loaded.slot.data[0][1000] - l[1000]).abs() < 1e-6);
        assert!((loaded.slot.data[1][1000] - r[1000]).abs() < 1e-6);
        assert_ne!(loaded.slot.data[0][1000], loaded.slot.data[1][1000]);
        assert_eq!(loaded.slot.loop_end, l.len());
        assert_eq!(loaded.slot.gain, 1.0);
    }

    #[test]
    fn mono_duplicates_to_both_channels() {
        let path = tmp("mono.wav");
        let m = sine(330.0, 48_000.0, 0.1, 0.4);
        write_wav_f32(&path, 48_000, &[m]);
        let loaded = load_sample(&path, 48_000.0).expect("load");
        assert_eq!(loaded.slot.data[0], loaded.slot.data[1]);
    }

    #[test]
    fn resamples_on_load() {
        let path = tmp("sr441.wav");
        let m = sine(1_000.0, 44_100.0, 0.5, 0.5);
        write_wav_f32(&path, 44_100, &[m]);
        let loaded = load_sample(&path, 48_000.0).expect("load");
        let expect = (0.5 * 48_000.0) as usize;
        assert!((loaded.slot.len() as i64 - expect as i64).abs() <= 2);
        assert_eq!(loaded.slot.sample_rate, 48_000.0);
    }

    #[test]
    fn cap_truncates_with_warning() {
        let path = tmp("long.wav");
        let m = sine(220.0, 48_000.0, 1.0, 0.3);
        write_wav_f32(&path, 48_000, &[m]);
        let loaded = load_sample_with_cap(&path, 48_000.0, 0.5).expect("load");
        assert_eq!(loaded.slot.len(), 24_000);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("truncated"));
    }

    #[test]
    fn missing_and_garbage_files_error() {
        match load_sample(Path::new("/nonexistent/mangle.wav"), 48_000.0) {
            Err(LoadError::Io(_)) => {}
            Err(e) => panic!("expected Io error, got {e:?}"),
            Ok(_) => panic!("expected Io error, got Ok"),
        }
        let path = tmp("garbage.bin");
        std::fs::write(&path, [0x42u8; 4096]).unwrap();
        match load_sample(&path, 48_000.0) {
            Err(LoadError::Decode(_)) => {}
            Err(e) => panic!("expected Decode error, got {e:?}"),
            Ok(_) => panic!("expected Decode error, got Ok"),
        }
    }

    #[test]
    fn hash_is_stable_and_content_sensitive() {
        let a = tmp("hash_a.wav");
        let b = tmp("hash_b.wav");
        write_wav_f32(&a, 48_000, &[sine(440.0, 48_000.0, 0.05, 0.5)]);
        write_wav_f32(&b, 48_000, &[sine(441.0, 48_000.0, 0.05, 0.5)]);
        assert_eq!(hash_file(&a).unwrap(), hash_file(&a).unwrap());
        assert_ne!(hash_file(&a).unwrap(), hash_file(&b).unwrap());
    }

    #[test]
    fn fill_from_skips_failures_without_burning_slots() {
        let good1 = tmp("fill1.wav");
        let good2 = tmp("fill2.wav");
        write_wav_f32(&good1, 48_000, &[sine(220.0, 48_000.0, 0.05, 0.5)]);
        write_wav_f32(&good2, 48_000, &[sine(330.0, 48_000.0, 0.05, 0.5)]);
        let mut bank = SampleBank::new();
        let paths = vec![good1, PathBuf::from("/nonexistent.wav"), good2];
        let results = fill_from(&mut bank, &paths, 4, 48_000.0);
        assert_eq!(results.len(), 3);
        assert!(results[0].result.is_ok());
        assert!(results[1].result.is_err());
        assert!(results[2].result.is_ok());
        assert!(bank.slots[4].is_some());
        assert!(bank.slots[5].is_some()); // failure didn't consume slot 5
        assert_eq!(bank.loaded().1, 2);
    }
}
