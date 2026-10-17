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
/// `model` — 使うモデル。"auto" / "dp8" / "dp2"（`paths::ModelChoice`）。
pub const MODEL: &CStr = c"model";
/// `vuRef` — VU メーターの基準（0 VU = 何 dBFS）。"20" / "18" / "14"。
pub const VU_REF: &CStr = c"vuRef";

pub const AMBIENCE_MIN: f64 = 0.0;
pub const AMBIENCE_MAX: f64 = 1.0;
pub const AMBIENCE_DEFAULT: f64 = 0.5;

pub const VOICE_MIN: f64 = -12.0;
pub const VOICE_MAX: f64 = 12.0;
pub const VOICE_DEFAULT: f64 = 0.0;

pub const VOICE_REVERB_MIN: f64 = 0.0;
pub const VOICE_REVERB_MAX: f64 = 1.0;
pub const VOICE_REVERB_DEFAULT: f64 = 0.0;

/// 既定のモデル（`paths::ModelChoice::Auto`）。
pub const MODEL_DEFAULT: &CStr = c"auto";
/// 既定の VU 基準（0 VU = -18 dBFS、EBU）。
pub const VU_REF_DEFAULT: &CStr = c"18";

/// VU メーターの基準値 = 「0 VU が何 dBFS か」。
///
/// 実機の VU は 0 VU = +4 dBu を基準にするが、ディジタルのフルスケールは
/// 規格ごとに違う値へ対応させる:
///   * `Db20` … SMPTE / 日本の業務基準 (0 VU = -20 dBFS)
///   * `Db18` … EBU (0 VU = -18 dBFS) — 既定
///   * `Db14` … 配信向けにラウド側を細かく見たいとき
///
/// 目盛り全体（-20 VU 〜 +3 VU）は基準と一緒に平行移動する。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum VuRef {
    Db20,
    #[default]
    Db18,
    Db14,
}

impl VuRef {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Db20 => "20",
            Self::Db18 => "18",
            Self::Db14 => "14",
        }
    }

    pub fn from_setting(s: &str) -> Self {
        match s {
            "20" => Self::Db20,
            "14" => Self::Db14,
            _ => Self::Db18,
        }
    }

    /// 0 VU に当たる dBFS（正の数で保持する）。
    pub fn dbfs(self) -> f32 {
        match self {
            Self::Db20 => 20.0,
            Self::Db18 => 18.0,
            Self::Db14 => 14.0,
        }
    }

    /// アトミックに詰めるための符号（そのまま dBFS の値）。
    pub fn code(self) -> u8 {
        match self {
            Self::Db20 => 20,
            Self::Db18 => 18,
            Self::Db14 => 14,
        }
    }

    pub fn from_code(c: u8) -> Self {
        match c {
            20 => Self::Db20,
            14 => Self::Db14,
            _ => Self::Db18,
        }
    }

    /// 全選択肢（プロパティのドロップダウン用）。
    pub const ALL: [Self; 3] = [Self::Db18, Self::Db20, Self::Db14];

    /// プロパティに出す表示名。
    pub fn label(self) -> &'static str {
        match self {
            Self::Db20 => "0 VU = -20 dBFS (SMPTE / 日本)",
            Self::Db18 => "0 VU = -18 dBFS (EBU)",
            Self::Db14 => "0 VU = -14 dBFS (ラウド)",
        }
    }
}

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

    #[test]
    fn vu_ref_round_trips() {
        for r in VuRef::ALL {
            assert_eq!(VuRef::from_setting(r.as_str()), r);
            assert_eq!(VuRef::from_code(r.code()), r);
        }
    }

    #[test]
    fn unknown_vu_ref_falls_back_to_ebu() {
        // OBS はキーが無いとき空文字を返すので、必ず既定 (EBU) に落ちること。
        assert_eq!(VuRef::from_setting(""), VuRef::Db18);
        assert_eq!(VuRef::from_setting("-18"), VuRef::Db18);
        assert_eq!(VuRef::from_code(0), VuRef::Db18);
        assert_eq!(VuRef::default(), VuRef::Db18);
    }

    #[test]
    fn vu_ref_dbfs_is_monotonic() {
        assert!(VuRef::Db20.dbfs() > VuRef::Db18.dbfs());
        assert!(VuRef::Db18.dbfs() > VuRef::Db14.dbfs());
    }
}
