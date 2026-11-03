//! Non-parameter plugin state. Build-order step 10 (core).
//!
//! Everything the host's param store can't carry lives here: the nine
//! drawable curves, the RANDOM per-unit locks and settings, and the sample
//! slot references. Serialized as a small versioned little-endian blob,
//! hex-encoded into nih-plug's string-keyed field store by the shell.
//!
//! Format v2 (v1 is the prefix up to and including the slot refs):
//! ```text
//! b"MNGL"  magic
//! u16      version (2)
//! 9 × 256 × f32   curves (Loop 1–3, LFO 1–6)
//! u32      lock bits (see ORDER below)
//! f32      random amount
//! f32      random complexity
//! u64      last random seed
//! 6 × slot ref: u8 present; if 1: u32 path len, utf-8 path, u64 hash
//! -- appended in v2 --
//! u64      instance seed (0 = not assigned yet)
//! u32      grain pool bits
//! u16      lfo pool bits
//! u16      source pool bits
//! u16      scale pool bits
//! u16      dest grain bits
//! u8       dest lfo bits
//! u8       dest loop-lfo bits
//! 6 × (f32 view start, f32 view end)  waveform zoom per slot
//! u8       embed samples (0/1)
//! ```
//! The format is **append-only**: the fields above are a fixed prefix in every
//! version, and later versions only append after them. `deserialize` therefore
//! upgrades older blobs (missing fields default) and tolerates newer ones
//! (unknown trailing bytes ignored), so saved patches survive a `VERSION` bump.
//!
//! Slot content hashes are BLAKE3 truncated to 8 bytes, produced by the
//! `mangle-loader` crate. The FNV helper below remains as a dependency-free
//! utility for tests and non-file hashing.

use crate::curve::Curve;
use crate::random::{DestPool, GrainParamPool, LfoParamPool, RandomLocks, RandomPools, SourcePool};

pub const MAGIC: [u8; 4] = *b"MNGL";
pub const VERSION: u16 = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct SlotRef {
    pub path: String,
    pub content_hash: u64,
}

/// All non-param state.
#[derive(Clone)]
pub struct ExtraState {
    /// Curves 0–2: Loop LFO 1–3. Curves 3–8: LFO 1–6.
    pub curves: [Curve; 9],
    pub locks: RandomLocks,
    pub random_amount: f32,
    pub random_complexity: f32,
    pub last_seed: u64,
    pub slot_refs: [Option<SlotRef>; 6],
    /// Per-instance random seed; 0 means not assigned yet.
    pub instance_seed: u64,
    pub pools: RandomPools,
    /// Waveform zoom `(start, end)` per slot, as fractions of the sample.
    pub slot_views: [(f32, f32); 6],
    /// Store sample audio in the state instead of only referencing files.
    pub embed_samples: bool,
}

impl Default for ExtraState {
    fn default() -> Self {
        Self {
            curves: [
                Curve::ramp(),
                Curve::ramp(),
                Curve::ramp(),
                Curve::sine(),
                Curve::sine(),
                Curve::sine(),
                Curve::sine(),
                Curve::sine(),
                Curve::sine(),
            ],
            locks: RandomLocks::default(),
            random_amount: 1.0,
            random_complexity: 0.6,
            last_seed: 0,
            slot_refs: [None, None, None, None, None, None],
            instance_seed: 0,
            pools: RandomPools::default(),
            slot_views: [(0.0, 1.0); 6],
            embed_samples: false,
        }
    }
}

// Lock bit order (LSB first): grain 0–2, loop_lfo 0–2, lfo 0–5, drift,
// noise, macros, matrix, loop_points.
fn pack_locks(l: &RandomLocks) -> u32 {
    let mut bits = 0u32;
    let mut i = 0;
    let mut push = |b: bool| {
        if b {
            bits |= 1 << i;
        }
        i += 1;
    };
    for g in l.grain {
        push(g);
    }
    for g in l.loop_lfo {
        push(g);
    }
    for g in l.lfo {
        push(g);
    }
    push(l.drift);
    push(l.noise);
    push(l.macros);
    push(l.matrix);
    push(l.loop_points);
    bits
}

