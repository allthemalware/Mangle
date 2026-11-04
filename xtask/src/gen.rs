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
    generate_selection(outdir, &variants())
}

fn generate_selection(outdir: &Path, variants: &[Variant]) -> io::Result<()> {
    fs::create_dir_all(outdir.join("sources"))?;
    let mut index = BufWriter::new(File::create(outdir.join("generated.tsv"))?);
    for (i, recipe) in RECIPES.iter().enumerate() {
        let id = i + 1;
        let samples = synthesize(i);
        write_wav(&outdir.join(format!("sources/{id:04}.wav")), &samples, 1)?;
        writeln!(index,
            "{id:04}\t{}\t{}\tgen\tsources/{id:04}.wav\t{}\tgenerated\t{}; root={}; duration={}s; seed={}; sample_rate={RATE}; attack_floor=5ms; fade=30ms; DC=removed; peak={PEAK}",
            recipe.name, recipe.category, recipe.root, recipe.parameters,
            recipe.root, recipe.seconds, SEED + id as u64)?;
    }
    for variant in variants {
        let samples = synthesize_variant(variant);
        write_wav(
            &outdir.join(format!("sources/{:04}.wav", variant.id)),
            &samples,
            variant.knobs.stereo.channels(),
        )?;
        writeln!(index, "{}", variant.row())?;
    }
    index.flush()
}

// These descriptors follow the original recipes so their positions remain stable.
const NEW_RECIPES: [Recipe; 14] = [
    Recipe { name: "Bowed String", category: "strings", root: 60, seconds: 4.0,
        parameters: "filtered saw harmonics=1..18; bow_noise=0.035; attack=0.25s" },
    Recipe { name: "Modal Bar", category: "bars", root: 60, seconds: 4.0,
        parameters: "marimba/vibraphone; modes=1,4,10 or 1,2.76,5.4; tremolo=5Hz" },
    Recipe { name: "Singing Bowl", category: "bells", root: 60, seconds: 4.0,
        parameters: "beating modal pairs; ratios=1,2,3,4.2; pair_offset=0.35..1.4Hz" },
    Recipe { name: "Wind Chimes", category: "bells", root: -1, seconds: 4.0,
        parameters: "8 sparse seeded strikes; ratios=1,1.25,1.5,2; bell_modes=1,2.76,5.4" },
    Recipe { name: "Vowel Choir", category: "voice", root: 60, seconds: 4.0,
        parameters: "additive formants; vowels=a,e,i,o,u; harmonics=1..32; attack=0.3s" },
    Recipe { name: "Unison Pad", category: "pads", root: 60, seconds: 4.0,
        parameters: "3 detuned saw voices; harmonics=1..12; attack=0.6s" },
    Recipe { name: "Shepard Drone", category: "drones", root: -1, seconds: 4.0,
        parameters: "5 octave layers; Gaussian register window; rise/fall=0.2oct/s; attack=0.3s" },
    Recipe { name: "Harmonic Stack", category: "pads", root: 60, seconds: 4.0,
        parameters: "chords=maj,min,sus,5th; 4 harmonics per note; root_anchor=1; attack=0.15s" },
    Recipe { name: "Comb Noise", category: "noise", root: -1, seconds: 4.0,
        parameters: "filtered white noise; tuned feedback comb; feedback=0.78; attack=0.1s" },
    Recipe { name: "FM Cluster", category: "bells", root: 60, seconds: 4.0,
        parameters: "3 detuned FM carriers; ratios=2.71,3.13,4.17; index_decay=0.35s; amplitude_decay=length/2" },
    Recipe { name: "Noise Cloud", category: "noise", root: -1, seconds: 4.0,
        parameters: "48 seeded Hann-windowed filtered noise grains; grain=60..240ms" },
    Recipe { name: "Sympathetic Harp", category: "plucked", root: 60, seconds: 4.0,
        parameters: "Karplus-Strong; sympathetic modes=1,2,3,4; resonance_level=0.22/n" },
    Recipe { name: "Soft Sub", category: "drones", root: 36, seconds: 4.0,
        parameters: "soft additive bass; harmonics=1,2,3; levels=1,0.15*brightness,0.1*brightness; tremolo=0.3Hz; attack=0.15s" },
    Recipe { name: "Evolving Texture", category: "noise", root: -1, seconds: 4.0,
        parameters: "12 seeded inharmonic modes; slow independent amplitude cycles; filtered noise bed" },
];

