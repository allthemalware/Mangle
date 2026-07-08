//! Mangle DSP core.
//!
//! Dependency-free so it compiles and tests anywhere; the `mangle` crate
//! wraps this in a nih-plug CLAP shell. Source of truth for ranges and IDs
//! is `mangle-parameter-map.md` in the repo root.

pub mod curve;
pub mod engine;
pub mod fft;
pub mod filter;
pub mod grain;
pub mod matrix;
pub mod modengine;
pub mod params;
pub mod random;
pub mod resolve;
pub mod rng;
pub mod sample;
pub mod spectral;
pub mod squeeze;
pub mod state;
pub mod voice;
