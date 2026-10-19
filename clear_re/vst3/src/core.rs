//! VST3 シェルの中身（DSP の駆動）。
//!
//! `Plugin::process` はこの型へ丸投げするだけにしてある。ホスト（DAW）の中で
//! しか動かせない部分をここに閉じ込めておくと、テストから直接叩ける
//! （`cargo test` でホスト無しに音声経路を確認できる）。
//!
//! 音声経路そのもの（STFT・モデル・遅延・レート変換）は OBS 版と同じ
//! `dpdfnet` クレートにある。ここが持つのは「いつ組み立てるか」「どのモデルを
//! 使うか」だけ。

use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use nih_plug::{nih_error, nih_log, nih_warn};

use dpdfnet::engine::{self, Engine, Meters, ParamSnapshot};
use dpdfnet::paths::{self, DeviceChoice, ModelChoice};

/// 起動時キャリブレーションで捨てるホップ数（初回の ONNX 実行は跳ねる）。
/// OBS 版 (`filter.rs`) と同じ方針。
const CALIBRATION_WARMUP_HOPS: usize = 2;

/// 起動時キャリブレーションで測るホップ数（6 ホップ = 60 ms ぶんの推論）。
const CALIBRATION_HOPS: usize = 6;

/// エディタ（パネル）が読む表示用の共有状態。
///
/// **音声スレッドは触らない。** 書き込むのは `build`（非音声スレッド）だけで、
/// パネルは 40 ms ごとに読むだけ。パラメータそのもの（つまみ / モデル /
/// デバイス / バイパス）は `Arc<DpdfnetParams>` のアトミックから直接読むので、
/// ここには「エンジンの状態」だけを置く。
pub struct EditorShared {
    /// エンジンのチャンネル数（0 = 未構築）。
    pub channels: AtomicUsize,
    /// エンジンが想定しているサンプルレート（0 = 未構築）。
    pub rate: AtomicU32,
    /// 2 出力モデル（dereverb あり）か。
    pub dereverb: AtomicBool,
    /// 0 = 正常（`panel_ui::STATUS_OK`）、非 0 = エラー / 未構築。LED の色に使う。
    status: AtomicU32,
    status_text: Mutex<String>,
    summary: Mutex<String>,
}

impl EditorShared {
    fn new() -> Self {
        Self {
            channels: AtomicUsize::new(0),
            rate: AtomicU32::new(0),
            dereverb: AtomicBool::new(false),
            status: AtomicU32::new(1),
            status_text: Mutex::new("初期化中".into()),
            summary: Mutex::new(String::new()),
        }
    }

    pub fn status(&self) -> u32 {
        self.status.load(Ordering::Relaxed)
    }

