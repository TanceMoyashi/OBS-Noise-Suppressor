//! VST3 のパラメータ。
//!
//! 意味と範囲は OBS 版とまったく同じ (`dpdfnet::params` の定数をそのまま使う)。
//! 違うのは ID の付け方だけ: VST3 ではホストが ID で保存するので、OBS の設定
//! キー (`ambience` / `voice` / …) と同じ文字列を使っておく。
//!
//! * つまみ 3 つ（Ambience / Voice / Voice Reverb）とモード 3 つは自動化できる
//! * モデルと推論デバイスは**自動化しない**（音声スレッドで組み直せないため、
//!   変わったときだけ `process` の先頭で組み直す。数 100 ms 音が途切れる）

use std::sync::Arc;

use nih_plug::prelude::*;

use dpdfnet::engine::ParamSnapshot;
use dpdfnet::params as dsp;
use dpdfnet::paths::{DeviceChoice, ModelChoice};

/// フェーダーごとのミュート / バイパス（`dsp::FeatureMode` と同じ意味）。
#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureKind {
    /// つまみの値を使う。
    #[id = "active"]
    #[name = "有効（つまみの値）"]
    Active,
    /// 最小値（最大抑圧 / presence 最小 / 残響を残さない）。
    #[id = "mute"]
    #[name = "ミュート（最小）"]
    Mute,
    /// 中立値（その機能を掛けない）。
    #[id = "bypass"]
    #[name = "バイパス（中立）"]
    Bypass,
}

impl FeatureKind {
    pub fn to_dsp(self) -> dsp::FeatureMode {
        match self {
            Self::Active => dsp::FeatureMode::Active,
            Self::Mute => dsp::FeatureMode::Mute,
            Self::Bypass => dsp::FeatureMode::Bypass,
        }
    }

    /// パネル (`panel_ui`) から来た値を戻す。`to_dsp` の逆。
    pub fn from_dsp(m: dsp::FeatureMode) -> Self {
        match m {
            dsp::FeatureMode::Active => Self::Active,
            dsp::FeatureMode::Mute => Self::Mute,
            dsp::FeatureMode::Bypass => Self::Bypass,
        }
    }
}

/// 使うモデル（OBS 版の `model` 設定と同じ 3 択）。
#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    /// 自動（品質優先。重ければ実測で軽量に落ちる）。
    #[id = "auto"]
    #[name = "自動"]
    Auto,
    /// 高品質 (`dpdfnet8`)。
    #[id = "dp8"]
    #[name = "高品質 (dp8)"]
    Quality,
    /// 軽量・低負荷 (`dpdfnet2`)。
    #[id = "dp2"]
    #[name = "軽量 (dp2)"]
    Fast,
}

impl ModelKind {
    pub fn to_dsp(self) -> ModelChoice {
        match self {
            Self::Auto => ModelChoice::Auto,
            Self::Quality => ModelChoice::Quality,
            Self::Fast => ModelChoice::Fast,
        }
    }

    /// パネル (`panel_ui`) の MODEL キーから来た値を戻す。`to_dsp` の逆。
    pub fn from_dsp(c: ModelChoice) -> Self {
        match c {
            ModelChoice::Auto => Self::Auto,
            ModelChoice::Quality => Self::Quality,
            ModelChoice::Fast => Self::Fast,
        }
    }
}

/// 推論デバイス（GPU = DirectML。実測で CPU より遅いので既定は CPU）。
#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    #[id = "cpu"]
    #[name = "CPU"]
    Cpu,
    #[id = "gpu"]
    #[name = "GPU (DirectML)"]
    Gpu,
}

impl DeviceKind {
    pub fn to_dsp(self) -> DeviceChoice {
        match self {
            Self::Cpu => DeviceChoice::Cpu,
            Self::Gpu => DeviceChoice::Gpu,
        }
    }

    /// パネル (`panel_ui`) の DEV キーから来た値を戻す。`to_dsp` の逆。
    pub fn from_dsp(c: DeviceChoice) -> Self {
        match c {
            DeviceChoice::Cpu => Self::Cpu,
            DeviceChoice::Gpu => Self::Gpu,
        }
    }
}

/// 0..1 を「0 % 〜 100 %」で見せる。
fn percent(v: f32) -> String {
    format!("{:.0} %", v * 100.0)
}

/// 「0 %」〜「100 %」を 0..1 に戻す。
fn parse_percent(s: &str) -> Option<f32> {
    let v: f32 = s.trim().trim_end_matches('%').trim().parse().ok()?;
    Some(v / 100.0)
}

#[derive(Params)]
pub struct DpdfnetParams {
    /// 残す暗騒音。0 % = 最大抑圧、100 % = そのまま。
    #[id = "ambience"]
    pub ambience: FloatParam,

    /// presence（±12 dB）。
    #[id = "voice"]
    pub voice: FloatParam,

    /// 残す残響。0 % = 残さない、100 % = そのまま。
    #[id = "voiceReverb"]
    pub voice_reverb: FloatParam,

    #[id = "ambienceMode"]
    pub ambience_mode: EnumParam<FeatureKind>,

    #[id = "voiceMode"]
    pub voice_mode: EnumParam<FeatureKind>,

    #[id = "voiceReverbMode"]
    pub voice_reverb_mode: EnumParam<FeatureKind>,

    /// 推論に使うモデル。**自動化しない**（変更は次の組み直しで効く）。
    #[id = "model"]
    pub model: EnumParam<ModelKind>,

    /// 推論デバイス。**自動化しない**（同上）。
    #[id = "inferenceDevice"]
    pub device: EnumParam<DeviceKind>,