fn unpack_locks(bits: u32) -> RandomLocks {
    let mut i = 0;
    let mut next = || {
        let b = bits & (1 << i) != 0;
        i += 1;
        b
    };
    RandomLocks {
        grain: [next(), next(), next()],
        loop_lfo: [next(), next(), next()],
        lfo: [next(), next(), next(), next(), next(), next()],
        drift: next(),
        noise: next(),
        macros: next(),
        matrix: next(),
        loop_points: next(),
    }
}

/// FNV-1a 64-bit hash, for tests and non-file data. Sample files are hashed
/// with BLAKE3 by mangle-loader.
pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

// ----------------------------------------------------------- writer ---

pub fn serialize(state: &ExtraState) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 2 + 9 * 256 * 4 + 64);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    for c in state.curves.iter() {
        for v in c.0.iter() {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out.extend_from_slice(&pack_locks(&state.locks).to_le_bytes());
    out.extend_from_slice(&state.random_amount.to_le_bytes());
    out.extend_from_slice(&state.random_complexity.to_le_bytes());
    out.extend_from_slice(&state.last_seed.to_le_bytes());
    for slot in state.slot_refs.iter() {
        match slot {
            None => out.push(0),
            Some(r) => {
                out.push(1);
                let bytes = r.path.as_bytes();
                out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                out.extend_from_slice(bytes);
                out.extend_from_slice(&r.content_hash.to_le_bytes());
            }
        }
    }
    out.extend_from_slice(&state.instance_seed.to_le_bytes());
    out.extend_from_slice(&state.pools.grain.to_bits().to_le_bytes());
    out.extend_from_slice(&state.pools.lfo.to_bits().to_le_bytes());
    out.extend_from_slice(&state.pools.sources.to_bits().to_le_bytes());
    out.extend_from_slice(&state.pools.scales.to_bits().to_le_bytes());
    let (dg, dl, dll) = state.pools.dests.to_bits();
    out.extend_from_slice(&dg.to_le_bytes());
    out.push(dl);
    out.push(dll);
    for (s, e) in state.slot_views.iter() {
        out.extend_from_slice(&s.to_le_bytes());
        out.extend_from_slice(&e.to_le_bytes());
    }
    out.push(state.embed_samples as u8);
    out
}

// ----------------------------------------------------------- reader ---

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.data.len() {
            return None;
        }
        let s = &self.data[self.pos..end];
        self.pos = end;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
}

pub fn deserialize(data: &[u8]) -> Option<ExtraState> {
    let mut r = Reader { data, pos: 0 };
    if r.take(4)? != MAGIC {
        return None;
    }
    let version = r.u16()?;
    if version == 0 {
        return None; // 0 is never a valid version
    }
    // Append-only versioned format: the v1 fields below are a fixed prefix in
    // every version >= 1, and later versions only *append* after them (never
    // reorder or resize existing fields). So a format bump is non-destructive:
    //   * newer code reading an older blob keeps defaults for the appended
    //     fields it doesn't find (the `version >=` gates at the end), and
    //   * older code reading a newer blob reads the prefix it understands and
    //     ignores the trailing bytes.
    // To add state: bump VERSION, append the writes in `serialize`, and append
    // a matching `if version >= N { … }` read block at the end of this function.
    let mut state = ExtraState::default();
    for c in state.curves.iter_mut() {
        for v in c.0.iter_mut() {
            let x = r.f32()?;
            *v = if x.is_finite() {
                x.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
    }
    state.locks = unpack_locks(r.u32()?);
    let amt = r.f32()?;
    state.random_amount = if amt.is_finite() {
        amt.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let cmplx = r.f32()?;
    state.random_complexity = if cmplx.is_finite() {
        cmplx.clamp(0.0, 1.0)
    } else {
        0.6
    };
    state.last_seed = r.u64()?;
    for slot in state.slot_refs.iter_mut() {
        *slot = match r.u8()? {
            0 => None,
            _ => {
                let len = r.u32()? as usize;
                if len > 16_384 {
                    return None; // sanity cap on path length
                }
                let path = String::from_utf8(r.take(len)?.to_vec()).ok()?;
                let content_hash = r.u64()?;
                Some(SlotRef { path, content_hash })
            }
        };
    }
    if version >= 2 {
        state.instance_seed = r.u64()?;
        state.pools.grain = GrainParamPool::from_bits(r.u32()?);
        state.pools.lfo = LfoParamPool::from_bits(r.u16()?);
        state.pools.sources = SourcePool::from_bits(r.u16()?);
        state.pools.scales = SourcePool::from_bits(r.u16()?);
        let dg = r.u16()?;
        let dl = r.u8()?;
        let dll = r.u8()?;
        state.pools.dests = DestPool::from_bits(dg, dl, dll);
        for view in state.slot_views.iter_mut() {
            let (s, e) = (r.f32()?, r.f32()?);
            let valid = s.is_finite() && e.is_finite() && 0.0 <= s && s < e && e <= 1.0;
            *view = if valid { (s, e) } else { (0.0, 1.0) };
        }
        state.embed_samples = r.u8()? != 0;
    }
    Some(state)
}

// -------------------------------------------------------------- hex ---

pub fn hex_encode(data: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 0xF) as usize] as char);
    }
    s
}

