//! Deterministic, offline synthesis of the generated factory sources.

use std::f64::consts::{PI, TAU};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::Path;

const RATE: u32 = 44_100;
const PEAK: f64 = 0.89;
const SEED: u64 = 0x4d61_6e67_6c65;

struct Recipe {
    name: &'static str,
    category: &'static str,
    root: i32,
    seconds: f64,
    parameters: &'static str,
}

const RECIPES: [Recipe; 16] = [
    Recipe { name: "Nylon Pluck", category: "plucked", root: 57, seconds: 3.0,
        parameters: "Karplus-Strong; damping=0.5; t60=2.5s" },
    Recipe { name: "Steel Pluck", category: "plucked", root: 52, seconds: 3.5,
        parameters: "Karplus-Strong; damping=0.12; t60=3s" },
    Recipe { name: "Glass Bar", category: "bars", root: 72, seconds: 4.0,
        parameters: "modal; ratios=1,2.76,5.40,8.93; levels=1,0.45,0.25,0.15; tau=1.2,0.35,0.2,0.12s" },
    Recipe { name: "Wood Bar", category: "bars", root: 64, seconds: 2.0,
        parameters: "modal; ratios=1,3.9,9.2; levels=1,0.4,0.2; tau=0.45,0.18,0.09s" },
    Recipe { name: "Tubular Bell", category: "bells", root: 60, seconds: 6.0,
        parameters: "modal; ratios=0.5,1,1.19,1.56,2,2.51,2.66,3.01; levels=0.3,1,0.3,0.25,0.2,0.15,0.12,0.1; tau=3,3,2.7,2.5,2.3,2.1,1.9,1.7s" },
    Recipe { name: "Tine Bell", category: "bells", root: 79, seconds: 4.0,
        parameters: "modal; ratios=1,2.32,4.25,6.63; levels=1,0.4,0.2,0.1; tau=1.5,0.6,0.3,0.15s" },
    Recipe { name: "Organ Drawbar", category: "keys", root: 48, seconds: 4.0,
        parameters: "additive; harmonics=1,2,3,4,6,8; levels=1,0.8,0.6,0.4,0.25,0.2; chorus=+-2cents; chorus_level=0.15" },
    Recipe { name: "Soft Pad Saw", category: "pads", root: 55, seconds: 5.0,
        parameters: "7 band-limited saws; detune=-6,-4,-2,0,2,4,6cents; lowpass=1200Hz; attack=1s" },
    Recipe { name: "Formant Ah", category: "voice", root: 55, seconds: 4.0,
        parameters: "glottal harmonic source 1/n^1.5; bandpasses=800,1150,2900Hz; Q=6; levels=1,0.8,0.5; attack=0.08s" },
    Recipe { name: "Formant Oo", category: "voice", root: 55, seconds: 4.0,
        parameters: "glottal harmonic source 1/n^1.5; bandpasses=300,870,2240Hz; Q=6; levels=1,0.8,0.5; attack=0.08s" },
    Recipe { name: "Hollow Reed", category: "winds", root: 62, seconds: 4.0,
        parameters: "odd harmonics=1,3,5,7,9; levels=1/n; breath=0.025; attack=0.08s" },
    Recipe { name: "Brass Swell", category: "brass", root: 58, seconds: 4.0,
        parameters: "harmonics=1..24; levels=exp(-(n-1)/brightness)/n; brightness=1..12 over 1s; attack=0.25s" },
    Recipe { name: "FM Bell", category: "bells", root: 69, seconds: 5.0,
        parameters: "2-op FM; ratio=3.5; index=6*exp(-t/0.8)*(1-t/5); amplitude_tau=1.5s" },
    Recipe { name: "Metal Plate", category: "noise", root: -1, seconds: 5.0,
        parameters: "24 random modes; base=180Hz; ratios=uniform(1,12); levels=1/sqrt(ratio); tau=uniform(0.5,2.5)s; random_phase" },
    Recipe { name: "Breath Noise", category: "noise", root: -1, seconds: 4.0,
        parameters: "white noise; bandpass centre=1100+450*sin(2*pi*0.18*t)Hz; Q=1.2; attack=0.1s" },
    Recipe { name: "Sub Sine Drone", category: "drones", root: 36, seconds: 5.0,
        parameters: "harmonics=1,2,3; levels=1,0.12,0.05; tremolo=0.85+0.15*sin(2*pi*0.3*t); attack=0.2s" },
];