fn recipe(family: usize) -> &'static Recipe {
    if family < RECIPES.len() {
        &RECIPES[family]
    } else {
        &NEW_RECIPES[family - RECIPES.len()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stereo {
    Mono,
    Decorrelated,
    Haas,
    WideChorus,
    AutopanFree,
}

impl Stereo {
    fn channels(self) -> u16 {
        if self == Self::Mono {
            1
        } else {
            2
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Mono => "mono",
            Self::Decorrelated => "decorrelated",
            Self::Haas => "haas",
            Self::WideChorus => "wide-chorus",
            Self::AutopanFree => "autopan-free",
        }
    }
}

#[derive(Clone, Copy)]
struct Knobs {
    root: i32,
    brightness: f64,
    detune: f64,
    stereo: Stereo,
    seed_offset: u64,
    seconds: f64,
    colour: usize,
}

#[derive(Clone)]
struct Variant {
    id: usize,
    family: usize,
    knobs: Knobs,
    flavour: String,
}

impl Variant {
    fn name(&self) -> String {
        const NOTES: [&str; 12] = [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ];
        format!(
            "{} {}{} {}",
            recipe(self.family).name,
            NOTES[(self.knobs.root % 12) as usize],
            self.knobs.root / 12 - 1,
            self.flavour
        )
    }

    fn root(&self) -> i32 {
        if recipe(self.family).root < 0 {
            -1
        } else {
            self.knobs.root
        }
    }

    fn parameters(&self) -> &'static str {
        match self.family {
            0 | 1 => "Karplus-Strong; t60=0.85*duration; sympathetic_modes=1..4; resonance_level=0.06/n",
            2 => "modal; ratios=1,2.76,5.4,8.93; decay=0.4*duration/(1+mode)",
            3 => "modal; ratios=1,3.9,9.2; decay=0.23*duration/(1+mode)",
            4 => "modal; ratios=1,0.5,1.19,1.56,2,2.51,2.66,3.01; decay=0.6*duration/(1+mode)",
            5 => "modal; ratios=1,2.32,4.25,6.63; decay=0.4*duration/(1+mode)",
            6 => "additive drawbars; harmonics=1,2,3,4,6,8; detuned chorus_level=0.1/n",
            7 => "3 detuned saw voices; harmonics=1..12; spectral_rolloff=2+10*brightness; attack=0.6s",
            8 | 9 => "additive vowel formants; harmonics=1..32; a=800,1150,2900Hz; u=300,870,2240Hz; attack=0.3s",
            10 => "odd harmonics=1..17; spectral_rolloff=2+12*brightness; filtered breath=0.035; attack=0.08s",
            11 => "brass harmonics=1..18; spectral_rolloff=2+12*brightness; attack=0.25s",
            12 => "2-op FM; ratio=3.5; index=6*brightness*exp(-t/0.35); amplitude_decay=duration/2",
            13 => "12 seeded modes; ratios=uniform(1,11); levels=brightness/(1+mode); decay=0.5+duration*uniform(0,1)",
            14 => "white noise; bandpass_centre=min(12000,root_hz*(2+8*brightness)); Q=1.2; attack=0.1s",
            15 => "additive bass; harmonics=1,2,3; levels=1,0.15*brightness,0.1*brightness; tremolo=0.85+0.15*sin(2*pi*0.3*t); attack=0.15s",
            _ => recipe(self.family).parameters,
        }
    }

    fn row(&self) -> String {
        let k = self.knobs;
        format!("{:04}\t{}\t{}\tgen\tsources/{:04}.wav\t{}\tgenerated\t{}; recipe={}; flavour={}; root={}; brightness={}; damping={}; detune={}cents; colour={}; duration={}s; seed={}; seed_offset={}; stereo: {}; sample_rate={RATE}; attack_floor=5ms; fade=30ms; DC=removed; peak={PEAK}",
            self.id, self.name(), recipe(self.family).category, self.id, self.root(),
            self.parameters(), recipe(self.family).name, self.flavour,
            k.root, k.brightness, if self.family == 1 { 0.15 * (1.0 - k.brightness) } else { 0.65 - 0.4 * k.brightness }, k.detune, k.colour,
            k.seconds, SEED + k.seed_offset, k.seed_offset, k.stereo.name())
    }
}

fn variants() -> Vec<Variant> {
    let mut result = Vec::with_capacity(483);
    for family in 0..30 {
        let count = if family < 16 {
            12
        } else if family == 29 {
            5
        } else {
            22
        };
        for n in 0..count {
            let offset = result.len();
            let colour = n / 6;
            let tone = ["Dark", "Bright", "Warm", "Air"][colour];
            let flavour = match family {
                17 => format!(
                    "{} {tone}",
                    if n % 2 == 0 { "Marimba" } else { "Vibraphone" }
                ),
                20 => format!("{} {tone}", ["A", "E", "I", "O", "U"][n % 5]),
                21 => format!("{} {tone}", ["Narrow", "Medium", "Wide"][n % 3]),
                22 => format!("{} {tone}", if n % 2 == 0 { "Rising" } else { "Falling" }),
                23 => format!("{} {tone}", ["Major", "Minor", "Sus", "Fifth"][n % 4]),
                _ => tone.to_owned(),
            };
            // The integer accumulator distributes 265 stereo sounds evenly over 483 variants.
            let stereo = if (offset + 1) * 55 / 100 > offset * 55 / 100 {
                [
                    Stereo::Decorrelated,
                    Stereo::Haas,
                    Stereo::WideChorus,
                    Stereo::AutopanFree,
                ][offset % 4]
            } else {
                Stereo::Mono
            };
            result.push(Variant {
                id: 17 + offset,
                family,
                flavour,
                knobs: Knobs {
                    root: (if family == 15 || family == 28 { 24 } else { 36 })
                        + (n % 6) as i32 * 12,
                    brightness: [0.45, 0.9, 0.2, 0.65][colour],
                    detune: [3.0, 8.0, 15.0][n % 3],
                    stereo,
                    seed_offset: 17 + offset as u64,
                    seconds: 2.0 + (n % 5) as f64,
                    colour: n,
                },
            });
        }
    }
    result
}

// Offline synthesis uses recursive oscillators to keep large additive banks cheap.
// Each partial also feeds a static pan bus for the autopan-free stereo method.
struct Voice {
    mid: Vec<f64>,
    pan: Vec<f64>,
}

impl Voice {
    fn new(seconds: f64) -> Self {
        let frames = (seconds * RATE as f64).round() as usize;
        Self {
            mid: vec![0.0; frames],
            pan: vec![0.0; frames],
        }
    }

    fn tone(&mut self, hz: f64, gain: f64, decay: f64, pan: f64, start: usize) {
        if hz >= RATE as f64 * 0.48 {
            return;
        }
        let (step_sin, step_cos) = (TAU * hz / RATE as f64).sin_cos();
        let (mut sine, mut cosine) = (0.0, gain);
        let damping = if decay > 0.0 {
            (-1.0 / (RATE as f64 * decay)).exp()
        } else {
            1.0
        };
        for n in start..self.mid.len() {
            self.mid[n] += sine;
            self.pan[n] += sine * pan;
            (sine, cosine) = (
                (sine * step_cos + cosine * step_sin) * damping,
                (cosine * step_cos - sine * step_sin) * damping,
            );
        }
    }

    fn envelope(&mut self, seconds: f64) {
        for (n, (mid, pan)) in self.mid.iter_mut().zip(&mut self.pan).enumerate() {
            let gain = attack(n as f64 / RATE as f64, seconds);
            *mid *= gain;
            *pan *= gain;
        }
    }

    fn modes(&mut self, f: f64, ratios: &[f64], brightness: f64, decay: f64) {
        for (i, ratio) in ratios.iter().enumerate() {
            let gain = if i == 0 {
                1.0
            } else {
                brightness / (i + 1) as f64
            };
            self.tone(f * ratio, gain, decay / (1.0 + i as f64), partial_pan(i), 0);
        }
    }
}

fn partial_pan(i: usize) -> f64 {
    if i == 0 {
        0.0
    } else if i.is_multiple_of(2) {
        -0.9
    } else {
        0.9
    }
}

fn render_variant(v: &Variant, seed: u64, cents: f64) -> Voice {
    let k = v.knobs;
    let f = frequency(k.root) * 2.0f64.powf(cents / 1200.0);
    let b = k.brightness;
    let mut voice = Voice::new(k.seconds);
    let mut random = Random(seed);
    match v.family {
        0 | 1 | 27 => {
            let damping = if v.family == 1 {
                0.15 * (1.0 - b)
            } else {
                0.65 - 0.4 * b
            };
            pluck(&mut voice.mid, f, damping, k.seconds * 0.85, &mut random);
            // A sympathetic fundamental steadies the pitch of the noise-excited string.
            for h in 1..=4 {
                voice.tone(
                    f * h as f64,
                    (if v.family == 27 { 0.22 } else { 0.06 }) / h as f64,
                    k.seconds / h as f64,
                    partial_pan(h),
                    0,
                );
            }
        }
        2 => voice.modes(f, &[1.0, 2.76, 5.4, 8.93], b, k.seconds * 0.4),
        3 => voice.modes(f, &[1.0, 3.9, 9.2], b, k.seconds * 0.23),
        4 => voice.modes(
            f,
            &[1.0, 0.5, 1.19, 1.56, 2.0, 2.51, 2.66, 3.01],
            b,
            k.seconds * 0.6,
        ),
        5 => voice.modes(f, &[1.0, 2.32, 4.25, 6.63], b, k.seconds * 0.4),
        6 => {
            for (i, h) in [1, 2, 3, 4, 6, 8].iter().enumerate() {
                voice.tone(
                    f * *h as f64,
                    if i == 0 { 1.0 } else { b / (i as f64).sqrt() },
                    0.0,
                    partial_pan(i),
                    0,
                );
                for sign in [-1.0, 1.0] {
                    voice.tone(
                        f * *h as f64 * 2.0f64.powf(sign * k.detune / 1200.0),
                        0.1 / *h as f64,
                        0.0,
                        sign,
                        0,
                    );
                }
            }
        }
        7 | 21 => {
            for detune in [-k.detune, 0.0, k.detune] {
                for h in 1..=12 {
                    voice.tone(
                        f * h as f64 * 2.0f64.powf(detune / 1200.0),
                        (-(h as f64) / (2.0 + 10.0 * b)).exp() / h as f64,
                        0.0,
                        if detune == 0.0 {
                            partial_pan(h - 1)
                        } else {
                            detune / k.detune
                        },
                        0,
                    );
                }
            }
            voice.envelope(0.6);
        }
        8 | 9 | 20 => {
            let vowel = if v.family == 8 {
                0
            } else if v.family == 9 {
                4
            } else {
                k.colour % 5
            };
            let formants = [
                [800.0, 1150.0, 2900.0],
                [400.0, 1700.0, 2600.0],
                [300.0, 2200.0, 3000.0],
                [450.0, 800.0, 2830.0],
                [300.0, 870.0, 2240.0],
            ][vowel];
            for h in 1..=32 {
                let hz = f * h as f64;
                let formant = formants
                    .iter()
                    .map(|centre| (-0.5 * ((hz - centre) / (100.0 + 180.0 * b)).powi(2)).exp())
                    .sum::<f64>();
                let gain = if h == 1 {
                    1.0
                } else {
                    (0.15 + formant * b * 3.0) / h as f64
                };
                voice.tone(hz, gain, 0.0, partial_pan(h - 1), 0);
            }
            voice.envelope(0.3);
        }
        10 | 11 | 16 => {
            for h in 1..=18 {
                if v.family == 10 && h % 2 == 0 {
                    continue;
                }
                voice.tone(
                    f * h as f64,
                    (-((h - 1) as f64) / (2.0 + 12.0 * b)).exp() / h as f64,
                    0.0,
                    partial_pan(h - 1),
                    0,
                );
            }
            if v.family != 11 {
                let mut low = 0.0;
                for (mid, pan) in voice.mid.iter_mut().zip(&mut voice.pan) {
                    low += 0.15 * (random.bipolar() - low);
                    *mid += 0.035 * low;
                    *pan += 0.035 * random.bipolar();
                }
            }
            voice.envelope(if v.family == 10 { 0.08 } else { 0.25 });
        }
        12 | 25 => {
            let ratios: &[f64] = if v.family == 12 {
                &[3.5]
            } else {
                &[2.71, 3.13, 4.17]
            };
            for (i, ratio) in ratios.iter().enumerate() {
                let hz = f * 2.0f64
                    .powf((i as f64 - (ratios.len() - 1) as f64 / 2.0) * k.detune / 1200.0);
                for (n, (mid, pan)) in voice.mid.iter_mut().zip(&mut voice.pan).enumerate() {
                    let t = n as f64 / RATE as f64;
                    let phase = TAU * hz * t;
                    let sample = (phase + 6.0 * b * (-t / 0.35).exp() * (phase * ratio).sin())
                        .sin()
                        * (-t / (k.seconds / 2.0)).exp()
                        / ratios.len() as f64;
                    *mid += sample;
                    // Pan the FM sidebands separately from the carrier; panning
                    // the whole operator would only change channel gain.
                    let carrier =
                        phase.sin() * (-t / (k.seconds / 2.0)).exp() / ratios.len() as f64;
                    *pan += (sample - carrier) * partial_pan(i + 1);
                }
            }
        }
        13 | 29 => {
            for i in 0..12 {
                let hz = f * (1.0 + 10.0 * random.unit());
                let decay = if v.family == 13 {
                    0.5 + k.seconds * random.unit()
                } else {
                    0.0
                };
                let gain = b / (i + 1) as f64;
                let pan = random.bipolar();
                if v.family == 29 {
                    // Symmetric sidebands give each mode its own slow AM cycle.
                    let rate = 0.05 + 0.2 * random.unit();
                    voice.tone(hz, gain * 0.6, 0.0, pan, 0);
                    voice.tone(hz - rate, gain * 0.2, 0.0, pan, 0);
                    voice.tone(hz + rate, gain * 0.2, 0.0, pan, 0);
                } else {
                    voice.tone(hz, gain, decay, pan, 0);
                }
            }
            if v.family == 29 {
                let mut low = 0.0;
                for sample in &mut voice.mid {
                    low += 0.02 * (random.bipolar() - low);
                    *sample += 0.015 * low;
                }
                voice.envelope(0.5);
            }
        }
        14 | 24 => {
            let mut filter = Bandpass::new((f * (2.0 + b * 8.0)).min(12_000.0), 1.2);
            let mut ring = vec![0.0; (RATE as f64 / f).round() as usize];
            for n in 0..voice.mid.len() {
                let noise = filter.process(random.bipolar());
                let sample = noise
                    + if v.family == 24 {
                        0.78 * ring[n % ring.len()]
                    } else {
                        0.0
                    };
                let position = n % ring.len();
                ring[position] = sample;
                voice.mid[n] = sample;
                voice.pan[n] = random.bipolar() * 0.12;
            }
            voice.envelope(0.1);
        }
        15 | 28 => {
            voice.modes(f, &[1.0, 2.0, 3.0], b * 0.3, 0.0);
            voice.envelope(0.15);
            for n in 0..voice.mid.len() {
                let gain = 0.85 + 0.15 * (TAU * 0.3 * n as f64 / RATE as f64).sin();
                voice.mid[n] *= gain;
                voice.pan[n] *= gain;
            }
        }
        17 => {
            let marimba = k.colour.is_multiple_of(2);
            voice.modes(
                f,
                if marimba {
                    &[1.0, 4.0, 10.0]
                } else {
                    &[1.0, 2.76, 5.4]
                },
                b,
                k.seconds * if marimba { 0.25 } else { 0.6 },
            );
            for n in 0..voice.mid.len() {
                let gain = 0.8 + 0.2 * (TAU * 5.0 * n as f64 / RATE as f64).sin();
                voice.mid[n] *= gain;
                voice.pan[n] *= gain;
            }
        }
        18 => {
            for (i, ratio) in [1.0, 2.0, 3.0, 4.2].iter().enumerate() {
                for offset in [-0.175, 0.175] {
                    voice.tone(
                        f * ratio + offset * (i + 1) as f64,
                        if i == 0 { 0.5 } else { b * 0.3 / i as f64 },
                        k.seconds / (1.0 + i as f64 * 0.4),
                        if offset < 0.0 { -0.8 } else { 0.8 },
                        0,
                    );
                }
            }
        }
        19 => {
            for strike in 0..8 {
                let start = (strike as f64 / 8.0 * (k.seconds - 0.4) * RATE as f64) as usize;
                let ratio = [1.0, 1.25, 1.5, 2.0][(random.unit() * 4.0) as usize];
                let pan = random.bipolar();
                for (h, mode) in [1.0, 2.76, 5.4].iter().enumerate() {
                    voice.tone(f * ratio * mode, 1.0 / (h + 1) as f64, 0.5 + b, pan, start);
                }
            }
        }
        22 => {
            let direction = if k.colour.is_multiple_of(2) {
                1.0
            } else {
                -1.0
            };
            for octave in -2..=2 {
                let mut phase = 0.0;
                for n in 0..voice.mid.len() {
                    let position = (octave as f64 + direction * 0.2 * n as f64 / RATE as f64 + 2.5)
                        .rem_euclid(5.0)
                        - 2.5;
                    let hz = f * 2.0f64.powf(position);
                    phase += TAU * hz / RATE as f64;
                    let sample = phase.sin() * (-position * position * 1.4).exp();
                    if hz < RATE as f64 * 0.48 {
                        voice.mid[n] += sample;
                        voice.pan[n] += sample * octave as f64 * 0.4;
                    }
                }
            }
            voice.envelope(0.3);
        }
        23 => {
            let intervals = [[0, 4, 7], [0, 3, 7], [0, 5, 7], [0, 7, 12]][k.colour % 4];
            voice.tone(f, 1.0, 0.0, 0.0, 0);
            for (i, interval) in intervals.iter().enumerate() {
                for h in 1..=4 {
                    voice.tone(
                        f * 2.0f64.powf(*interval as f64 / 12.0) * h as f64,
                        (if i == 0 { 0.6 } else { 0.18 }) * b / h as f64,
                        0.0,
                        partial_pan(i + h),
                        0,
                    );
                }
            }
            voice.envelope(0.15);
        }
        26 => {
            for _ in 0..48 {
                let start = (random.unit() * (voice.mid.len() - 1) as f64) as usize;
                let length = ((0.06 + 0.18 * random.unit()) * RATE as f64) as usize;
                let pan = random.bipolar();
                let mut low = 0.0;
                for n in 0..length.min(voice.mid.len() - start) {
                    low += (0.05 + b * 0.5) * (random.bipolar() - low);
                    let sample = low * (0.5 - 0.5 * (TAU * n as f64 / length as f64).cos());
                    voice.mid[start + n] += sample;
                    voice.pan[start + n] += sample * pan;
                }
            }
        }
        _ => unreachable!(),
    }
    voice
}

fn interpolated(samples: &[f64], position: f64) -> f64 {
    if position < 0.0 || position >= (samples.len() - 1) as f64 {
        return 0.0;
    }
    let n = position as usize;
    let fraction = position - n as f64;
    samples[n] * (1.0 - fraction) + samples[n + 1] * fraction
}

fn synthesize_variant(v: &Variant) -> Vec<f64> {
    let k = v.knobs;
    let seed = SEED + k.seed_offset;
    let mut voice = render_variant(v, seed, 0.0);
    if k.stereo == Stereo::Mono {
        finish(&mut voice.mid);
        return voice.mid;
    }
    let mut side = match k.stereo {
        Stereo::Decorrelated => {
            let mut random = Random(seed ^ 0x9e37_79b9_7f4a_7c15);
            let left = render_variant(v, seed + 100_003, -k.detune * (0.8 + 0.4 * random.unit()));
            let right = render_variant(v, seed + 200_003, k.detune * (0.8 + 0.4 * random.unit()));
            left.mid
                .iter()
                .zip(right.mid)
                .map(|(l, r)| (l - r) * 0.5)
                .collect::<Vec<_>>()
        }
        Stereo::Haas | Stereo::WideChorus => {
            let haas = k.stereo == Stereo::Haas;
            (0..voice.mid.len())
                .map(|n| {
                    let t = n as f64 / RATE as f64;
                    // Integrated frequency modulation: the derivative of position is
                    // the playback ratio. Chorus copies move by at most the chosen cents.
                    let depth = (2.0f64.powf(k.detune / 1200.0) - 1.0) * RATE as f64 / TAU;
                    let left = if haas {
                        n as f64 * 2.0f64.powf(-3.0 / 1200.0)
                    } else {
                        n as f64 + depth / 0.23 * (TAU * 0.23 * t).sin() - depth / 0.23
                    };
                    let right = if haas {
                        n as f64 * 2.0f64.powf(3.0 / 1200.0) - 0.011 * RATE as f64
                    } else {
                        n as f64 + depth / 0.31 * (TAU * 0.31 * t + 1.0).sin() - depth / 0.31
                    };
                    (interpolated(&voice.mid, left) - interpolated(&voice.mid, right)) * 0.5
                })
                .collect()
        }
        Stereo::AutopanFree => voice.pan,
        Stereo::Mono => unreachable!(),
    };
    // Keep a common dry centre. Bound RMS side energy, preserving a useful mono
    // fold-down even when delayed copies cancel each other at particular notes.
    let mid_energy = voice.mid.iter().map(|x| x * x).sum::<f64>();
    let side_energy = side.iter().map(|x| x * x).sum::<f64>();
    let width = if side_energy > 0.0 {
        (0.5 * (mid_energy / side_energy).sqrt()).min(6.0)
    } else {
        0.0
    };
    for (mid, side) in voice.mid.iter_mut().zip(&mut side) {
        let spread = *side * width;
        (*mid, *side) = (*mid + spread, *mid - spread);
    }
    finish_stereo(&mut voice.mid, &mut side);
    voice
        .mid
        .into_iter()
        .zip(side)
        .flat_map(|(l, r)| [l, r])
        .collect()
}

fn finish_stereo(left: &mut [f64], right: &mut [f64]) {
    // Use the same fades/DC removal as mono, but preserve the channel balance
    // and normalize only once against the louder channel.
    let left_peak = window_and_dc(left);
    let right_peak = window_and_dc(right);
    let gain = PEAK / left_peak.max(right_peak);
    for sample in left.iter_mut().chain(right) {
        *sample *= gain;
        assert!(sample.is_finite());
    }
}

fn window_and_dc(samples: &mut [f64]) -> f64 {
    let last = samples.len() - 1;
    let window: Vec<_> = (0..samples.len())
        .map(|n| {
            let a = (n as f64 / (0.005 * RATE as f64)).min(1.0);
            let r = ((last - n) as f64 / (0.030 * RATE as f64)).min(1.0);
            (0.5 - 0.5 * (PI * a).cos()) * (0.5 - 0.5 * (PI * r).cos())
        })
        .collect();
    for (sample, w) in samples.iter_mut().zip(&window) {
        *sample *= w;
    }
    let dc = samples.iter().sum::<f64>() / window.iter().sum::<f64>();
    let mut peak = 0.0f64;
    for (sample, w) in samples.iter_mut().zip(window) {
        *sample -= dc * w;
        peak = peak.max(sample.abs());
    }
    assert!(peak.is_finite() && peak > 0.0);
    peak
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

fn write_wav(path: &Path, samples: &[f64], channels: u16) -> io::Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    let data_bytes = (samples.len() * 2) as u32;
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_bytes).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&channels.to_le_bytes())?;
    out.write_all(&RATE.to_le_bytes())?;
    out.write_all(&(RATE * 2 * channels as u32).to_le_bytes())?;
    out.write_all(&(2 * channels).to_le_bytes())?; // block alignment
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
        generate_selection(&first.0, &[]).unwrap();
        generate_selection(&second.0, &[]).unwrap();
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
        generate_selection(&dir.0, &[]).unwrap();
        assert_eq!(fs::read_dir(dir.0.join("sources")).unwrap().count(), 16);
        for (i, recipe) in RECIPES.iter().enumerate() {
            let bytes = fs::read(dir.0.join(format!("sources/{:04}.wav", i + 1))).unwrap();
            assert_eq!(checksum(&bytes), LEGACY_CHECKSUMS[i], "{}", recipe.name);
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
        generate_selection(&dir.0, &[]).unwrap();
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

    const LEGACY_CHECKSUMS: [u64; 16] = [
        0xc36226fdde46f8e8,
        0x3a04d3ca2f172de6,
        0xd40781adccb33c6e,
        0xc850fecad4ecb5d4,
        0xa35fdb2b6292f7e9,
        0x3e0089fd0989c780,
        0x7b7ad9f0010b7df7,
        0x995bb17bb1acfe19,
        0xc438e5fb76a4b3c5,
        0xcb5d18eb8dff2341,
        0x2de2cf199e0dd7a8,
        0x9e0fe636b1341bf1,
        0x2f3bf9a85f32a71d,
        0x7667f735b444ddf7,
        0xbfc5bb00f585fd7e,
        0x92ac2e14fb926d73,
    ];

    fn checksum(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ *byte as u64).wrapping_mul(0x100_0000_01b3)
        })
    }

    #[test]
    fn expanded_catalogue_is_contiguous_unique_and_parseable() {
        use std::collections::HashSet;
        let variants = variants();
        assert_eq!(variants.len() + RECIPES.len(), 499);
        let mut names: HashSet<_> = RECIPES.iter().map(|r| r.name.to_owned()).collect();
        let mut paths = HashSet::new();
        for (i, v) in variants.iter().enumerate() {
            let row = v.row();
            let columns: Vec<_> = row.split('\t').collect();
            assert_eq!(columns.len(), 8);
            assert_eq!(columns[0], format!("{:04}", i + 17));
            assert!(names.insert(columns[1].to_owned()), "{}", v.name());
            assert_eq!(columns[2], recipe(v.family).category);
            assert_eq!(columns[3], "gen");
            assert_eq!(columns[4], format!("sources/{:04}.wav", i + 17));
            assert!(!columns[4].contains(".."));
            assert!(paths.insert(columns[4].to_owned()));
            assert!((-1..=127).contains(&columns[5].parse::<i32>().unwrap()));
            assert_eq!(columns[6], "generated");
            for field in [
                "recipe=",
                "root=",
                "brightness=",
                "detune=",
                "duration=",
                "seed_offset=",
                "stereo: ",
            ] {
                assert!(columns[7].contains(field));
            }
            assert!((2.0..=6.0).contains(&v.knobs.seconds));
        }
        assert_eq!(names.len(), 499);
        let stereo = variants
            .iter()
            .filter(|v| v.knobs.stereo != Stereo::Mono)
            .count();
        assert!((0.54..0.56).contains(&(stereo as f64 / variants.len() as f64)));
        for family in 0..30 {
            let members: Vec<_> = variants.iter().filter(|v| v.family == family).collect();
            assert_eq!(
                members.len(),
                if family < 16 {
                    12
                } else if family == 29 {
                    5
                } else {
                    22
                }
            );
            if family < 16 {
                let low = members.iter().map(|v| v.knobs.root).min().unwrap();
                let high = members.iter().map(|v| v.knobs.root).max().unwrap();
                assert!(high - low >= 48);
                assert!(members.iter().any(|v| v.knobs.seconds == 2.0));
                assert!(members.iter().any(|v| v.knobs.seconds == 6.0));
            }
        }
        for method in [
            Stereo::Mono,
            Stereo::Decorrelated,
            Stereo::Haas,
            Stereo::WideChorus,
            Stereo::AutopanFree,
        ] {
            assert!(variants.iter().any(|v| v.knobs.stereo == method));
        }
    }

    fn validate_variant(bytes: &[u8], v: &Variant) -> Vec<f64> {
        let channels = v.knobs.stereo.channels();
        let label = format!("{} {}", v.id, v.name());
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(u32_at(bytes, 4) as usize + 8, bytes.len());
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u32_at(bytes, 16), 16);
        assert_eq!(u16_at(bytes, 20), 1);
        assert_eq!(u16_at(bytes, 22), channels);
        assert_eq!(u32_at(bytes, 24), RATE);
        assert_eq!(u32_at(bytes, 28), RATE * channels as u32 * 2);
        assert_eq!(u16_at(bytes, 32), channels * 2);
        assert_eq!(u16_at(bytes, 34), 16);
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32_at(bytes, 40) as usize, bytes.len() - 44);
        let samples = pcm(bytes);
        assert_eq!(
            samples.len(),
            (v.knobs.seconds * RATE as f64) as usize * channels as usize
        );
        let peak = samples.iter().fold(0.0f64, |p, s| p.max(s.abs()));
        assert!((0.85..=0.90).contains(&peak), "{label}: {peak}");
        assert!(samples.iter().all(|s| s.is_finite() && s.abs() < 1.0));
        for channel in 0..channels as usize {
            let channel: Vec<_> = samples
                .iter()
                .skip(channel)
                .step_by(channels as usize)
                .copied()
                .collect();
            let rms = (channel.iter().map(|s| s * s).sum::<f64>() / channel.len() as f64).sqrt();
            assert!(rms > 0.005, "{label}: rms={rms}");
            assert!(
                (channel.iter().sum::<f64>() / channel.len() as f64).abs() < 1.0 / 32767.0,
                "{label}: DC"
            );
            assert_eq!(channel[0], 0.0, "{label}");
            assert_eq!(channel.last(), Some(&0.0), "{label}");
            assert!(
                channel[channel.len() - 32..].iter().all(|s| s.abs() < 0.01),
                "{label}: fade"
            );
        }
        if channels == 1 {
            return samples;
        }
        let (mut ll, mut rr, mut lr, mut mm, mut ss) = (0.0, 0.0, 0.0, 0.0, 0.0);
        let mid: Vec<_> = samples
            .as_chunks::<2>()
            .0
            .iter()
            .map(|frame| {
                let (l, r) = (frame[0], frame[1]);
                ll += l * l;
                rr += r * r;
                lr += l * r;
                mm += ((l + r) * 0.5).powi(2);
                ss += ((l - r) * 0.5).powi(2);
                (l + r) * 0.5
            })
            .collect();
        let correlation = lr / (ll * rr).sqrt();
        assert!(correlation < 0.995, "{label}: correlation={correlation}");
        assert!(mm.sqrt() >= 0.5 * ss.sqrt(), "{label}: mid/side");
        mid
    }

    #[test]
    fn every_family_has_deterministic_valid_audio_and_pitch() {
        let dir = TempDir::new();
        for v in variants().iter().filter(|v| {
            v.knobs.colour == 2
                || matches!(v.family, 12 | 17 | 20 | 21 | 22 | 23) && v.knobs.colour < 5
        }) {
            let path = dir.0.join("sample.wav");
            let samples = synthesize_variant(v);
            assert!(samples.iter().all(|s| s.is_finite()));
            write_wav(&path, &samples, v.knobs.stereo.channels()).unwrap();
            let first = fs::read(&path).unwrap();
            let mid = validate_variant(&first, v);
            write_wav(&path, &synthesize_variant(v), v.knobs.stereo.channels()).unwrap();
            assert_eq!(first, fs::read(&path).unwrap(), "{}", v.name());
            if v.root() >= 0 && v.knobs.colour == 2 {
                let expected = frequency(v.root());
                let actual = estimate_f0(&mid, expected);
                let cents = 1200.0 * (actual / expected).log2();
                assert!(cents.abs() < 25.0, "{}: {cents} cents", v.name());
            }
        }
    }

    #[test]
    #[ignore = "full bank checksum and quality sweep; run with --release --ignored"]
    fn full_bank_release_sweep() {
        if cfg!(debug_assertions) {
            panic!("run this sweep with --release");
        }
        let first = TempDir::new();
        let second = TempDir::new();
        for dir in [&first, &second] {
            let start = std::time::Instant::now();
            generate(&dir.0).unwrap();
            let elapsed = start.elapsed();
            eprintln!("full generation: {elapsed:?}");
            assert!(elapsed.as_secs_f64() < 60.0);
            assert_eq!(fs::read_dir(dir.0.join("sources")).unwrap().count(), 499);
        }
        let tsv = fs::read_to_string(first.0.join("generated.tsv")).unwrap();
        assert_eq!(
            tsv,
            fs::read_to_string(second.0.join("generated.tsv")).unwrap()
        );
        assert_eq!(tsv.lines().count(), 499);
        let variants = variants();
        let mut invalid = Vec::new();
        for id in 1..=499 {
            let path = format!("sources/{id:04}.wav");
            let bytes = fs::read(first.0.join(&path)).unwrap();
            assert_eq!(bytes, fs::read(second.0.join(path)).unwrap(), "id={id}");
            if id <= 16 {
                assert_eq!(checksum(&bytes), LEGACY_CHECKSUMS[id - 1], "id={id}");
            } else {
                if std::panic::catch_unwind(|| validate_variant(&bytes, &variants[id - 17]))
                    .is_err()
                {
                    invalid.push(id);
                }
                assert_eq!(tsv.lines().nth(id - 1).unwrap(), variants[id - 17].row());
            }
        }
        assert!(invalid.is_empty(), "invalid ids: {invalid:?}");
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
        generate_selection(&dir.0, &[]).unwrap();
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
