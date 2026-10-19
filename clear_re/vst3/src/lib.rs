//! DPDFNet Voice — VST3 版（DAW 用）。
//!
//! OBS フィルタ (`clear_re/obs_plugin`) と**同じ DSP コア** (`clear_re/dpdfnet`)
//! を使う。ここにあるのは nih-plug のシェルだけ:
//!
//!   * パラメータ → `ParamSnapshot`（OBS 版と同じ意味・同じ既定値）
//!   * `initialize` でエンジンを組み立て、遅延をホストに報告する
//!   * `process` は `core::Core` に丸投げ（レート変換もコア側がやる）
//!
//! GUI は OBS 版と同じ自前 Win32 パネル（共通クレート `panel_ui`）を子ウィンドウ
//! として出す（`editor.rs`）。つまみ / ミュート / BYPASS / モデル / デバイス /
//! メモリーをパネルから操作できる。状態（プリセット）は nih-plug がパラメータ
//! 一覧から自動で保存するので、DAW 側のプリセットに乗る。

// `nih_export_vst3!` が展開する vst3-sys 側のマクロが、式位置の末尾セミコロンで
// 警告を出す（上流のマクロ由来。こちらのコードではない）。VST3 版だけの話なので
// ここで黙らせる。
#![allow(semicolon_in_expressions_from_non_local_macros)]

use std::sync::Arc;

use nih_plug::prelude::*;

mod core;
mod editor;
mod params;

#[cfg(test)]
mod offline_tests;

use core::Core;
use params::DpdfnetParams;

/// 遅延・レートの再計算に使うので、`initialize` で受け取った値を覚えておく。
struct DpdfnetVoice {
    params: Arc<DpdfnetParams>,
    core: Core,
    rate: f32,
}

impl Default for DpdfnetVoice {
    fn default() -> Self {
        Self {
            params: Arc::new(DpdfnetParams::default()),
            core: Core::new(),
            rate: 48_000.0,
        }
    }
}

impl Plugin for DpdfnetVoice {
    const NAME: &'static str = "DPDFNet Voice";
    const VENDOR: &'static str = "TanceMoyashi";
    const URL: &'static str = "https://github.com/TanceMoyashi/OBS-Noise-Suppressor";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    // モノ → モノ、ステレオ → ステレオ。モデルは 1 ch ずつ推論するので、
    // OBS フィルタと同じ扱いになる。
    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(1),
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            ..AudioIOLayout::const_default()
        },
    ];

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        // パネル（自前 Win32 の子ウィンドウ）。描画・入力はホストの UI スレッドで
        // 動き、音声スレッドからは触らない。メーターと表示用の状態は `Core` が
        // 持つアトミック / ミューテックスを共有する。
        Some(Box::new(editor::DpdfnetEditor::new(
            Arc::clone(&self.params),
            self.core.meters(),
            self.core.shared(),
        )))
    }

    fn initialize(
        &mut self,
        layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        context: &mut impl InitContext<Self>,
    ) -> bool {
        // ここは音声スレッドではないので、モデルの読み込み（数百 ms）をしてよい。
        self.rate = buffer_config.sample_rate;
        let channels = layout.main_input_channels.map(|c| c.get() as usize).unwrap_or(2);
        self.core.build(
            channels,
            self.rate,
            self.params.model_choice(),
            self.params.device_choice(),
        );

        // 組み立てに失敗しても true を返す（パススルーで生き延びる）。理由は
        // ログに出る（`NIH_LOG=<ファイル>` でファイルにも残せる）。
        if let Some(e) = self.core.error() {
            nih_error!("DPDFNet Voice: {e} — 入力をそのまま通します");
        }

        let latency = self.core.latency_samples(self.rate);
        context.set_latency_samples(latency);
        nih_log!("DPDFNet Voice: {} Hz / 遅延 {latency} samples", self.rate as u32);
        true
    }

    fn reset(&mut self) {
        // シーク・再開でモデルの内部状態と FIFO を戻す。
        self.core.reset();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        // パネルの BYPASS がオンなら何もせず素通しする。エンジンに流さないので
        // メーターの通し番号が止まり、パネルの針は静止位置へ戻る。
        if self.params.bypassed() {
            return ProcessStatus::Normal;
        }

        let frames = buffer.samples();
        let params = self.params.snapshot();

        // モデル / 推論デバイスは自動化しない設定だが、ホストの汎用表示からは
        // 変えられる。変わっていたらここで組み直す（数 100 ms 音が途切れる。
        // 自動化されない値なので、意図して変えたときだけ起きる）。
        let model = self.params.model_choice();
        let device = self.params.device_choice();
        if model != self.core.model() || device != self.core.device() {
            self.core.build(buffer.channels(), self.rate, model, device);
            if let Some(e) = self.core.error() {
                nih_error!("DPDFNet Voice: {e} — 入力をそのまま通します");
            }
            context.set_latency_samples(self.core.latency_samples(self.rate));
        }

        let chans = buffer.as_slice();
        self.core.process(chans, frames, params);

        ProcessStatus::Normal
    }
}

impl Vst3Plugin for DpdfnetVoice {
    // 変えてはいけない（ホストがこの ID でプラグインを覚える）。
    const VST3_CLASS_ID: [u8; 16] = *b"DPDFNetVoiceVST3";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Restoration];
}

nih_export_vst3!(DpdfnetVoice);