/// Writes the source WAVs and the headerless index fragment into `outdir`.
pub fn generate(outdir: &Path) -> io::Result<()> {
    fs::create_dir_all(outdir.join("sources"))?;
    let mut index = BufWriter::new(File::create(outdir.join("generated.tsv"))?);
    for (i, recipe) in RECIPES.iter().enumerate() {
        let id = i + 1;
        let samples = synthesize(i);
        write_wav(&outdir.join(format!("sources/{id:04}.wav")), &samples)?;
        writeln!(index,
            "{id:04}\t{}\t{}\tgen\tsources/{id:04}.wav\t{}\tgenerated\t{}; root={}; duration={}s; seed={}; sample_rate={RATE}; attack_floor=5ms; fade=30ms; DC=removed; peak={PEAK}",
            recipe.name, recipe.category, recipe.root, recipe.parameters,
            recipe.root, recipe.seconds, SEED + id as u64)?;
    }
    index.flush()
}

struct Random(u64);

impl Random {
    // Xorshift64*, with a fixed nonzero seed for each sound.
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn bipolar(&mut self) -> f64 {
        2.0 * self.unit() - 1.0
    }
}

fn frequency(root: i32) -> f64 {
    440.0 * 2.0f64.powf((root - 69) as f64 / 12.0)
}

fn attack(time: f64, seconds: f64) -> f64 {
    (time / seconds).min(1.0)
}

fn synthesize(i: usize) -> Vec<f64> {
    let recipe = &RECIPES[i];
    let mut samples = vec![0.0; (recipe.seconds * RATE as f64).round() as usize];
    let f = frequency(recipe.root);
    let mut random = Random(SEED + i as u64 + 1);
    match i {
        0 => pluck(&mut samples, f, 0.5, 2.5, &mut random),
        1 => pluck(&mut samples, f, 0.12, 3.0, &mut random),
        2 => modal(
            &mut samples,
            f,
            &[1.0, 2.76, 5.40, 8.93],
            &[1.0, 0.45, 0.25, 0.15],
            &[1.2, 0.35, 0.2, 0.12],
        ),
        3 => modal(
            &mut samples,
            f,
            &[1.0, 3.9, 9.2],
            &[1.0, 0.4, 0.2],
            &[0.45, 0.18, 0.09],
        ),
        4 => modal(
            &mut samples,
            f,
            &[0.5, 1.0, 1.19, 1.56, 2.0, 2.51, 2.66, 3.01],
            &[0.3, 1.0, 0.3, 0.25, 0.2, 0.15, 0.12, 0.1],
            &[3.0, 3.0, 2.7, 2.5, 2.3, 2.1, 1.9, 1.7],
        ),
        5 => modal(
            &mut samples,
            f,
            &[1.0, 2.32, 4.25, 6.63],
            &[1.0, 0.4, 0.2, 0.1],
            &[1.5, 0.6, 0.3, 0.15],
        ),
        6 => {
            for (harmonic, level) in [(1, 1.0), (2, 0.8), (3, 0.6), (4, 0.4), (6, 0.25), (8, 0.2)] {
                for cents in [-2.0, 0.0, 2.0] {
                    let hz = f * harmonic as f64 * 2.0f64.powf(cents / 1200.0);
                    let gain = level * if cents == 0.0 { 1.0 } else { 0.15 };
                    add_sine(&mut samples, hz, |_| gain);
                }
            }
        }
        7 => {
            for voice in -3..=3 {
                let hz = f * 2.0f64.powf(voice as f64 * 2.0 / 1200.0);
                for harmonic in 1..=((RATE as f64 * 0.5 / hz) as usize) {
                    add_sine(&mut samples, hz * harmonic as f64, |_| {
                        1.0 / harmonic as f64
                    });
                }
            }
            let coefficient = 1.0 - (-TAU * 1200.0 / RATE as f64).exp();
            let mut low = 0.0;
            for (n, sample) in samples.iter_mut().enumerate() {
                low += coefficient * (*sample - low);
                *sample = low * attack(n as f64 / RATE as f64, 1.0);
            }
        }
        8 | 9 => {
            for harmonic in 1..=((RATE as f64 * 0.5 / f) as usize) {
                add_sine(&mut samples, f * harmonic as f64, |_| {
                    1.0 / (harmonic as f64).powf(1.5)
                });
            }
            let centres = if i == 8 {
                [800.0, 1150.0, 2900.0]
            } else {
                [300.0, 870.0, 2240.0]
            };
            let mut filters = centres.map(|hz| Bandpass::new(hz, 6.0));
            for (n, sample) in samples.iter_mut().enumerate() {
                let input = *sample;
                *sample = filters
                    .iter_mut()
                    .zip([1.0, 0.8, 0.5])
                    .map(|(filter, level)| filter.process(input) * level)
                    .sum::<f64>()
                    * attack(n as f64 / RATE as f64, 0.08);
            }
        }
        10 => {
            for harmonic in [1, 3, 5, 7, 9] {
                add_sine(&mut samples, f * harmonic as f64, |_| 1.0 / harmonic as f64);
            }
            for (n, sample) in samples.iter_mut().enumerate() {
                *sample =
                    (*sample + 0.025 * random.bipolar()) * attack(n as f64 / RATE as f64, 0.08);
            }
        }
        11 => {
            for harmonic in 1..=24 {
                add_sine(&mut samples, f * harmonic as f64, |t| {
                    let brightness = 1.0 + 11.0 * t.min(1.0);
                    (-(harmonic as f64 - 1.0) / brightness).exp() / harmonic as f64
                        * attack(t, 0.25)
                });
            }
        }
        12 => {
            for (n, sample) in samples.iter_mut().enumerate() {
                let t = n as f64 / RATE as f64;
                let phase = TAU * f * t;
                let index = 6.0 * (-t / 0.8).exp() * (1.0 - t / recipe.seconds);
                *sample = (phase + index * (3.5 * phase).sin()).sin() * (-t / 1.5).exp();
            }
        }
        13 => {
            for _ in 0..24 {
                let ratio = 1.0 + 11.0 * random.unit();
                let decay = 0.5 + 2.0 * random.unit();
                let phase = TAU * random.unit();
                for (n, sample) in samples.iter_mut().enumerate() {
                    let t = n as f64 / RATE as f64;
                    *sample +=
                        (TAU * 180.0 * ratio * t + phase).sin() * (-t / decay).exp() / ratio.sqrt();
                }
            }
        }
        14 => {
            let mut filter = Bandpass::new(1100.0, 1.2);
            for (n, sample) in samples.iter_mut().enumerate() {
                let t = n as f64 / RATE as f64;
                filter.tune(1100.0 + 450.0 * (TAU * 0.18 * t).sin(), 1.2);
                *sample = filter.process(random.bipolar()) * attack(t, 0.1);
            }
        }
        15 => {
            for (harmonic, level) in [(1, 1.0), (2, 0.12), (3, 0.05)] {
                add_sine(&mut samples, f * harmonic as f64, |t| {
                    level * (0.85 + 0.15 * (TAU * 0.3 * t).sin()) * attack(t, 0.2)
                });
            }
        }
        _ => unreachable!(),
    }
    finish(&mut samples);
    samples
}