// `usize::is_multiple_of` would be cleaner but needs Rust 1.87; the crate keeps
// a lower MSRV (see the blake3 pin), so the explicit modulo stays; likewise
// `as_chunks` is too new, so `chunks_exact(2)` stays.
#[allow(clippy::manual_is_multiple_of, clippy::chunks_exact_to_as_chunks)]
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let nib = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    for pair in b.chunks_exact(2) {
        out.push(nib(pair[0])? << 4 | nib(pair[1])?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_full_state() {
        let mut st = ExtraState::default();
        st.curves[0] = Curve::random_walk(7);
        st.curves[8].invert();
        st.locks.grain[1] = true;
        st.locks.matrix = true;
        st.random_amount = 0.42;
        st.random_complexity = 0.9;
        st.last_seed = 0xDEAD_BEEF_CAFE_F00D;
        st.slot_refs[2] = Some(SlotRef {
            path: "/home/voyd/samples/drone — Ω.flac".to_string(),
            content_hash: 12345,
        });

        st.instance_seed = 0x0123_4567_89AB_CDEF;
        st.pools.grain.0[0] = !st.pools.grain.0[0];
        st.pools.lfo.0[3] = !st.pools.lfo.0[3];
        st.pools.sources.noise = true;
        st.pools.scales.key = true;
        st.pools.dests.grain[0] = !st.pools.dests.grain[0];
        st.pools.dests.lfo[0] = true;
        st.pools.dests.loop_lfo[3] = true;
        st.slot_views[1] = (0.25, 0.75);
        st.slot_views[5] = (0.1, 0.2);
        st.embed_samples = true;

        let bytes = serialize(&st);
        let back = deserialize(&bytes).expect("must deserialize");
        assert_eq!(back.instance_seed, st.instance_seed);
        assert_eq!(back.pools, st.pools);
        assert_ne!(back.pools, RandomPools::default());
        assert_eq!(back.slot_views, st.slot_views);
        assert!(back.embed_samples);
        for (a, b) in st.curves.iter().zip(back.curves.iter()) {
            assert_eq!(a.0[..], b.0[..]);
        }
        assert_eq!(back.locks, st.locks);
        assert_eq!(back.random_amount, st.random_amount);
        assert_eq!(back.random_complexity, st.random_complexity);
        assert_eq!(back.last_seed, st.last_seed);
        assert_eq!(back.slot_refs[2], st.slot_refs[2]);
        assert_eq!(back.slot_refs[0], None);
    }

    #[test]
    fn rejects_bad_magic_and_truncation() {
        let st = ExtraState::default();
        let mut bytes = serialize(&st);
        assert!(deserialize(&bytes[..bytes.len() - 3]).is_none());
        bytes[0] = b'X';
        assert!(deserialize(&bytes).is_none());
        assert!(deserialize(&[]).is_none());
    }

    #[test]
    fn loads_newer_version_keeping_the_known_prefix() {
        // A hypothetical future v3 blob: a valid v2 payload with the version
        // bumped to 3 and extra trailing bytes (fields this code can't read).
        // Every v1 and v2 field must still load; the trailing bytes are ignored.
        let mut st = ExtraState::default();
        st.curves[3] = Curve::random_walk(9);
        st.locks.lfo[2] = true;
        st.random_amount = 0.33;
        st.last_seed = 0x1234_5678_9ABC_DEF0;
        st.slot_refs[1] = Some(SlotRef {
            path: "/x/y.wav".to_string(),
            content_hash: 99,
        });

        st.instance_seed = 77;
        st.pools.sources.key = true;
        st.slot_views[0] = (0.5, 0.6);
        st.embed_samples = true;

        let mut bytes = serialize(&st);
        bytes[4] = 3; // version u16 (LE) low byte: 2 -> 3
        bytes.extend_from_slice(&[0xAA; 16]); // appended "v3" data, unknown to us

        let back = deserialize(&bytes).expect("a newer-version blob must still load");
        assert_eq!(back.curves[3].0[..], st.curves[3].0[..]);
        assert_eq!(back.locks, st.locks);
        assert!((back.random_amount - 0.33).abs() < 1e-6);
        assert_eq!(back.last_seed, st.last_seed);
        assert_eq!(back.slot_refs[1], st.slot_refs[1]);
        assert_eq!(back.instance_seed, 77);
        assert_eq!(back.pools, st.pools);
        assert_eq!(back.slot_views, st.slot_views);
        assert!(back.embed_samples);
    }

    #[test]
    fn v1_blob_loads_with_v2_defaults() {
        let mut st = ExtraState {
            last_seed: 5,
            instance_seed: 9,
            embed_samples: true,
            ..Default::default()
        };
        st.slot_views[0] = (0.2, 0.3);
        st.slot_refs[0] = Some(SlotRef {
            path: "/a.wav".to_string(),
            content_hash: 1,
        });
        let mut bytes = serialize(&st);
        let v2_len = 8 + 4 + 2 + 2 + 2 + 2 + 1 + 1 + 6 * 8 + 1;
        bytes.truncate(bytes.len() - v2_len);
        bytes[4] = 1;
        bytes[5] = 0;
        let back = deserialize(&bytes).expect("v1 blob must load");
        assert_eq!(back.last_seed, 5);
        assert_eq!(back.slot_refs[0], st.slot_refs[0]);
        assert_eq!(back.instance_seed, 0);
        assert_eq!(back.pools, RandomPools::default());
        assert_eq!(back.slot_views, [(0.0, 1.0); 6]);
        assert!(!back.embed_samples);
    }

    #[test]
    fn bad_views_sanitize_to_full_range() {
        let st = ExtraState {
            slot_views: [
                (f32::NAN, 1.0),
                (0.0, f32::INFINITY),
                (0.5, 0.5),
                (0.8, 0.2),
                (-0.1, 0.5),
                (0.2, 1.5),
            ],
            ..Default::default()
        };
        let back = deserialize(&serialize(&st)).unwrap();
        assert_eq!(back.slot_views, [(0.0, 1.0); 6]);
    }

    #[test]
    fn rejects_version_zero() {
        let mut bytes = serialize(&ExtraState::default());
        bytes[4] = 0;
        bytes[5] = 0;
        assert!(deserialize(&bytes).is_none());
    }

    #[test]
    fn lock_bits_roundtrip_all_patterns() {
        for bits in [0u32, 0x1FFFF, 0b1_0101_0101_0101_0101, 0x0F0F0] {
            let l = unpack_locks(bits);
            assert_eq!(pack_locks(&l), bits & 0x1FFFF);
        }
    }

    #[test]
    fn hex_roundtrip() {
        let data: Vec<u8> = (0..=255).collect();
        let s = hex_encode(&data);
        assert_eq!(hex_decode(&s).unwrap(), data);
        assert!(hex_decode("abc").is_none());
        assert!(hex_decode("zz").is_none());
        assert_eq!(hex_decode("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn content_hash_is_stable_and_sensitive() {
        let a = content_hash(b"mangle");
        assert_eq!(a, content_hash(b"mangle"));
        assert_ne!(a, content_hash(b"mangle "));
        assert_ne!(content_hash(b""), 0);
    }

    #[test]
    fn deserialize_sanitizes_non_finite_values() {
        let mut bytes = serialize(&ExtraState::default());
        // Byte 6 starts curve 0 sample 0; the amount f32 follows 9*256 curve
        // samples and the u32 lock bits.
        bytes[6..10].copy_from_slice(&f32::NAN.to_le_bytes());
        let amount_at = 6 + 9 * 256 * 4 + 4;
        bytes[amount_at..amount_at + 4].copy_from_slice(&f32::INFINITY.to_le_bytes());
        let st = deserialize(&bytes).expect("blob still parses");
        assert!(st.curves.iter().all(|c| c.0.iter().all(|v| v.is_finite())));
        assert_eq!(st.curves[0].0[0], 0.0);
        assert_eq!(st.random_amount, 1.0);
    }
}
