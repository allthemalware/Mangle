//! Audio → GUI realtime activity feed.
//!
//! The audio thread publishes a per-block snapshot of what each grain engine
//! is doing (live grains, read cursor, spawn counts, output peaks); the
//! editor reads it to drive the waveform overlay and the scrolling lanes
//! display. Everything is fixed-size and non-blocking on the audio side:
//! atomics for the accumulators and a single `try_lock` memcpy for the frame
//! (skipped on contention — the GUI just keeps the previous frame).

use mangle_dsp::engine::GrainView;
use std::sync::atomic::AtomicU32;
use std::sync::Mutex;

/// Grain-marker cap per engine per frame (aggregated across voices).
pub const FEED_GRAINS: usize = 64;

/// `EngineFrame::slot` value meaning "no slot resolved".
pub const NO_SLOT: u8 = 0xFF;

/// One engine's activity, captured at the end of a block.
#[derive(Clone, Copy)]
pub struct EngineFrame {
    pub grains: [GrainView; FEED_GRAINS],
    pub n_grains: u8,
    /// `GrainMode` step index of the last captured voice.
    pub mode: u8,
    /// Resolved engine gate of the last captured voice.
    pub gate: bool,
    /// Sample slot the cursor refers to (`NO_SLOT` = none).
    pub slot: u8,
    /// Read cursor per channel, 0..1 within the slot's full sample; NaN when
    /// no voice is sounding. Granular/Cloud: where the next grain spawns;
    /// Stretch: the scrub playhead; Spectral: the vocoder analysis playhead.
    pub cursor: [f32; 2],
}

impl Default for EngineFrame {
    fn default() -> Self {
        Self {
            grains: [GrainView::default(); FEED_GRAINS],
            n_grains: 0,
            mode: 0,
            gate: false,
            slot: NO_SLOT,
            cursor: [f32::NAN; 2],
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct FeedFrame {
    pub engines: [EngineFrame; 3],
    /// Sounding voice count.
    pub voices: u8,
}

#[derive(Default)]
pub struct EngineFeed {
    /// Whole-frame snapshot, overwritten once per block by the audio thread.
    /// Both sides only ever `try_lock`, so neither can stall the other.
    pub frame: Mutex<FeedFrame>,
    /// Grain spawns per engine since the GUI last drained
    /// (audio `fetch_add`, GUI `swap(0)`).
    pub spawns: [AtomicU32; 3],
    /// Per-engine block peak (pre master chain) as f32 bits, peak-held
    /// (audio `fetch_max`, GUI `swap(0)`). Valid only because peaks are
    /// non-negative: non-negative IEEE-754 floats order like their bits.
    pub level_bits: [AtomicU32; 3],
}
