//! DPDFNet の DSP コア（ホスト非依存）。
//!
//! 移植元は VST3 版の JUCE 非依存部分:
//!   `VorbisStft` / `ModelMeta` / `DpdfNet` / `OrtRuntime`
//! と、`PluginProcessor` のホップスケジューラ。JUCE に依存しない設計だったため、
//! そのまま Rust へ写している。
//!
//! モデルは 48 kHz 前提なので、内側は常に 48 kHz で回す。ホストが別のレートで
//! 動いているときは `resample` が入口と出口で変換する（`engine::build` に
//! レートを渡すと自動で挟まる）。48 kHz のときは変換段を作らないので、
//! 従来の経路がそのまま使われる。
//!
//! # ホストとの境界
//!
//! このクレートは OBS にも nih-plug にも依存しない。ホスト固有のものは 2 つだけで、
//! どちらも外から差し込む:
//!
//!   * **ログ**: `log::set_sink` に `fn(Level, &str)` を渡す（未設定なら黙る）
//!   * **探索先**: `engine::build(..., resource_dirs)` と `paths` の環境変数 /
//!     自分の DLL の隣。ホストは `resource_dirs` に自前の置き場を足す
//!
//! OBS フィルタ（`clear_re/obs_plugin`）と VST3 版（`clear_re/vst3`）が
//! このクレートを共有している。

pub mod engine;
pub mod log;
pub mod meta;
pub mod net;
pub mod ort;
pub mod params;
pub mod paths;
pub mod resample;
pub mod stft;

#[cfg(test)]
mod offline_tests;

/// 内部サンプルレート。モデルが 48 kHz 前提のため固定。
pub const INTERNAL_SAMPLE_RATE: f32 = 48000.0;