fn add_sine(samples: &mut [f64], hz: f64, amplitude: impl Fn(f64) -> f64) {
    for (n, sample) in samples.iter_mut().enumerate() {
        let t = n as f64 / RATE as f64;
        *sample += (TAU * hz * t).sin() * amplitude(t);
    }
}

fn modal(samples: &mut [f64], f: f64, ratios: &[f64], levels: &[f64], decays: &[f64]) {
    for ((&ratio, &level), &decay) in ratios.iter().zip(levels).zip(decays) {
        add_sine(samples, f * ratio, |t| level * (-t / decay).exp());
    }
}

fn pluck(samples: &mut [f64], f: f64, damping: f64, t60: f64, random: &mut Random) {
    let omega = TAU * f / RATE as f64;
    // Compensate the loop filter's phase delay before splitting the delay into
    // integer and fractional samples. Linear interpolation supplies the fraction.
    let filter_delay = (damping * omega.sin()).atan2(1.0 - damping + damping * omega.cos()) / omega;
    let delay = RATE as f64 / f - filter_delay;
    let mut ring: Vec<f64> = (0..delay.ceil() as usize + 1)
        .map(|_| random.bipolar())
        .collect();
    let mean = ring.iter().sum::<f64>() / ring.len() as f64;
    for value in &mut ring {
        *value -= mean;
    }
    let gain = (-1000.0f64.ln() / (t60 * f)).exp();
    let mut previous = 0.0;
    let mut write = 0;
    for sample in samples {
        let read = (write as f64 - delay).rem_euclid(ring.len() as f64);
        let left = read.floor() as usize;
        let fraction = read - left as f64;
        let delayed = ring[left] * (1.0 - fraction) + ring[(left + 1) % ring.len()] * fraction;
        let filtered = ((1.0 - damping) * delayed + damping * previous) * gain;
        previous = delayed;
        ring[write] = filtered;
        write = (write + 1) % ring.len();
        *sample = filtered;
    }
}

