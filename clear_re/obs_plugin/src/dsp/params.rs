//! パラメータの ID と変換。
//!
//! 移植元: `clear_re/plugin/src/Parameters.h` と `PluginProcessor::updateParameters`。
//! 「Ambience」は残す環境ノイズ量で、公式オフライン実装の attenuation limit と
//! 同じ定義（noisy 側の重み α = 10^(-L/20)）を使う。

use std::ffi::CStr;

/// `ambience` 0..1。大きいほど抑圧が弱い（暗騒音を残す）。
pub const AMBIENCE: &CStr = c"ambience";
/// `voice` -12..+12 dB。presence / gain。
pub const VOICE: &CStr = c"voice";
/// `voiceReverb` 0..1。残す残響量（1 = そのまま）。
pub const VOICE_REVERB: &CStr = c"voiceReverb";

pub const AMBIENCE_MIN: f64 = 0.0;
pub const AMBIENCE_MAX: f64 = 1.0;
pub const AMBIENCE_DEFAULT: f64 = 0.5;

pub const VOICE_MIN: f64 = -12.0;
pub const VOICE_MAX: f64 = 12.0;
pub const VOICE_DEFAULT: f64 = 0.0;

pub const VOICE_REVERB_MIN: f64 = 0.0;
pub const VOICE_REVERB_MAX: f64 = 1.0;
pub const VOICE_REVERB_DEFAULT: f64 = 0.0;

/// 抑圧リミット [dB]。ambience == 1.0 で最小、0.0 で最大。
pub const MIN_ATTENUATION_DB: f32 = 6.0;
pub const MAX_ATTENUATION_DB: f32 = 48.0;

/// パラメータ平滑化の係数（1 ホップごと）。
pub const SMOOTHING_K: f32 = 0.3;

/// ambience(0..1) -> 抑圧リミット [dB]。
///
/// `juce::jmap (amb, 1.0, 0.0, minAttenuationDb, maxAttenuationDb)` と同じ。
#[inline]
pub fn ambience_to_attenuation_db(ambience01: f32) -> f32 {
    let a = ambience01.clamp(0.0, 1.0);
    MIN_ATTENUATION_DB + (1.0 - a) * (MAX_ATTENUATION_DB - MIN_ATTENUATION_DB)
}

/// 抑圧リミット [dB] -> noisy 側の重み α。公式実装と同じ定義。
#[inline]
pub fn attenuation_to_noisy_weight(limit_db: f32) -> f32 {
    10.0f32.powf(-limit_db / 20.0)
}

#[inline]
pub fn decibels_to_gain(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambience_maps_to_expected_limits() {
        assert!((ambience_to_attenuation_db(1.0) - 6.0).abs() < 1e-6);
        assert!((ambience_to_attenuation_db(0.0) - 48.0).abs() < 1e-6);
        assert!((ambience_to_attenuation_db(0.5) - 27.0).abs() < 1e-6);
    }

    #[test]
    fn default_ambience_gives_about_minus_27db() {
        let w = attenuation_to_noisy_weight(ambience_to_attenuation_db(0.5));
        assert!((w - 10f32.powf(-27.0 / 20.0)).abs() < 1e-9);
    }
}
