//! VST3 のエディタ（パネル）。共通クレート [`panel_ui`] のホスト結線。
//!
//! DAW が渡してくる親ウィンドウの**子ウィンドウ**としてパネルを作る。
//! メッセージループはホスト（DAW）のものを使うので、`panel_ui` が仕掛ける
//! `WM_TIMER`（40 ms）で [`Panel::tick`] を回し、**変化があったときだけ**
//! 描き直す。
//!
//! パネルのつまみ / MUTE / BYP / BYPASS / MODEL / DEV は [`ParamSetter`] で
//! パラメータへ書き込む（つまみとミュート系は自動化にも乗る）。モデル /
//! デバイスは非自動化パラメータで、変更は次の `process` の先頭で拾って
//! エンジンを組み直す（OBS 版と同じく数 100 ms 音が途切れる）。
//!
//! パネルは音声スレッドからは触らない。描画・入力はホストの UI スレッドだけ。

use std::any::Any;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use nih_plug::prelude::*;

use dpdfnet::engine::Meters;
use dpdfnet::params::VuRef;

use panel_ui::{Config, Event, Host, Panel, Parent, State};

use crate::core::EditorShared;
use crate::params::{DeviceKind, DpdfnetParams, FeatureKind, ModelKind};

/// VST3 のエディタ。`Plugin::editor()` が返す。
pub struct DpdfnetEditor {
    params: Arc<DpdfnetParams>,
    meters: Arc<Meters>,
    shared: Arc<EditorShared>,
    /// ホストが要求した DPI 倍率。`f32` のビット列で持つ（`Editor: Send` と
    /// `&self` の両方から触るため）。
    scale: AtomicU32,
}

impl DpdfnetEditor {
    pub fn new(params: Arc<DpdfnetParams>, meters: Arc<Meters>, shared: Arc<EditorShared>) -> Self {
        Self {
            params,
            meters,
            shared,
            scale: AtomicU32::new(1.0f32.to_bits()),
        }
    }

    fn scale_factor(&self) -> f32 {
        f32::from_bits(self.scale.load(Ordering::Relaxed))
    }
}

impl Editor for DpdfnetEditor {
    fn spawn(
        &self,
        parent: ParentWindowHandle,
        context: Arc<dyn GuiContext>,
    ) -> Box<dyn Any + Send> {
        // このプラグインは Windows 専用なので、親は HWND のはず。
        let ParentWindowHandle::Win32Hwnd(hwnd) = parent else {
            nih_error!("DPDFNet Voice: 対応していないウィンドウ環境です");
            return Box::new(());
        };

        let host = Vst3Host {
            params: Arc::clone(&self.params),
            meters: Arc::clone(&self.meters),
            shared: Arc::clone(&self.shared),
            context,
            dragging: Mutex::new(None),
        };

        let config = Config {
            // ホストの子ウィンドウ。ESC / 閉じるボタンはホストが扱う。
            parent: Parent::Child(hwnd),
            title: String::new(),
            scale: self.scale_factor(),
            preset_path: preset_path(),
            // MODEL / DEV はパネルのキーで選ぶ（OBS 版はプロパティ画面）。
            model_key: true,
            device_key: true,
        };

        match Panel::new(Box::new(host), config) {
            Ok(panel) => Box::new(EditorHandle {
                panel: Some(panel),
            }),
            Err(e) => {
                nih_error!("DPDFNet Voice: パネルを作れません: {e}");
                Box::new(())
            }
        }
    }

    fn size(&self) -> (u32, u32) {
        // 論理ピクセル。ホストが倍率を掛けて物理サイズにする。
        panel_ui::logical_size()
    }

    fn set_scale_factor(&self, factor: f32) -> bool {
        self.scale.store(factor.to_bits(), Ordering::Relaxed);
        true
    }

    // ホストの自動化 / 汎用表示からパラメータが変わっても、パネルは 40 ms
    // ごとの `tick` で値を引き直し、差分があったときだけ描き直す。だから
    // ここで描き直しを促す必要は無い。
    fn param_value_changed(&self, _id: &str, _normalized_value: f32) {}
    fn param_modulation_changed(&self, _id: &str, _modulation_offset: f32) {}
    fn param_values_changed(&self) {}
}

/// エディタの実体。ホストが `removed()` でこのハンドルを落としたときに
/// 子ウィンドウを破棄する。
struct EditorHandle {
    panel: Option<Box<Panel>>,
}

impl Drop for EditorHandle {
    fn drop(&mut self) {
        if let Some(mut p) = self.panel.take() {
            p.destroy();
        }
    }
}

/// パネル（`panel_ui`）とプラグインの境目。
struct Vst3Host {
    params: Arc<DpdfnetParams>,
    meters: Arc<Meters>,
    shared: Arc<EditorShared>,
    context: Arc<dyn GuiContext>,
    /// ドラッグ中のフェーダー。`begin_set_parameter` / `end_set_parameter` を
    /// 1 回ずつ対にするために覚えておく。
    dragging: Mutex<Option<usize>>,
}