struct Bandpass {
    b0: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Bandpass {
    fn new(hz: f64, q: f64) -> Self {
        let mut filter = Self {
            b0: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        };
        filter.tune(hz, q);
        filter
    }

    fn tune(&mut self, hz: f64, q: f64) {
        let omega = TAU * hz / RATE as f64;
        let alpha = omega.sin() / (2.0 * q);
        self.b0 = alpha / (1.0 + alpha);
        self.a1 = -2.0 * omega.cos() / (1.0 + alpha);
        self.a2 = (1.0 - alpha) / (1.0 + alpha);
    }

    fn process(&mut self, input: f64) -> f64 {
        let output = self.b0 * input + self.z1;
        self.z1 = -self.a1 * output + self.z2;
        self.z2 = -self.b0 * input - self.a2 * output;
        output
    }
}

fn finish(samples: &mut [f64]) {
    let last = samples.len() - 1;
    let window = |n: usize| {
        let attack = (n as f64 / (0.005 * RATE as f64)).min(1.0);
        let release = ((last - n) as f64 / (0.030 * RATE as f64)).min(1.0);
        // Smooth endpoints, including the final sample at exactly zero.
        (0.5 - 0.5 * (PI * attack).cos()) * (0.5 - 0.5 * (PI * release).cos())
    };
    for (n, sample) in samples.iter_mut().enumerate() {
        *sample *= window(n);
    }
    // Remove the mean through the same window so DC correction preserves fades.
    let dc = samples.iter().sum::<f64>() / (0..samples.len()).map(window).sum::<f64>();
    for (n, sample) in samples.iter_mut().enumerate() {
        *sample -= dc * window(n);
    }
    let peak = samples
        .iter()
        .fold(0.0f64, |peak, sample| peak.max(sample.abs()));
    assert!(peak.is_finite() && peak > 0.0);
    for sample in samples {
        *sample *= PEAK / peak;
        assert!(sample.is_finite());
    }
}

fn write_wav(path: &Path, samples: &[f64]) -> io::Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    let data_bytes = (samples.len() * 2) as u32;
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_bytes).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&1u16.to_le_bytes())?; // mono
    out.write_all(&RATE.to_le_bytes())?;
    out.write_all(&(RATE * 2).to_le_bytes())?;
    out.write_all(&2u16.to_le_bytes())?; // block alignment
    out.write_all(&16u16.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&data_bytes.to_le_bytes())?;
    for sample in samples {
        out.write_all(&((sample * i16::MAX as f64).round() as i16).to_le_bytes())?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "mangle-sources-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn u16_at(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn pcm(bytes: &[u8]) -> Vec<f64> {
        bytes[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair) as f64 / i16::MAX as f64)
            .collect()
    }

    #[test]
    fn generation_is_byte_identical() {
        let first = TempDir::new();
        let second = TempDir::new();
        generate(&first.0).unwrap();
        generate(&second.0).unwrap();
        for id in 1..=16 {
            let path = format!("sources/{id:04}.wav");
            assert_eq!(
                fs::read(first.0.join(&path)).unwrap(),
                fs::read(second.0.join(&path)).unwrap(),
                "{path}"
            );
        }
        assert_eq!(
            fs::read(first.0.join("generated.tsv")).unwrap(),
            fs::read(second.0.join("generated.tsv")).unwrap()
        );
    }

    #[test]
    fn wav_format_levels_lengths_and_fades() {
        let dir = TempDir::new();
        generate(&dir.0).unwrap();
        assert_eq!(fs::read_dir(dir.0.join("sources")).unwrap().count(), 16);
        for (i, recipe) in RECIPES.iter().enumerate() {
            let bytes = fs::read(dir.0.join(format!("sources/{:04}.wav", i + 1))).unwrap();
            assert_eq!(&bytes[0..4], b"RIFF");
            assert_eq!(u32_at(&bytes, 4) as usize + 8, bytes.len());
            assert_eq!(&bytes[8..16], b"WAVEfmt ");
            assert_eq!(u32_at(&bytes, 16), 16);
            assert_eq!(u16_at(&bytes, 20), 1);
            assert_eq!(u16_at(&bytes, 22), 1);
            assert_eq!(u32_at(&bytes, 24), 44_100);
            assert_eq!(u32_at(&bytes, 28), 88_200);
            assert_eq!(u16_at(&bytes, 32), 2);
            assert_eq!(u16_at(&bytes, 34), 16);
            assert_eq!(&bytes[36..40], b"data");
            assert_eq!(u32_at(&bytes, 40) as usize, bytes.len() - 44);
            let samples = pcm(&bytes);
            let peak = samples.iter().fold(0.0f64, |p, s| p.max(s.abs()));
            assert!((0.85..=0.90).contains(&peak), "{}: {peak}", recipe.name);
            assert!((samples.len() as f64 / 44_100.0 - recipe.seconds).abs() < 0.1);
            assert!(samples.iter().all(|s| s.is_finite() && s.abs() < 1.0));
            let rms = (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt();
            assert!(rms > 0.005, "{}: {rms}", recipe.name);
            let mean = samples.iter().sum::<f64>() / samples.len() as f64;
            assert!(mean.abs() < 1.0 / 32767.0, "{}: DC={mean}", recipe.name);
            assert_eq!(samples[0], 0.0);
            assert_eq!(samples.last(), Some(&0.0));
            assert!(samples[samples.len() - 32..].iter().all(|s| s.abs() < 0.01));
        }
    }

    #[test]
    fn index_has_the_factory_columns() {
        let dir = TempDir::new();
        generate(&dir.0).unwrap();
        let tsv = fs::read_to_string(dir.0.join("generated.tsv")).unwrap();
        let rows: Vec<_> = tsv.lines().collect();
        assert_eq!(rows.len(), 16);
        for (i, row) in rows.iter().enumerate() {
            let columns: Vec<_> = row.split('\t').collect();
            assert_eq!(columns.len(), 8);
            assert_eq!(columns[0], format!("{:04}", i + 1));
            assert_eq!(columns[1], RECIPES[i].name);
            assert_eq!(columns[2], RECIPES[i].category);
            assert_eq!(columns[3], "gen");
            assert_eq!(columns[4], format!("sources/{:04}.wav", i + 1));
            assert_eq!(columns[5], RECIPES[i].root.to_string());
            assert_eq!(columns[6], "generated");
            assert!(columns[7].starts_with(RECIPES[i].parameters));
            assert!(columns[7].contains("seed="));
        }
    }

    // Normalized autocorrelation over a fixed window, with a parabolic peak
    // interpolation. Search around the expected octave, allowing +/- 600 cents.
    fn estimate_f0(samples: &[f64], expected: f64) -> f64 {
        let period = RATE as f64 / expected;
        let min = (period / 2.0f64.sqrt()).floor() as usize;
        let max = (period * 2.0f64.sqrt()).ceil() as usize;
        let start = (1.2 * RATE as f64) as usize;
        let window = (0.25 * RATE as f64) as usize;
        let mean = samples[start..start + window + max + 1].iter().sum::<f64>()
            / (window + max + 1) as f64;
        let correlation = |lag: usize| {
            let mut xy = 0.0;
            let mut xx = 0.0;
            let mut yy = 0.0;
            for n in start..start + window {
                let x = samples[n] - mean;
                let y = samples[n + lag] - mean;
                xy += x * y;
                xx += x * x;
                yy += y * y;
            }
            xy / (xx * yy).sqrt()
        };
        let lag = (min..=max)
            .max_by(|&a, &b| correlation(a).total_cmp(&correlation(b)))
            .unwrap();
        assert!(lag > min && lag < max);
        let left = correlation(lag - 1);
        let middle = correlation(lag);
        let right = correlation(lag + 1);
        assert!(middle > 0.8, "weak periodicity: {middle}");
        let offset = 0.5 * (left - right) / (left - 2.0 * middle + right);
        RATE as f64 / (lag as f64 + offset)
    }

    #[test]
    fn pitched_sources_match_their_roots() {
        let dir = TempDir::new();
        generate(&dir.0).unwrap();
        for i in [0, 1, 2, 3, 6, 7, 15] {
            let bytes = fs::read(dir.0.join(format!("sources/{:04}.wav", i + 1))).unwrap();
            let expected = frequency(RECIPES[i].root);
            let actual = estimate_f0(&pcm(&bytes), expected);
            let cents = 1200.0 * (actual / expected).log2();
            eprintln!("{}: {actual:.3} Hz, {cents:.3} cents", RECIPES[i].name);
            assert!(cents.abs() < 20.0, "{}: {cents} cents", RECIPES[i].name);
        }
    }
}