    /// 全体のバイパス。パネルの BYPASS キー。オンにすると何もせず素通しする
    /// （自動化できる。他のパラメータと同じくホストの汎用表示からも変えられる）。
    #[id = "bypass"]
    pub bypass: BoolParam,
}

impl Default for DpdfnetParams {
    fn default() -> Self {
        Self {
            ambience: FloatParam::new(
                "Ambience",
                dsp::AMBIENCE_DEFAULT as f32,
                FloatRange::Linear {
                    min: dsp::AMBIENCE_MIN as f32,
                    max: dsp::AMBIENCE_MAX as f32,
                },
            )
            .with_step_size(0.01)
            .with_value_to_string(Arc::new(percent))
            .with_string_to_value(Arc::new(parse_percent)),

            voice: FloatParam::new(
                "Voice",
                dsp::VOICE_DEFAULT as f32,
                FloatRange::Linear {
                    min: dsp::VOICE_MIN as f32,
                    max: dsp::VOICE_MAX as f32,
                },
            )
            .with_step_size(0.1)
            .with_unit(" dB"),

            voice_reverb: FloatParam::new(
                "Voice Reverb",
                dsp::VOICE_REVERB_DEFAULT as f32,
                FloatRange::Linear {
                    min: dsp::VOICE_REVERB_MIN as f32,
                    max: dsp::VOICE_REVERB_MAX as f32,
                },
            )
            .with_step_size(0.01)
            .with_value_to_string(Arc::new(percent))
            .with_string_to_value(Arc::new(parse_percent)),

            ambience_mode: EnumParam::new("Ambience", FeatureKind::Active),
            voice_mode: EnumParam::new("Voice", FeatureKind::Active),
            voice_reverb_mode: EnumParam::new("Voice Reverb", FeatureKind::Active),

            model: EnumParam::new("Model", ModelKind::Auto).non_automatable(),
            device: EnumParam::new("Inference Device", DeviceKind::Cpu).non_automatable(),

            // 既定は「有効」（= バイパスしない）。OBS 版のフィルタ有効と同じ。
            bypass: BoolParam::new("Bypass", false),
        }
    }
}

impl DpdfnetParams {
    /// 音声スレッドが読むスナップショット。
    pub fn snapshot(&self) -> ParamSnapshot {
        ParamSnapshot {
            ambience: self.ambience.value(),
            voice_db: self.voice.value(),
            voice_reverb: self.voice_reverb.value(),
            ambience_mode: self.ambience_mode.value().to_dsp(),
            voice_mode: self.voice_mode.value().to_dsp(),
            voice_reverb_mode: self.voice_reverb_mode.value().to_dsp(),
        }
    }

    pub fn model_choice(&self) -> ModelChoice {
        self.model.value().to_dsp()
    }

    pub fn device_choice(&self) -> DeviceChoice {
        self.device.value().to_dsp()
    }

    /// バイパス中か（= 全体が無効）。
    pub fn bypassed(&self) -> bool {
        self.bypass.value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_snapshot_matches_the_obs_defaults() {
        let p = DpdfnetParams::default();
        let s = p.snapshot();
        let d = ParamSnapshot::default();
        assert_eq!(s.ambience, d.ambience);
        assert_eq!(s.voice_db, d.voice_db);
        assert_eq!(s.voice_reverb, d.voice_reverb);
        assert_eq!(s.ambience_mode, d.ambience_mode);
        assert_eq!(s.voice_mode, d.voice_mode);
        assert_eq!(s.voice_reverb_mode, d.voice_reverb_mode);
        // 既定は「自動」「CPU」（OBS 版と同じ）。
        assert_eq!(p.model_choice(), ModelChoice::Auto);
        assert_eq!(p.device_choice(), DeviceChoice::Cpu);
        // 既定はバイパスしない（= 有効）。
        assert!(!p.bypassed());
    }

    #[test]
    fn percent_round_trips() {
        assert_eq!(percent(0.5), "50 %");
        assert_eq!(parse_percent("50 %"), Some(0.5));
        assert_eq!(parse_percent("100%"), Some(1.0));
        assert_eq!(parse_percent("0.5"), Some(0.005));
        assert_eq!(parse_percent(""), None);
    }

    #[test]
    fn feature_kinds_map_to_the_obs_modes() {
        assert_eq!(FeatureKind::Active.to_dsp(), dsp::FeatureMode::Active);
        assert_eq!(FeatureKind::Mute.to_dsp(), dsp::FeatureMode::Mute);
        assert_eq!(FeatureKind::Bypass.to_dsp(), dsp::FeatureMode::Bypass);
    }

    #[test]
    fn model_and_device_kinds_cover_all_choices() {
        for (k, c) in [
            (ModelKind::Auto, ModelChoice::Auto),
            (ModelKind::Quality, ModelChoice::Quality),
            (ModelKind::Fast, ModelChoice::Fast),
        ] {
            assert_eq!(k.to_dsp(), c);
        }
        assert_eq!(DeviceKind::Cpu.to_dsp(), DeviceChoice::Cpu);
        assert_eq!(DeviceKind::Gpu.to_dsp(), DeviceChoice::Gpu);
    }

    #[test]
    fn kinds_round_trip_through_dsp() {
        for k in [FeatureKind::Active, FeatureKind::Mute, FeatureKind::Bypass] {
            assert_eq!(FeatureKind::from_dsp(k.to_dsp()), k);
        }
        for k in [ModelKind::Auto, ModelKind::Quality, ModelKind::Fast] {
            assert_eq!(ModelKind::from_dsp(k.to_dsp()), k);
        }
        for k in [DeviceKind::Cpu, DeviceKind::Gpu] {
            assert_eq!(DeviceKind::from_dsp(k.to_dsp()), k);
        }
    }
}