impl Vst3Host {
    fn fader(&self, i: usize) -> &FloatParam {
        match i {
            0 => &self.params.ambience,
            1 => &self.params.voice,
            _ => &self.params.voice_reverb,
        }
    }
}

impl Host for Vst3Host {
    fn state(&self) -> State {
        let p = &self.params;
        let m = &self.meters;
        let sh = &self.shared;
        State {
            values: [p.ambience.value(), p.voice.value(), p.voice_reverb.value()],
            modes: [
                p.ambience_mode.value().to_dsp(),
                p.voice_mode.value().to_dsp(),
                p.voice_reverb_mode.value().to_dsp(),
            ],
            model: p.model_choice(),
            device: p.device_choice(),
            // VST3 版に VU 基準の設定は無いので、OBS 版の既定と同じ -18 dBFS。
            vref: VuRef::default().dbfs(),
            dereverb: sh.dereverb.load(Ordering::Relaxed),
            channels: sh.channels.load(Ordering::Relaxed),
            rate: sh.rate.load(Ordering::Relaxed),
            status: sh.status(),
            status_text: sh.status_text(),
            summary: sh.summary(),
            enabled: !p.bypassed(),
            seq: m.seq(),
            in_level: [m.in_level(0), m.in_level(1)],
            in_peak: [m.in_peak(0), m.in_peak(1)],
            out_level: [m.out_level(0), m.out_level(1)],
            out_peak: [m.out_peak(0), m.out_peak(1)],
            bands: [
                m.get(0, 0).max(m.get(1, 0)),
                m.get(0, 1).max(m.get(1, 1)),
                m.get(0, 2).max(m.get(1, 2)),
            ],
        }
    }

    fn event(&self, ev: Event) {
        let setter = ParamSetter::new(self.context.as_ref());
        match ev {
            Event::Fader { index, value } => {
                let param = self.fader(index);
                // ドラッグの最初に 1 度だけ begin を出す（ホストに「操作中」を
                // 伝える）。ホイールやダブルクリックは Fader → FaderCommit が
                // 続けて来るので begin / end が対になる。
                let need_begin = match self.dragging.lock() {
                    Ok(mut g) => {
                        let new = *g != Some(index);
                        *g = Some(index);
                        new
                    }
                    Err(_) => true,
                };
                if need_begin {
                    setter.begin_set_parameter(param);
                }
                setter.set_parameter(param, value);
            }
            Event::FaderCommit { index } => {
                let was_dragging = match self.dragging.lock() {
                    Ok(mut g) => g.take() == Some(index),
                    Err(_) => false,
                };
                if was_dragging {
                    setter.end_set_parameter(self.fader(index));
                }
            }
            Event::Mode { index, mode } => {
                let kind = FeatureKind::from_dsp(mode);
                match index {
                    0 => setter.set_parameter(&self.params.ambience_mode, kind),
                    1 => setter.set_parameter(&self.params.voice_mode, kind),
                    _ => setter.set_parameter(&self.params.voice_reverb_mode, kind),
                }
            }
            Event::ToggleBypass => {
                let on = !self.params.bypass.value();
                setter.set_parameter(&self.params.bypass, on);
            }
            Event::Model(m) => {
                setter.set_parameter(&self.params.model, ModelKind::from_dsp(m));
            }
            Event::Device(d) => {
                setter.set_parameter(&self.params.device, DeviceKind::from_dsp(d));
            }
            Event::ApplyPreset(pr) => {
                setter.set_parameter(&self.params.ambience, pr.ambience);
                setter.set_parameter(&self.params.voice, pr.voice_db);
                setter.set_parameter(&self.params.voice_reverb, pr.reverb);
                setter.set_parameter(
                    &self.params.ambience_mode,
                    FeatureKind::from_dsp(pr.ambience_mode),
                );
                setter.set_parameter(&self.params.voice_mode, FeatureKind::from_dsp(pr.voice_mode));
                setter.set_parameter(
                    &self.params.voice_reverb_mode,
                    FeatureKind::from_dsp(pr.reverb_mode),
                );
                setter.set_parameter(&self.params.model, ModelKind::from_dsp(pr.model_choice()));
            }
        }
    }
}

/// メモリー（1〜8）の保存先: `%APPDATA%\DPDFNet Voice\presets.json`。
///
/// OBS 版はプラグイン設定ディレクトリを使うが、DAW にはそれに当たる場所が
/// 無いのでユーザーのアプリデータに置く。プロジェクト / ホストのプリセットとは
/// 別で、プラグインをまたいで使い回せる。
fn preset_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("DPDFNet Voice").join(panel_ui::preset::FILE_NAME))
}
