//! DPDFNet Voice の DSP コア（OBS 非依存）。
//!
//! 移植元は VST3 版の JUCE 非依存部分:
//!   `VorbisStft` / `ModelMeta` / `DpdfNet` / `OrtRuntime`
//! と、`PluginProcessor` のホップスケジューラ。JUCE に依存しない設計だったため、
//! そのまま Rust へ写している。
//!
//! 48 kHz 専用。それ以外のサンプルレートではフィルタ側がパススルーする。

pub mod engine;
pub mod meta;
pub mod net;
pub mod ort;
pub mod params;
pub mod stft;

#[cfg(test)]
mod offline_tests;

/// 内部サンプルレート。モデルが 48 kHz 前提のため固定。
pub const INTERNAL_SAMPLE_RATE: f32 = 48000.0;