    pub fn status_text(&self) -> String {
        self.status_text.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn summary(&self) -> String {
        self.summary.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn set_status(&self, status: u32, text: impl Into<String>) {
        if let Ok(mut g) = self.status_text.lock() {
            *g = text.into();
        }
        self.status.store(status, Ordering::Relaxed);
    }

    fn set_summary(&self, text: impl Into<String>) {
        if let Ok(mut g) = self.summary.lock() {
            *g = text.into();
        }
    }
}

pub struct Core {
    engine: Option<Engine>,
    /// 組み立てに使ったモデル / デバイス（設定が変わったかの判定に使う）。
    model: ModelChoice,
    device: DeviceChoice,
    /// 最後の失敗理由。`None` なら正常。
    error: Option<String>,
    meters: Arc<Meters>,
    /// エディタ（パネル）が読む表示用の状態。
    shared: Arc<EditorShared>,
    /// モデル / ONNX Runtime の追加探索先（bundle の中を明示したいとき用）。
    extra_dirs: Vec<PathBuf>,
}

impl Core {
    /// ログの出口を nih-plug のロガーへ繋いでから作る。
    ///
    /// コア側は「どこへ出すか」を知らないので、最初の 1 回だけ差し込む
    /// （`set_sink` は 2 回目以降を無視する）。`NIH_LOG=<ファイル>` で
    /// ファイルにも出せる（nih-plug の仕様）。
    pub fn new() -> Self {
        dpdfnet::log::set_sink(log_sink);
        Self {
            engine: None,
            model: ModelChoice::Auto,
            device: DeviceChoice::Cpu,
            error: None,
            meters: Arc::new(Meters::new()),
            shared: Arc::new(EditorShared::new()),
            extra_dirs: Vec::new(),
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// エディタ（パネル）と共有するメーター。
    pub fn meters(&self) -> Arc<Meters> {
        Arc::clone(&self.meters)
    }

    /// エディタ（パネル）と共有する表示用の状態。
    pub fn shared(&self) -> Arc<EditorShared> {
        Arc::clone(&self.shared)
    }

    pub fn model(&self) -> ModelChoice {
        self.model
    }

    pub fn device(&self) -> DeviceChoice {
        self.device
    }

    /// エンジンを組み立てる。**音声スレッド以外から呼ぶこと**（数百 ms かかる）。
    ///
    /// `model` が「自動」のときは、組み立てたあとに実測して重ければ軽量モデルで
    /// 組み直す（OBS 版の起動時キャリブレーションと同じ判断）。DAW では「音が
    /// 乱れてから自動で落ちる」までの数秒がそのまま録音に残るので、最初から
    /// 決めてしまうほうが良い。
    pub fn build(&mut self, channels: usize, rate: f32, model: ModelChoice, device: DeviceChoice) {
        self.model = model;
        self.device = device;
        self.engine = None;
        self.error = None;
        self.shared.channels.store(0, Ordering::Relaxed);
        self.shared.dereverb.store(false, Ordering::Relaxed);
        self.shared.rate.store(rate as u32, Ordering::Relaxed);

        let channels = channels.clamp(1, 2);
        if !dpdfnet::resample::supported(rate as u32) {
            self.error = Some(format!("未対応のサンプルレート: {rate} Hz"));
            self.shared
                .set_status(1, format!("未対応のサンプルレート: {} Hz", rate as u32));
            self.shared.set_summary(String::new());
            return;
        }

        let dirs = self.extra_dirs.clone();
        let Some(model_path) =
            paths::env_model_file().or_else(|| paths::find_model_choice(model, &dirs))
        else {
            self.error = Some(format!(
                "DPDFNet モデルが見つかりません (探索先: {})",
                paths::model_dirs(&dirs)
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ));
            self.shared.set_status(1, "モデルが見つかりません");
            self.shared.set_summary(String::new());
            return;
        };

        let mut eng = match engine::build(
            channels,
            rate as u32,
            &model_path,
            &dirs,
            device,
            Arc::clone(&self.meters),
        ) {
            Ok(e) => e,
            Err(e) => {
                self.error = Some(format!("モデルの読み込みに失敗: {e}"));
                self.shared
                    .set_status(1, format!("モデルの読み込みに失敗: {e}"));
                self.shared.set_summary(String::new());
                return;
            }
        };

        // 起動時キャリブレーション。「自動」で品質モデルを組んだときだけ、
        // 実際に回して予算に収まるかを測る（検証用の env 指定は尊重する）。
        if model == ModelChoice::Auto && paths::env_model_file().is_none() {
            if let Some(light) = paths::find_model_choice(ModelChoice::Fast, &dirs) {
                let avg_us = eng.measure_hop_us(CALIBRATION_WARMUP_HOPS, CALIBRATION_HOPS);
                let ms = avg_us as f64 / 1000.0;
                let budget_ms = eng.budget_us as f64 / 1000.0;
                let pct = avg_us as f64 * 100.0 / eng.budget_us.max(1) as f64;

                if dpdfnet::load::too_slow_at_start(avg_us, eng.budget_us) {
                    nih_warn!(
                        "起動時の実測が予算に収まらないため軽量モデルで開始します: \
                         {ms:.2} ms / 予算 {budget_ms:.2} ms ({pct:.0} %)"
                    );
                    match engine::build(
                        channels,
                        rate as u32,
                        &light,
                        &dirs,
                        device,
                        Arc::clone(&self.meters),
                    ) {
                        Ok(l) => {
                            eng = l;
                            self.model = ModelChoice::Fast;
                        }
                        Err(e) => nih_warn!(
                            "軽量モデルに切り替えられませんでした（品質モデルのまま続けます）: {e}"
                        ),
                    }
                } else {
                    nih_log!(
                        "起動時の実測: {ms:.2} ms / 予算 {budget_ms:.2} ms ({pct:.0} %) — \
                         品質モデルのまま開始します"
                    );
                }
            }
        }

        nih_log!(
            "model={} / ch={channels} / {} Hz / 遅延 {} samples / dev {} / {}",
            self.model.as_str(),
            rate as u32,
            eng.latency_samples,
            eng.device.label(),
            eng.model_summary
        );

        // パネル（エディタ）に出す状態を更新する。ステータス 1 行は OBS 版と
        // 同じ形にして、下段に短い要約を出す。
        let lat = eng.latency_samples;
        let lat_ms = lat as f64 / dpdfnet::INTERNAL_SAMPLE_RATE as f64 * 1000.0;
        let dev_label = eng.device.label();
        self.shared.channels.store(channels, Ordering::Relaxed);
        self.shared.rate.store(rate as u32, Ordering::Relaxed);
        self.shared
            .dereverb
            .store(eng.dereverb_available, Ordering::Relaxed);
        self.shared.set_status(
            panel_ui::STATUS_OK,
            format!(
                "{} / ch={channels} / 遅延 {} samples ({lat_ms:.0} ms) / dev {} / {}",
                panel_ui::rate_label(rate as u32),
                lat,
                dev_label,
                eng.model_summary
            ),
        );
        self.shared.set_summary(format!(
            "{channels} ch  ·  {}  ·  遅延 {lat} samples ({lat_ms:.0} ms)  ·  dereverb {}  ·  model {}  ·  dev {}",
            panel_ui::rate_label(rate as u32),
            if eng.dereverb_available { "あり" } else { "なし" },
            self.model.as_str(),
            dev_label
        ));

        self.engine = Some(eng);
    }

    /// タイムスタンプの不連続（シーク・再開）で状態を戻す。
    pub fn reset(&mut self) {
        if let Some(e) = self.engine.as_mut() {
            e.reset();
        }
    }

    /// ホストに報告する遅延 [samples]。48 kHz なら 2880（60 ms）そのもの。
    ///
    /// レート変換を使うときは、変換段の遅延 (`added_latency_ms`) を足す。
    pub fn latency_samples(&self, rate: f32) -> u32 {
        let Some(eng) = &self.engine else {
            return 0;
        };
        let base =
            eng.latency_samples as f64 * rate as f64 / dpdfnet::INTERNAL_SAMPLE_RATE as f64;
        let extra = eng.added_latency_ms as f64 * rate as f64 / 1000.0;
        (base + extra).round() as u32
    }

    /// 音声ブロックをその場で処理する。
    ///
    /// **アロケーションしない**（ポインタ配列は固定長のスタック配列）。
    /// エンジンが無い / チャンネル数が合わないときはパススルー。
    pub fn process(&mut self, chans: &mut [&mut [f32]], frames: usize, params: ParamSnapshot) {
        let Some(eng) = self.engine.as_mut() else {
            return;
        };
        let n = chans.len().min(2);
        if n == 0 || frames == 0 || eng.num_channels != n {
            return;
        }

        let mut ptrs = [ptr::null_mut::<f32>(); 2];
        for (i, c) in chans.iter_mut().enumerate().take(n) {
            ptrs[i] = c.as_mut_ptr();
        }
        eng.process(&ptrs[..n], frames, params);
    }

    /// テスト用: 入出力をまとめて流す（OBS 版の `run_stream` と同じ形）。
    #[cfg(test)]
    pub fn process_all(&mut self, chans: &mut [Vec<f32>], block: usize, params: ParamSnapshot) {
        let frames = chans[0].len();
        let mut pos = 0usize;
        while pos < frames {
            let n = block.min(frames - pos);
            let mut ptrs: Vec<&mut [f32]> = chans.iter_mut().map(|c| &mut c[pos..]).collect();
            self.process(&mut ptrs, n, params);
            pos += n;
        }
    }
}

/// DSP コアのログを nih-plug のロガーへ流す。
fn log_sink(level: dpdfnet::log::Level, msg: &str) {
    match level {
        dpdfnet::log::Level::Info => nih_log!("{}", msg),
        dpdfnet::log::Level::Warning => nih_warn!("{}", msg),
        dpdfnet::log::Level::Error => nih_error!("{}", msg),
    }
}
