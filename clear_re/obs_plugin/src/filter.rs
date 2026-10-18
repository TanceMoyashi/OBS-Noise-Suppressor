//! OBS フィルタ本体（libobs との境界）。
//!
//! `clear_re/plugin/src/PluginProcessor.cpp` の役割をそのまま引き継ぐ:
//!   * 48 kHz 以外はレート変換してから本体へ渡す（`dsp::resample`）
//!   * 変換段を作れない異常なレート、モデル未ロードはパススルー
//!   * チャンネル数は OBS の音声設定に従う（モノラル/ステレオ）
//!   * 内容遅延は `kLatencyHops * hop` = 2880 サンプル (60 ms) + 変換段の遅延
//!
//! スレッドモデル:
//!   * `create` / `update` / `activate` / `deactivate` / `destroy` は UI スレッド
//!   * `filter_audio` はオーディオスレッド
//!
//! エンジン (= ORT セッション) は `create` / `update` / `activate` の
//! UI スレッドで組み立てる。
//!
//! **`activate` だけに頼ってはいけない。** libobs がフィルタを activate するのは
//! `obs_source_video_tick()` の中で「親ソースのアクティブ状態が変化した時」だけ
//! なので (`libobs/obs-source.c` の 1404-1421 行)、
//!
//!   * すでにアクティブなソースに後からフィルタを追加した場合
//!   * 映像を持たない音声ソース (マイクなど)
//!
//! では `activate` が一度も呼ばれない。`create` は必ず呼ばれるので、ここで
//! 作っておくのが本筋。
//!
//! 差し替え中の安全性: libobs は `update` と `filter_audio` の相互排他を
//! してくれない (`obs_source_update` は音声専用ソースでは `filter_mutex` を
//! 取らない)。だから古いエンジンは即座に破棄せず `retired` に積んでおき、
//! もう呼ばれないことが確実な `destroy` でまとめて破棄する。

use std::ffi::{c_char, CStr};
use std::os::raw::c_void;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{
    AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering,
};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::obs_log;
use crate::dump::Dump;
use crate::dsp::engine::{self, DspStats, Engine, Meters, ParamSnapshot};
use crate::dsp::ort;
use crate::dsp::params as dsp;
use crate::paths;
use crate::ui;

// --------------------------------------------------------------------------
// ステータス
// --------------------------------------------------------------------------

pub const ST_OK: u32 = 0;
/// レート変換段を作れないサンプルレート（パススルー）。
pub const ST_BAD_RATE: u32 = 1;
pub const ST_MODEL_NOT_FOUND: u32 = 2;
pub const ST_MODEL_LOAD_FAILED: u32 = 3;
pub const ST_ORT_UNAVAILABLE: u32 = 4;
pub const ST_INACTIVE: u32 = 5;

/// モデルが前提にしている内部レート（`dsp::INTERNAL_SAMPLE_RATE`）。
pub const OBS_SR: u32 = crate::dsp::INTERNAL_SAMPLE_RATE as u32;

/// 表示用のレート表記（1000 の倍数は kHz）。ログとパネルで使う。
pub fn rate_label(rate: u32) -> String {
    if rate % 1000 == 0 {
        format!("{} kHz", rate / 1000)
    } else {
        format!("{rate} Hz")
    }
}

// --------------------------------------------------------------------------
// パラメータ（オーディオスレッドはアトミックを読むだけ）
// --------------------------------------------------------------------------

#[derive(Debug)]
pub struct Params {
    ambience: AtomicU32,
    voice: AtomicU32,
    voice_reverb: AtomicU32,
    /// フェーダーごとのミュート / バイパス（`dsp::FeatureMode::code()`）。
    /// つまみの値（上の 3 つ）は上書きしない。
    ambience_mode: AtomicU8,
    voice_mode: AtomicU8,
    voice_reverb_mode: AtomicU8,
    /// 使うモデル（`paths::ModelChoice::code()`）。エンジンを組み直すかの判定にも使う。
    model: AtomicU8,
    /// 推論デバイス（`paths::DeviceChoice::code()`）。同じく組み直しの判定に使う。
    device: AtomicU8,
    /// VU メーターの基準（`dsp::VuRef::code()` = dBFS）。表示にだけ効く。
    vu_ref: AtomicU8,
}

impl Params {
    fn new() -> Self {
        Self {
            ambience: AtomicU32::new((dsp::AMBIENCE_DEFAULT as f32).to_bits()),
            voice: AtomicU32::new((dsp::VOICE_DEFAULT as f32).to_bits()),
            voice_reverb: AtomicU32::new((dsp::VOICE_REVERB_DEFAULT as f32).to_bits()),
            ambience_mode: AtomicU8::new(dsp::FeatureMode::default().code()),
            voice_mode: AtomicU8::new(dsp::FeatureMode::default().code()),
            voice_reverb_mode: AtomicU8::new(dsp::FeatureMode::default().code()),
            model: AtomicU8::new(paths::ModelChoice::default().code()),
            device: AtomicU8::new(paths::DeviceChoice::default().code()),
            vu_ref: AtomicU8::new(dsp::VuRef::default().code()),
        }
    }

    pub fn snapshot(&self) -> ParamSnapshot {
        ParamSnapshot {
            ambience: f32::from_bits(self.ambience.load(Ordering::Relaxed)),
            voice_db: f32::from_bits(self.voice.load(Ordering::Relaxed)),
            voice_reverb: f32::from_bits(self.voice_reverb.load(Ordering::Relaxed)),
            ambience_mode: self.ambience_mode(),
            voice_mode: self.voice_mode(),
            voice_reverb_mode: self.voice_reverb_mode(),
        }
    }

    pub fn ambient(&self) -> f32 {
        f32::from_bits(self.ambience.load(Ordering::Relaxed))
    }
    pub fn voice(&self) -> f32 {
        f32::from_bits(self.voice.load(Ordering::Relaxed))
    }
    pub fn voice_reverb(&self) -> f32 {
        f32::from_bits(self.voice_reverb.load(Ordering::Relaxed))
    }

    /// フェーダーごとのミュート / バイパス。
    pub fn ambience_mode(&self) -> dsp::FeatureMode {
        dsp::FeatureMode::from_code(self.ambience_mode.load(Ordering::Relaxed))
    }
    pub fn voice_mode(&self) -> dsp::FeatureMode {
        dsp::FeatureMode::from_code(self.voice_mode.load(Ordering::Relaxed))
    }
    pub fn voice_reverb_mode(&self) -> dsp::FeatureMode {
        dsp::FeatureMode::from_code(self.voice_reverb_mode.load(Ordering::Relaxed))
    }

    pub fn set_ambience(&self, v: f32) {
        self.ambience.store(v.to_bits(), Ordering::Relaxed);
    }
    pub fn set_voice(&self, v: f32) {
        self.voice.store(v.to_bits(), Ordering::Relaxed);
    }
    pub fn set_voice_reverb(&self, v: f32) {
        self.voice_reverb.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn set_ambience_mode(&self, m: dsp::FeatureMode) {
        self.ambience_mode.store(m.code(), Ordering::Relaxed);
    }
    pub fn set_voice_mode(&self, m: dsp::FeatureMode) {
        self.voice_mode.store(m.code(), Ordering::Relaxed);
    }
    pub fn set_voice_reverb_mode(&self, m: dsp::FeatureMode) {
        self.voice_reverb_mode.store(m.code(), Ordering::Relaxed);
    }

    /// 使うモデル。
    pub fn model(&self) -> paths::ModelChoice {
        paths::ModelChoice::from_code(self.model.load(Ordering::Relaxed))
    }

    pub fn set_model(&self, m: paths::ModelChoice) {
        self.model.store(m.code(), Ordering::Relaxed);
    }

    /// 推論デバイス（CPU / GPU）。
    pub fn device(&self) -> paths::DeviceChoice {
        paths::DeviceChoice::from_code(self.device.load(Ordering::Relaxed))
    }

    pub fn set_device(&self, d: paths::DeviceChoice) {
        self.device.store(d.code(), Ordering::Relaxed);
    }

    /// VU メーターの基準（0 VU = 何 dBFS）。
    pub fn vu_ref(&self) -> dsp::VuRef {
        dsp::VuRef::from_code(self.vu_ref.load(Ordering::Relaxed))
    }

    pub fn set_vu_ref(&self, v: dsp::VuRef) {
        self.vu_ref.store(v.code(), Ordering::Relaxed);
    }

    /// OBS の設定から読み込む（`update` / `create` のみが呼ぶ）。
    ///
    /// 文字列の設定は「キーが無いと空文字」が返るので、空 = 既定値として扱う
    /// （古いシーンコレクションを読んでも既定で動く）。
    pub fn read_settings(&self, settings: *mut crate::ffi::obs_data_t) {
        if settings.is_null() {
            return;
        }
        unsafe {
            let a = crate::ffi::obs_data_get_double(settings, dsp::AMBIENCE.as_ptr()) as f32;
            let v = crate::ffi::obs_data_get_double(settings, dsp::VOICE.as_ptr()) as f32;
            let r = crate::ffi::obs_data_get_double(settings, dsp::VOICE_REVERB.as_ptr()) as f32;
            self.set_ambience(a);
            self.set_voice(v);
            self.set_voice_reverb(r);

            let m = crate::ffi::obs_data_get_string(settings, dsp::MODEL.as_ptr());
            if !m.is_null() {
                let s = CStr::from_ptr(m).to_string_lossy();
                self.set_model(paths::ModelChoice::from_setting(&s));
            }

            let dv = crate::ffi::obs_data_get_string(settings, dsp::DEVICE.as_ptr());
            if !dv.is_null() {
                let s = CStr::from_ptr(dv).to_string_lossy();
                self.set_device(paths::DeviceChoice::from_setting(&s));
            }

            let vr = crate::ffi::obs_data_get_string(settings, dsp::VU_REF.as_ptr());
            if !vr.is_null() {
                let s = CStr::from_ptr(vr).to_string_lossy();
                self.set_vu_ref(dsp::VuRef::from_setting(&s));
            }

            // フェーダーのミュート / バイパス。文字列なので、キーが無ければ
            // 空文字 → 既定（有効）に落ちる。
            let am = crate::ffi::obs_data_get_string(settings, dsp::AMBIENCE_MODE.as_ptr());
            if !am.is_null() {
                let s = CStr::from_ptr(am).to_string_lossy();
                self.set_ambience_mode(dsp::FeatureMode::from_setting(&s));
            }
            let vm = crate::ffi::obs_data_get_string(settings, dsp::VOICE_MODE.as_ptr());
            if !vm.is_null() {
                let s = CStr::from_ptr(vm).to_string_lossy();
                self.set_voice_mode(dsp::FeatureMode::from_setting(&s));
            }
            let rm = crate::ffi::obs_data_get_string(settings, dsp::VOICE_REVERB_MODE.as_ptr());
            if !rm.is_null() {
                let s = CStr::from_ptr(rm).to_string_lossy();
                self.set_voice_reverb_mode(dsp::FeatureMode::from_setting(&s));
            }
        }
    }
}

// --------------------------------------------------------------------------
// 共有状態（オーディオスレッド / UI スレッド / パネルが触る）
// --------------------------------------------------------------------------

/// パネル UI と共有する状態。
///
/// **`Arc` で持つのが肝**。パネルはウィンドウが生きている間 `Arc` を保持するので、
/// OBS 側のフィルタが先に破棄されてもぶら下がりポインタを作らない。
/// `source`（`obs_source_t*`）だけは OBS が所有するため、`alive` が false に
/// なったらパネルからは触らない。
pub struct Shared {
    pub params: Params,
    pub meters: Arc<Meters>,

    /// フィルタが生存しているか。`destroy` で false になる。
    pub alive: AtomicBool,

    /// 開いているパネルの `HWND`（0 = 閉じている）。UI スレッドのみが書く。
    pub panel: AtomicUsize,

    /// 2 出力モデル（dereverb あり）か。パネルの REVERB フェーダーの有効/無効に使う。
    pub dereverb: AtomicBool,

    /// 現在のエンジンのチャンネル数（1 = モノラル / 2 = ステレオ）。
    /// パネルがメーターを間引くのに使う。0 = エンジン未構築。
    pub channels: AtomicUsize,

    /// エンジンが想定している OBS のサンプルレート。パネルの見出しに出す。
    /// 0 = エンジン未構築。
    pub rate: AtomicU32,

    status: AtomicU32,
    status_detail: Mutex<String>,
    /// モデルの概要（パネルに出す 1 行）。
    summary: Mutex<String>,
}

impl Shared {
    pub(crate) fn new(meters: Arc<Meters>) -> Self {
        Self {
            params: Params::new(),
            meters,
            alive: AtomicBool::new(true),
            panel: AtomicUsize::new(0),
            dereverb: AtomicBool::new(false),
            channels: AtomicUsize::new(0),
            rate: AtomicU32::new(0),
            status: AtomicU32::new(ST_INACTIVE),
            status_detail: Mutex::new(String::new()),
            summary: Mutex::new(String::new()),
        }
    }

    pub fn status(&self) -> u32 {
        self.status.load(Ordering::Relaxed)
    }

    pub fn status_detail(&self) -> String {
        self.status_detail.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn summary(&self) -> String {
        self.summary.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn set_status_detail(&self, status: u32, detail: impl Into<String>) {
        let detail = detail.into();
        if let Ok(mut g) = self.status_detail.lock() {
            *g = detail.clone();
        }
        let prev = self.status.swap(status, Ordering::Relaxed);
        if prev != status {
            obs_log!(crate::ffi::LOG_INFO, "status: {}", detail);
        }
    }

    pub(crate) fn set_summary(&self, s: String) {
        if let Ok(mut g) = self.summary.lock() {
            *g = s;
        }
    }
}

// --------------------------------------------------------------------------
// インスタンスデータ
// --------------------------------------------------------------------------

pub struct FilterData {
    /// パネルと共有する状態。
    pub shared: Arc<Shared>,

    /// 現在のエンジン。`null` はパススルー。
    ///
    /// 書き換えは UI スレッド (`create` / `update` / `activate` / `destroy`) のみ。
    /// オーディオスレッドは `Acquire` で読んで使う。古いエンジンは即座に破棄せず
    /// `retired` に積んで `destroy` で破棄する（`filter_audio` が掴んでいる
    /// 可能性があるため）。
    engine: AtomicPtr<Engine>,
    engine_channels: AtomicUsize,
    /// 今のエンジンが使っているモデル（`paths::ModelChoice::code()`）。
    /// 設定変更で組み直すべきかの判定に使う。
    engine_model: AtomicU8,
    /// 今のエンジンが使っている推論デバイス（`paths::DeviceChoice::code()`）。
    engine_device: AtomicU8,
    /// 今のエンジンが想定しているサンプルレート。OBS の設定変更で変わる。
    engine_rate: AtomicU32,

    /// 監視スレッドが「軽量へ落とす」と決めたときの強制指定
    /// (`paths::ModelChoice::code()`、`FORCED_NONE` = なし）。ユーザーの設定は
    /// 書き換えない（次に OK を押すと解除される）。
    forced_model: AtomicU8,

    /// エンジンの組み立ての直列化。監視スレッドと UI スレッド
    /// (`update`) が同時に組み直さないようにする。
    rebuild_lock: Mutex<()>,

    /// 負荷の監視スレッド（間に合っていないかをログに出し、必要なら軽量
    /// モデルへ落とす）。`destroy` で停止させてから join する。
    watchdog: Mutex<Option<Watchdog>>,

    /// 差し替えで引退したエンジン。オーディオスレッドがまだ使っている可能性が
    /// あるので、`destroy` まで保持する。
    retired: Mutex<Vec<*mut Engine>>,

    /// 「エンジンが無いのでパススルー中」を一度だけログしたか。
    passthrough_logged: AtomicBool,

    /// 再生位置 (ns)。不連続を検出してリセットするため。
    last_timestamp: AtomicU64,

    /// フィルタソース自身（プロパティ/UI から親を引くため）。借用のみで保持。
    pub source: *mut crate::ffi::obs_source_t,

    /// モデル / ORT を探すディレクトリ。
    pub resource_dirs: Vec<PathBuf>,

    /// 実機検証用のダンプ（`DPDFNET_DUMP` が設定されているときだけ `Some`）。
    /// 環境変数が無ければ `None` なので、通常運用ではロックすら取らない。
    pub dump: Mutex<Option<Dump>>,
}

impl FilterData {
    pub fn new(
        settings: *mut crate::ffi::obs_data_t,
        source: *mut crate::ffi::obs_source_t,
        resource_dirs: Vec<PathBuf>,
    ) -> Self {
        let me = Self {
            shared: Arc::new(Shared::new(Arc::new(Meters::new()))),
            engine: AtomicPtr::new(ptr::null_mut()),
            engine_channels: AtomicUsize::new(0),
            engine_model: AtomicU8::new(u8::MAX),
            engine_device: AtomicU8::new(u8::MAX),
            engine_rate: AtomicU32::new(0),
            forced_model: AtomicU8::new(FORCED_NONE),
            rebuild_lock: Mutex::new(()),
            watchdog: Mutex::new(None),
            retired: Mutex::new(Vec::new()),
            passthrough_logged: AtomicBool::new(false),
            last_timestamp: AtomicU64::new(0),
            source,
            resource_dirs,
            dump: Mutex::new(None),
        };
        me.shared.params.read_settings(settings);
        me
    }

    pub fn status_detail(&self) -> String {
        self.shared.status_detail()
    }

    fn set_status_detail(&self, status: u32, detail: impl Into<String>) {
        self.shared.set_status_detail(status, detail)
    }

    /// エンジンを（必要なら）組み立てる。UI スレッド
    /// (`create` / `update` / `activate`) と監視スレッドから呼ぶ。
    pub fn activate_engine(&self) -> bool {
        // 監視スレッドと `update` が同時に組み立てないように直列化する
        // （組み立ては数百 ms かかるので、待たされる側には影響しない）。
        let _rebuild = match self.rebuild_lock.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };

        let audio = unsafe { crate::ffi::obs_get_audio() };
        if audio.is_null() {
            self.set_status_detail(ST_INACTIVE, "OBS audio がまだ初期化されていない");
            return false;
        }

        let sr = unsafe { crate::ffi::audio_output_get_sample_rate(audio) };
        if !crate::dsp::resample::supported(sr) {
            // 変換段を作れないレート（想定外の値）。安全側でパススルー。
            self.shared.rate.store(0, Ordering::Relaxed);
            self.set_status_detail(
                ST_BAD_RATE,
                format!(
                    "OBS の音声が {} です。この値は変換できないためパススルーします",
                    rate_label(sr)
                ),
            );
            return false;
        }

        let want = (unsafe { crate::ffi::audio_output_get_channels(audio) } as usize).clamp(1, 2);
        // 監視スレッドが「負荷が高いので軽量へ」と決めたときはその指定を使う
        // （ユーザーの設定「自動」は書き換えない。次に OK を押すと解除される）。
        let forced = self.forced_model.load(Ordering::Relaxed);
        let want_model = if forced == FORCED_NONE {
            self.shared.params.model()
        } else {
            paths::ModelChoice::from_code(forced)
        };

        let want_device = self.shared.params.device();

        let cur = self.engine.load(Ordering::Acquire);
        if !cur.is_null()
            && self.engine_channels.load(Ordering::Relaxed) == want
            && self.engine_model.load(Ordering::Relaxed) == want_model.code()
            && self.engine_device.load(Ordering::Relaxed) == want_device.code()
            && self.engine_rate.load(Ordering::Relaxed) == sr
        {
            return true;
        }

        // ONNX Runtime を先に確定させる（失敗理由を出せるように）。
        // CPU と GPU は別の DLL なので、デバイスごとに読み込み結果を持つ。
        if let Err(e) = ort::ensure_loaded(want_device, &self.resource_dirs) {
            self.set_status_detail(ST_ORT_UNAVAILABLE, e);
            return false;
        }

        let model = paths::env_model_file()
            .or_else(|| paths::find_model_choice(want_model, &self.resource_dirs));
        let Some(model) = model else {
            self.set_status_detail(
                ST_MODEL_NOT_FOUND,
                format!(
                    "DPDFNet モデルが見つかりません (探索先: {})",
                    paths::model_dirs(&self.resource_dirs)
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" / ")
                ),
            );
            return false;
        };

        match engine::build(
            want,
            sr,
            &model,
            &self.resource_dirs,
            want_device,
            Arc::clone(&self.shared.meters),
        ) {
            Ok(eng) => {
                let mut eng = eng;
                // 起動時キャリブレーション: 音を出す前に「このマシンで回るか」を
                // 実測して、重ければ軽量モデルで組み直す（下の関数を参照）。
                let mut model_now = want_model;
                let mut forced_now = forced;
                if let Some(light) = self.calibrate_and_maybe_downgrade(
                    &mut eng,
                    want,
                    sr,
                    want_model,
                    want_device,
                    forced,
                ) {
                    eng = light;
                    model_now = paths::ModelChoice::Fast;
                    forced_now = model_now.code();
                }

                let summary = eng.model_summary.clone();
                let eng_latency = eng.latency_samples;
                let added_ms = eng.added_latency_ms;
                let dereverb = eng.dereverb_available;
                let device_used = eng.device;
                let ptr = Box::into_raw(Box::new(eng));

                let old = self.engine.swap(ptr, Ordering::AcqRel);
                self.engine_channels.store(want, Ordering::Relaxed);
                self.engine_model.store(model_now.code(), Ordering::Relaxed);
                self.engine_device.store(device_used.code(), Ordering::Relaxed);
                self.engine_rate.store(sr, Ordering::Relaxed);
                self.shared.dereverb.store(dereverb, Ordering::Relaxed);
                self.shared.channels.store(want, Ordering::Relaxed);
                self.shared.rate.store(sr, Ordering::Relaxed);

                // 実機検証用のダンプ（`DPDFNET_DUMP` 未設定なら何もしない）。
                // 中身は OBS が見ているレートのまま（= 変換前後）なので、
                // WAV のヘッダにも実際のレートを書く。
                if let Ok(mut g) = self.dump.lock() {
                    if g.is_none() {
                        *g = Dump::from_env(sr);
                    }
                }

                if !old.is_null() {
                    // `update` 経由でも呼ばれる = 音が流れている最中かもしれない。
                    // オーディオスレッドがまだ古いエンジンを掴んでいる可能性が
                    // あるので、ここでは破棄せず destroy まで持っておく。
                    self.retire(old);
                }

                // 無事できたので、次のパススルー警告はまた出せるようにする。
                self.passthrough_logged.store(false, Ordering::Relaxed);

                self.set_status_detail(
                    ST_OK,
                    format!(
                        "{} / ch={want} / latency {} samples ({:.0} ms) / dev {} / {summary}",
                        rate_label(sr),
                        eng_latency,
                        eng_latency as f64 / OBS_SR as f64 * 1000.0,
                        device_used.label()
                    ),
                );
                // パネルに出す 1 行は短く保つ（冗長な詳細は status_detail 側にあり、
                // ログとプロパティの説明に出る）。旧 VST3 版の
                // `48 kHz / model loaded (2 outputs)` に相当する。
                let lat_ms = eng_latency as f64 / OBS_SR as f64 * 1000.0;
                let src = if added_ms >= 0.05 {
                    format!(" +SRC {added_ms:.1} ms")
                } else {
                    String::new()
                };
                // 負荷で自動的に落ちている場合は、そのことが分かるように書く
                // （プロパティの設定は「自動」のままなので、混乱しないように）。
                let model_label = if forced_now == FORCED_NONE {
                    model_now.as_str().to_string()
                } else {
                    format!("{} (負荷により自動切替)", model_now.as_str())
                };
                // GPU を指定したのに組み立てられなかったときは、そう書く
                // （黙って CPU で動くと「GPU が効いていない」ことに気付けない）。
                let device_label = if device_used == want_device {
                    want_device.label().to_string()
                } else {
                    format!("{} (GPU 失敗→CPU)", device_used.label())
                };
                self.shared.set_summary(format!(
                    "{want} ch  ·  {}  ·  遅延 {eng_latency} samples ({lat_ms:.0} ms{src})  ·  dereverb {}  ·  model {model_label}  ·  dev {device_label}",
                    rate_label(sr),
                    if dereverb { "あり" } else { "なし" },
                ));
                true
            }
            Err(e) => {
                self.set_status_detail(ST_MODEL_LOAD_FAILED, format!("モデルの読み込みに失敗: {e}"));
                false
            }
        }
    }

    /// 起動時キャリブレーション: 組んだエンジンを実際に空回しして、このマシンで
    /// 予算（1 ホップ = 480 サンプル = 10 ms）に収まるかを測る。
    ///
    /// 収まらなければ**軽量モデルを組んで返す**。呼び出し側はそれを採用し、
    /// パネルには「負荷により自動切替」と出す（監視スレッドが落としたときと
    /// 同じ見え方にする）。
    ///
    /// 監視スレッドの自動フォールバック（下）は「3 秒続けて間に合わない」のを
    /// 待ってから落とすので、その 3 秒ぶんは音が乱れる。遅い PC ではこれが
    /// 「音が 3 秒遅れる」ように見えるため、**音を出す前に**決めてしまう。
    ///
    /// 判定は次のときだけ行う（＝ユーザーの明示指定や検証用の固定は尊重する）:
    ///   * 自動フォールバックと同じ条件（`FORCED_NONE` / 設定「自動」/ env 未設定）
    ///   * 軽量モデルが探索先にある
    fn calibrate_and_maybe_downgrade(
        &self,
        eng: &mut Engine,
        want: usize,
        sr: u32,
        want_model: paths::ModelChoice,
        want_device: paths::DeviceChoice,
        forced: u8,
    ) -> Option<Engine> {
        if forced != FORCED_NONE
            || want_model != paths::ModelChoice::Auto
            || paths::env_model_file().is_some()
        {
            return None;
        }
        let light = paths::find_model_choice(paths::ModelChoice::Fast, &self.resource_dirs)?;

        let budget_us = eng.budget_us;
        let avg_us = eng.measure_hop_us(CALIBRATION_WARMUP_HOPS, CALIBRATION_HOPS);
        let ms = avg_us as f64 / 1000.0;
        let budget_ms = budget_us as f64 / 1000.0;
        let pct = avg_us as f64 * 100.0 / budget_us.max(1) as f64;

        if !crate::load::too_slow_at_start(avg_us, budget_us) {
            obs_log!(
                crate::ffi::LOG_INFO,
                "起動時の実測: {ms:.2} ms / 予算 {budget_ms:.2} ms ({pct:.0} %) — 品質モデルのまま開始します"
            );
            return None;
        }

        obs_log!(
            crate::ffi::LOG_INFO,
            "起動時の実測が予算に収まらないため軽量モデルで開始します: {ms:.2} ms / 予算 {budget_ms:.2} ms ({pct:.0} %)"
        );
        match engine::build(
            want,
            sr,
            &light,
            &self.resource_dirs,
            want_device,
            Arc::clone(&self.shared.meters),
        ) {
            Ok(light_eng) => {
                // ここで指定を残すので、以後の `update` / `activate` も軽量で組まれる
                // （監視スレッドの `fallback_to_light` と同じ扱い）。
                self.forced_model
                    .store(paths::ModelChoice::Fast.code(), Ordering::Relaxed);
                Some(light_eng)
            }
            Err(e) => {
                obs_log!(
                    crate::ffi::LOG_WARNING,
                    "軽量モデルに切り替えられませんでした（品質モデルのまま続けます）: {e}"
                );
                None
            }
        }
    }

    pub fn engine_ptr(&self) -> *mut Engine {
        self.engine.load(Ordering::Acquire)
    }

    pub fn drop_engine(&self) {
        let old = self.engine.swap(ptr::null_mut(), Ordering::AcqRel);
        self.engine_channels.store(0, Ordering::Relaxed);
        self.engine_model.store(u8::MAX, Ordering::Relaxed);
        self.engine_device.store(u8::MAX, Ordering::Relaxed);
        self.engine_rate.store(0, Ordering::Relaxed);
        self.shared.channels.store(0, Ordering::Relaxed);
        self.shared.rate.store(0, Ordering::Relaxed);
        if !old.is_null() {
            unsafe { drop(Box::from_raw(old)) };
        }
    }

    /// 差し替えで不要になったエンジンを「引退」させる（破棄はしない）。
    fn retire(&self, engine: *mut Engine) {
        let mut g = match self.retired.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        g.push(engine);
    }

    /// 引退したエンジンをまとめて破棄する。もうオーディオスレッドから
    /// 呼ばれないことが確実な `destroy` からのみ呼ぶこと。
    pub fn free_retired(&self) {
        let mut g = match self.retired.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        for p in g.drain(..) {
            if !p.is_null() {
                unsafe { drop(Box::from_raw(p)) };
            }
        }
    }
}

// --------------------------------------------------------------------------
// リソース探索パス
// --------------------------------------------------------------------------

/// モデルと ORT を探すディレクトリ一覧。
pub fn collect_resource_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 1) OBS が設定したモジュールデータディレクトリ
    if let Some(d) = module_data_dir() {
        dirs.push(d.clone());
        dirs.push(d.join("models"));
    }

    // 2) 自分の DLL の 2 つ上 (= <plugin root>) とその data
    if let Some(dll_dir) = paths::own_dll_dir() {
        let root = dll_dir.join("..").join("..");
        dirs.push(root.join("data"));
        dirs.push(root.join("data").join("models"));
        dirs.push(root.clone());
    }

    dirs
}

/// `obs_module_file("")` 相当。モジュールのデータディレクトリ。
pub fn module_data_dir() -> Option<PathBuf> {
    let module = crate::MODULE_POINTER.load(Ordering::SeqCst);
    if module.is_null() {
        return None;
    }

    let p = unsafe { crate::ffi::obs_find_module_file(module, c"".as_ptr()) };
    if p.is_null() {
        return None;
    }

    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { crate::ffi::bfree(p as *mut c_void) };

    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

// --------------------------------------------------------------------------
// obs_source_info コールバック
// --------------------------------------------------------------------------

pub unsafe extern "C" fn get_name(_type_data: *mut c_void) -> *const c_char {
    c"OBS Noise Suppressor".as_ptr()
}

pub unsafe extern "C" fn create(
    settings: *mut crate::ffi::obs_data_t,
    source: *mut crate::ffi::obs_source_t,
) -> *mut c_void {
    crate::guard(|| {
        let dirs = collect_resource_dirs();
        let fd = Box::new(FilterData::new(settings, source, dirs));
        obs_log!(
            crate::ffi::LOG_INFO,
            "filter created (ambience {:.2}, voice {:.2} dB, reverb {:.2})",
            fd.shared.params.ambient(),
            fd.shared.params.voice(),
            fd.shared.params.voice_reverb()
        );
        // ここでエンジンまで作っておく。libobs はフィルタの `activate` を
        // 「親ソースのアクティブ状態が変化した時」しか呼ばないため、
        // (1) すでにアクティブなソースへ後から追加した場合や
        // (2) 映像を持たない音声ソースでは `activate` が来ず、
        // パススルーのまま動かなくなる。`create` は必ず呼ばれる。
        fd.activate_engine();

        // 負荷の監視を始める。監視スレッドは `FilterData` のポインタを使うので、
        // `Box::into_raw` の後（= アドレスが確定した後）に始める。
        let fd = Box::into_raw(fd);
        if let Some(w) = spawn_watchdog(fd) {
            // SAFETY: `fd` は今作った有効なポインタ。
            unsafe { (*fd).set_watchdog(w) };
        }
        fd as *mut c_void
    })
}

pub unsafe extern "C" fn destroy(data: *mut c_void) {
    crate::guard(|| {
        if data.is_null() {
            return;
        }
        let fd = Box::from_raw(data as *mut FilterData);
        // パネルが `source` に触らないように先に落とす。
        fd.shared.alive.store(false, Ordering::SeqCst);
        // 監視スレッドを先に止める（止めるまで、この先の後始末
        // (`drop_engine` / `free_retired`) と競合させない）。
        fd.stop_watchdog();
        ui::close_panel(&fd.shared);
        fd.drop_engine();
        // 差し替えで引退したエンジンもここでまとめて破棄する
        // (もうオーディオスレッドからは呼ばれない)。
        fd.free_retired();
        // ダンプはヘッダを書き戻して閉じる（WAV として読めるように）。
        if let Ok(mut g) = fd.dump.lock() {
            if let Some(d) = g.as_mut() {
                d.finish();
            }
        }
        obs_log!(crate::ffi::LOG_INFO, "filter destroyed");
    })
}

pub unsafe extern "C" fn get_defaults(settings: *mut crate::ffi::obs_data_t) {
    crate::guard(|| {
        if settings.is_null() {
            return;
        }
        crate::ffi::obs_data_set_default_double(
            settings,
            dsp::AMBIENCE.as_ptr(),
            dsp::AMBIENCE_DEFAULT,
        );
        crate::ffi::obs_data_set_default_double(settings, dsp::VOICE.as_ptr(), dsp::VOICE_DEFAULT);
        crate::ffi::obs_data_set_default_double(
            settings,
            dsp::VOICE_REVERB.as_ptr(),
            dsp::VOICE_REVERB_DEFAULT,
        );
        crate::ffi::obs_data_set_default_string(
            settings,
            dsp::MODEL.as_ptr(),
            dsp::MODEL_DEFAULT.as_ptr(),
        );
        crate::ffi::obs_data_set_default_string(
            settings,
            dsp::DEVICE.as_ptr(),
            dsp::DEVICE_DEFAULT.as_ptr(),
        );
        crate::ffi::obs_data_set_default_string(
            settings,
            dsp::VU_REF.as_ptr(),
            dsp::VU_REF_DEFAULT.as_ptr(),
        );
        for key in [dsp::AMBIENCE_MODE, dsp::VOICE_MODE, dsp::VOICE_REVERB_MODE] {
            crate::ffi::obs_data_set_default_string(
                settings,
                key.as_ptr(),
                dsp::FEATURE_MODE_DEFAULT.as_ptr(),
            );
        }
    })
}

pub unsafe extern "C" fn update(data: *mut c_void, settings: *mut crate::ffi::obs_data_t) {
    crate::guard(|| {
        if data.is_null() || settings.is_null() {
            return;
        }
        // アトミック書き込みのみ。エンジンには触らない。
        let fd = &*(data as *const FilterData);
        // ユーザーがプロパティでモデルを選び直したときだけ、自動フォールバックの
        // 指定を解除する。パネルのつまみを動かしただけの `update`
        // (`push_settings` 経由) でもここを通るので、無条件に解除すると
        // 高品質へ組み直してまた音が途切れてしまう。
        let before = fd.shared.params.model();
        fd.shared.params.read_settings(settings);
        if fd.shared.params.model() != before {
            fd.forced_model.store(FORCED_NONE, Ordering::Relaxed);
        }
        // レート/チャンネル数が変わっていたら作り直す。古いエンジンは
        // `retired` に積まれるので、オーディオスレッドが使用中でも安全。
        // (モデル未配置のまま起動した後、配置してから「OK」を押した場合の
        //  再試行も兼ねる。)
        fd.activate_engine();
        obs_log!(
            crate::ffi::LOG_DEBUG,
            "settings updated (ambience {:.2} [{}], voice {:.2} dB [{}], reverb {:.2} [{}])",
            fd.shared.params.ambient(),
            fd.shared.params.ambience_mode().as_str(),
            fd.shared.params.voice(),
            fd.shared.params.voice_mode().as_str(),
            fd.shared.params.voice_reverb(),
            fd.shared.params.voice_reverb_mode().as_str()
        );
    })
}

pub unsafe extern "C" fn activate(data: *mut c_void) {
    crate::guard(|| {
        if data.is_null() {
            return;
        }
        let fd = &*(data as *const FilterData);
        fd.activate_engine();

        // 検証用: `DPDFNET_OPEN_PANEL_ON_START` があればパネルを自動で開く
        // (クリック操作なしで見た目を確認するため)。
        if std::env::var_os("DPDFNET_OPEN_PANEL_ON_START").is_some()
            && fd.shared.panel.load(Ordering::SeqCst) == 0
        {
            obs_log!(crate::ffi::LOG_INFO, "パネルを自動で開きます (activate)");
            if let Err(e) = ui::open_panel(&fd.shared, fd.source) {
                obs_log!(crate::ffi::LOG_WARNING, "パネルを開けません: {e}");
            }
        }
    })
}

pub unsafe extern "C" fn deactivate(data: *mut c_void) {
    crate::guard(|| {
        if data.is_null() {
            return;
        }
        let fd = &*(data as *const FilterData);
        fd.set_status_detail(ST_INACTIVE, "非アクティブ（パススルー）");
    })
}

pub unsafe extern "C" fn get_properties(
    data: *mut c_void,
) -> *mut crate::ffi::obs_properties_t {
    crate::guard(|| {
        let props = crate::ffi::obs_properties_create();

        let status = if data.is_null() {
            "（フィルタが未設定）".to_string()
        } else {
            let fd = &*(data as *const FilterData);
            fd.status_detail()
        };

        let info = crate::ffi::obs_properties_add_text(
            props,
            c"dpdfnet_info".as_ptr(),
            c"OBS Noise Suppressor — 内容遅延 60 ms / 48 kHz 以外は内部でレート変換".as_ptr(),
            crate::ffi::OBS_TEXT_INFO,
        );
        let status_c = std::ffi::CString::new(status).unwrap_or_default();
        crate::ffi::obs_property_set_long_description(info, status_c.as_ptr());

        // フェーダーごとのミュート / バイパス（パネルのキーと同じ設定）。
        // つまみの値はそのままで、効きだけを差し替える。
        let add_mode_list = |key: &CStr, title: &CStr, desc: &CStr| {
            let list = crate::ffi::obs_properties_add_list(
                props,
                key.as_ptr(),
                title.as_ptr(),
                crate::ffi::OBS_COMBO_TYPE_LIST,
                crate::ffi::OBS_COMBO_FORMAT_STRING,
            );
            for m in dsp::FeatureMode::ALL {
                let v = std::ffi::CString::new(m.as_str()).unwrap_or_default();
                let l = std::ffi::CString::new(m.label()).unwrap_or_default();
                crate::ffi::obs_property_list_add_string(list, l.as_ptr(), v.as_ptr());
            }
            crate::ffi::obs_property_set_long_description(list, desc.as_ptr());
        };

        let amb = crate::ffi::obs_properties_add_float_slider(
            props,
            dsp::AMBIENCE.as_ptr(),
            c"Ambience".as_ptr(),
            dsp::AMBIENCE_MIN,
            dsp::AMBIENCE_MAX,
            0.01,
        );
        crate::ffi::obs_property_set_description(
            amb,
            c"残す暗騒音。0 % = 最大抑圧 (-48 dB)、100 % = 最小抑圧 (-6 dB)".as_ptr(),
        );
        add_mode_list(
            dsp::AMBIENCE_MODE,
            c"Ambience の扱い",
            c"ミュート = 0 %（暗騒音を残さない）/ バイパス = 100 %（そのまま）。つまみの位置は変わりません。",
        );

        let voice = crate::ffi::obs_properties_add_float_slider(
            props,
            dsp::VOICE.as_ptr(),
            c"Voice".as_ptr(),
            dsp::VOICE_MIN,
            dsp::VOICE_MAX,
            0.1,
        );
        crate::ffi::obs_property_float_set_suffix(voice, c" dB".as_ptr());
        crate::ffi::obs_property_set_description(
            voice,
            c"声の presence。0 dB で完全なパススルー".as_ptr(),
        );
        add_mode_list(
            dsp::VOICE_MODE,
            c"Voice の扱い",
            c"ミュート = -12 dB（presence を最小）/ バイパス = 0 dB（掛けない）。つまみの位置は変わりません。",
        );

        let rev = crate::ffi::obs_properties_add_float_slider(
            props,
            dsp::VOICE_REVERB.as_ptr(),
            c"Voice Reverb".as_ptr(),
            dsp::VOICE_REVERB_MIN,
            dsp::VOICE_REVERB_MAX,
            0.01,
        );
        crate::ffi::obs_property_set_description(
            rev,
            c"残す残響（モデルが 2 出力のときのみ有効）".as_ptr(),
        );
        add_mode_list(
            dsp::VOICE_REVERB_MODE,
            c"Voice Reverb の扱い",
            c"ミュート = 0 %（残響を残さない）/ バイパス = 100 %（そのまま）。つまみの位置は変わりません。",
        );

        // ---- モデル（ドロップダウン） ----
        // 同梱しているのは dpdfnet8 と dpdfnet2 の 2 種だけなので選択肢は 3 つ。
        // 同梱されていない選択肢は disabled にして選べないようにする。
        let dirs = if data.is_null() {
            collect_resource_dirs()
        } else {
            let fd = &*(data as *const FilterData);
            fd.resource_dirs.clone()
        };

        let model_list = crate::ffi::obs_properties_add_list(
            props,
            dsp::MODEL.as_ptr(),
            c"モデル".as_ptr(),
            crate::ffi::OBS_COMBO_TYPE_LIST,
            crate::ffi::OBS_COMBO_FORMAT_STRING,
        );
        for (idx, (val, label)) in [
            ("auto", "自動（高品質を優先）"),
            ("dp8", "高品質 - dpdfnet8"),
            ("dp2", "軽量 - dpdfnet2"),
        ]
        .iter()
        .enumerate()
        {
            let v = std::ffi::CString::new(*val).unwrap_or_default();
            let l = std::ffi::CString::new(*label).unwrap_or_default();
            crate::ffi::obs_property_list_add_string(model_list, l.as_ptr(), v.as_ptr());
            if paths::find_model_choice(paths::ModelChoice::from_setting(val), &dirs).is_none() {
                crate::ffi::obs_property_list_item_disable(model_list, idx, true);
            }
        }
        crate::ffi::obs_property_set_long_description(
            model_list,
            c"使う DPDFNet モデル。切り替えるとエンジンを作り直すため数十 ms 音が途切れます。".as_ptr(),
        );

        // ---- 推論デバイス（ドロップダウン） ----
        // GPU は DirectML 経由で動くが、実測では CPU より遅い（per-hop の
        // dispatch / sync が支配的なため）。CPU 側の余裕がまったく無い環境用の
        // 選択肢として残してある。同梱の DML 版 ORT が無ければ選べない。
        let device_list = crate::ffi::obs_properties_add_list(
            props,
            dsp::DEVICE.as_ptr(),
            c"推論デバイス".as_ptr(),
            crate::ffi::OBS_COMBO_TYPE_LIST,
            crate::ffi::OBS_COMBO_FORMAT_STRING,
        );
        for (idx, d) in [paths::DeviceChoice::Cpu, paths::DeviceChoice::Gpu]
            .iter()
            .enumerate()
        {
            let v = std::ffi::CString::new(d.as_str()).unwrap_or_default();
            let label = match d {
                paths::DeviceChoice::Cpu => "CPU".to_string(),
                paths::DeviceChoice::Gpu => "GPU - DirectML".to_string(),
            };
            let l = std::ffi::CString::new(label).unwrap_or_default();
            crate::ffi::obs_property_list_add_string(device_list, l.as_ptr(), v.as_ptr());
            if paths::ort_dll_candidates(*d, &dirs).is_empty() {
                crate::ffi::obs_property_list_item_disable(device_list, idx, true);
            }
        }
        crate::ffi::obs_property_set_long_description(
            device_list,
            c"推論の実行先。GPU は DirectML 経由で、別配布の追加パック (gpu-pack) を\
入れたときだけ選べます。実測では GPU のほうが 1 フレームあたり 1.6〜2.3 倍遅いので、\
通常は CPU のままにしてください。"
                .as_ptr(),
        );

        // ---- VU メーターの基準 ----
        let vu_list = crate::ffi::obs_properties_add_list(
            props,
            dsp::VU_REF.as_ptr(),
            c"VU メーター基準".as_ptr(),
            crate::ffi::OBS_COMBO_TYPE_LIST,
            crate::ffi::OBS_COMBO_FORMAT_STRING,
        );
        for vr in dsp::VuRef::ALL {
            let v = std::ffi::CString::new(vr.as_str()).unwrap_or_default();
            let l = std::ffi::CString::new(vr.label()).unwrap_or_default();
            crate::ffi::obs_property_list_add_string(vu_list, l.as_ptr(), v.as_ptr());
        }
        crate::ffi::obs_property_set_long_description(
            vu_list,
            c"0 VU が何 dBFS に当たるか。針の目盛り全体（-20 VU 〜 +3 VU）が基準と一緒に動きます。"
                .as_ptr(),
        );

        // 自前 Win32 パネル（フェーダー + メーター + メモリー）。`data` はこの
        // フィルタの `FilterData` なので、そのまま `priv` として渡せる。
        if !data.is_null() {
            crate::ffi::obs_properties_add_button2(
                props,
                c"dpdfnet_panel".as_ptr(),
                c"パネルを開く（フェーダー / メーター / メモリー）".as_ptr(),
                Some(on_open_panel_clicked),
                data,
            );
        }

        props
    })
}

/// 「パネルを開く」ボタン。
unsafe extern "C" fn on_open_panel_clicked(
    _props: *mut crate::ffi::obs_properties_t,
    _property: *mut crate::ffi::obs_property_t,
    data: *mut c_void,
) -> bool {
    crate::guard(|| {
        if data.is_null() {
            return false;
        }
        let fd = &*(data as *const FilterData);
        let shared = Arc::clone(&fd.shared);
        match ui::open_panel(&shared, fd.source) {
            Ok(()) => {
                obs_log!(crate::ffi::LOG_INFO, "panel opened");
                true
            }
            Err(e) => {
                obs_log!(crate::ffi::LOG_WARNING, "パネルを開けません: {}", e);
                false
            }
        }
    })
}

/// 音声処理。`audio` をその場で書き換えて返す。
pub unsafe extern "C" fn filter_audio(
    data: *mut c_void,
    audio: *mut crate::ffi::obs_audio_data,
) -> *mut crate::ffi::obs_audio_data {
    // パニック時は素通し（audio をそのまま返す）。
    let fallback = audio;

    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if data.is_null() || audio.is_null() {
            return;
        }

        let fd = &*(data as *const FilterData);
        let engine_ptr = fd.engine_ptr();
        if engine_ptr.is_null() {
            // 一度だけ理由を出す。原因切り分けに一番効く
            // (モデルが無い / 読み込み失敗 / 変換できないレート / 未構築 等)。
            if !fd.passthrough_logged.swap(true, Ordering::Relaxed) {
                obs_log!(
                    crate::ffi::LOG_WARNING,
                    "エンジン未構築のためパススルーします: {}",
                    fd.status_detail()
                );
            }
            return; // パススルー
        }

        let a = &mut *audio;
        let frames = a.frames as usize;
        if frames == 0 {
            return;
        }

        // チャンネル数は data[i] が非 NULL の個数（obs_audio_data にフィールドが無い）。
        let mut num_ch = 0usize;
        for plane in a.data.iter() {
            if plane.is_null() {
                break;
            }
            num_ch += 1;
        }
        if num_ch == 0 {
            return;
        }

        let engine = &mut *engine_ptr;
        if engine.num_channels != num_ch {
            // 設定変更などでチャンネル数が変わった。activate で作り直すまでパススルー。
            return;
        }

        // タイムスタンプの不連続（一時停止など）で状態をリセットする。
        let ts = a.timestamp;
        let last = fd.last_timestamp.swap(ts, Ordering::Relaxed);
        if last != 0 && ts.wrapping_sub(last) > 1_000_000_000 {
            engine.reset();
        }

        let mut buffers = [ptr::null_mut::<f32>(); crate::ffi::MAX_AV_PLANES];
        for (i, plane) in a.data.iter().take(num_ch).enumerate() {
            buffers[i] = *plane as *mut f32;
        }

        // 検証用ダンプ（`DPDFNET_DUMP` 未設定なら `None` で素通り）。
        // エンジンの `process` はその場でバッファを書き換えるので、入力を先に控える。
        let mut guard = fd.dump.lock().ok();
        if let Some(d) = guard.as_deref_mut().and_then(|o| o.as_mut()) {
            d.capture_input(&buffers[..num_ch], frames);
        }

        engine.process(
            &buffers[..num_ch],
            frames,
            fd.shared.params.snapshot(),
        );

        if let Some(d) = guard.as_deref_mut().and_then(|o| o.as_mut()) {
            d.write_output(&buffers[..num_ch], frames);
        }
    }));

    if r.is_err() {
        obs_log!(crate::ffi::LOG_ERROR, "panic in filter_audio; passthrough");
        return fallback;
    }

    audio
}

// --------------------------------------------------------------------------
// 負荷の監視
// --------------------------------------------------------------------------
//
// DPDFNet の推論は 1 ホップ = 480 サンプル (10 ms) ごとに走る。この 10 ms に
// 間に合わないと OBS の音声スレッドが詰まり、音が途切れて「音が遅れる」ように
// 見える。速い CPU でも高品質モデル (`dpdfnet8`) は 1 コアの 7 割を使うので、
//
//   * 起動前に「このマシンで回るか」を実測し、重ければ最初から軽量モデルで
//     始める（`calibrate_and_maybe_downgrade`）
//   * 余裕がどれくらいあるかを 1 行だけログに残す（切り分け用）
//   * 間に合っていない状態が続いたら、1 度だけ軽量モデルへ自動で落とす
//
// を行う。判定は共通クレートの `crate::load`（純関数）側にある。

/// 自動フォールバックの指定が無いことを表す値（`engine_model` と同じ流儀）。
const FORCED_NONE: u8 = u8::MAX;

/// 起動時キャリブレーションで捨てるホップ数。
///
/// 最初のホップは ONNX Runtime の初回実行（グラフ初期化・メモリ確保）で跳ねる
/// ので、平均に入れない。
const CALIBRATION_WARMUP_HOPS: usize = 2;

/// 起動時キャリブレーションで測るホップ数（6 ホップ = 60 ms ぶんの推論）。
const CALIBRATION_HOPS: usize = 6;

/// 監視の周期。短くしておくと `destroy` の待ち時間も短くなる。
const WATCH_PERIOD: Duration = Duration::from_millis(1000);

/// 同じ警告を出し直す最短の間隔。
const WARN_INTERVAL: Duration = Duration::from_secs(60);

/// 監視スレッドのハンドル。
struct Watchdog {
    stop: mpsc::Sender<()>,
    handle: JoinHandle<()>,
}

/// 監視スレッドへ渡す `FilterData` ポインタ。生ポインタは `Send` ではないので包む。
///
/// 安全に使える根拠: 監視スレッドは `destroy` の `stop_watchdog()` で
/// join されるまで生きており、join が終わるまで `FilterData` は破棄されない。
struct FdPtr(*mut FilterData);
unsafe impl Send for FdPtr {}

fn spawn_watchdog(fd: *mut FilterData) -> Option<Watchdog> {
    let (tx, rx) = mpsc::channel::<()>();
    let ptr = FdPtr(fd);
    let handle = std::thread::Builder::new()
        .name("obs-noise-suppressor-load".to_string())
        .spawn(move || watchdog_loop(ptr, rx))
        .ok()?;
    Some(Watchdog { stop: tx, handle })
}

fn watchdog_loop(ptr: FdPtr, rx: mpsc::Receiver<()>) {
    let mut prev = DspStats::default();
    let mut reported = false;
    let mut slow_streak = 0u32;
    let mut last_warn: Option<Instant> = None;
    let mut fell_back = false;

    loop {
        // 終了指示が来たら抜ける（周期ごとに目を覚ます）。
        match rx.recv_timeout(WATCH_PERIOD) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        // SAFETY: `destroy` は `stop_watchdog()`（= このスレッドの join）を
        // 済ませてから `FilterData` を破棄する。
        let fd = unsafe { &*ptr.0 };
        if !fd.shared.alive.load(Ordering::Relaxed) {
            return;
        }

        let cur = fd.shared.meters.dsp_stats();
        let Some(load) = crate::load::sample(prev, cur) else {
            // まだサンプルが足りない（無音でホップが進んでいない等）。
            continue;
        };
        prev = cur;

        if !reported {
            reported = true;
            obs_log!(
                crate::ffi::LOG_INFO,
                "音声処理の負荷: 1 ホップ平均 {:.2} ms / 予算 {:.2} ms ({:.0} %)、最大 {:.2} ms",
                load.avg_ms,
                load.budget_ms,
                load.usage_pct(),
                load.max_ms
            );
        }

        if !load.slow {
            slow_streak = 0;
            continue;
        }

        if last_warn.map_or(true, |t| t.elapsed() >= WARN_INTERVAL) {
            last_warn = Some(Instant::now());
            obs_log!(
                crate::ffi::LOG_WARNING,
                "音声処理がリアルタイムに間に合っていません (1 ホップ {:.2} ms の枠に平均 {:.2} ms、予算超過 {:.0} %、最大 {:.2} ms)。音が途切れる場合は、プロパティの「モデル」を「軽量 (dp2)」にすると負荷が約 1/3 になります",
                load.budget_ms,
                load.avg_ms,
                load.over_pct,
                load.max_ms
            );
        }

        // 自動フォールバックは「本当に間に合っていない」ときだけ。予算ぎりぎり
        // で動いている機械で、一時的な負荷変動だけに反応しないようにする。
        if !load.critical {
            slow_streak = 0;
            continue;
        }

        slow_streak += 1;
        if fell_back || slow_streak < crate::load::FALLBACK_STREAK {
            continue;
        }

        // 1 度だけ試す（軽量モデルが無い / ユーザーが明示指定しているときは
        // 切り替えない）。
        fell_back = true;
        if fd.fallback_to_light() {
            obs_log!(
                crate::ffi::LOG_WARNING,
                "負荷が高いため、自動的に軽量モデル (dp2) へ切り替えました (平均 {:.2} ms / 予算 {:.2} ms)。プロパティの「モデル」は「自動」のままです。プロパティでモデルを選び直すと元の指定に戻ります",
                load.avg_ms,
                load.budget_ms
            );
        }
    }
}

impl FilterData {
    fn set_watchdog(&self, w: Watchdog) {
        let mut g = match self.watchdog.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        *g = Some(w);
    }

    /// 監視スレッドを止めて join する（`destroy` の最初に呼ぶ）。
    fn stop_watchdog(&self) {
        let w = {
            let mut g = match self.watchdog.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            g.take()
        };
        if let Some(w) = w {
            let _ = w.stop.send(());
            let _ = w.handle.join();
        }
    }

    /// 軽量モデルへ組み替える（監視スレッドから呼ぶ）。実際に強制指定を
    /// 入れて組み立て直したら true。
    fn fallback_to_light(&self) -> bool {
        let ok = can_fallback_to_light(
            // モデルを固定している（検証時）なら触らない。
            paths::env_model_file().is_some(),
            // ユーザーの明示指定は尊重する（「自動」のときだけ落とす）。
            self.shared.params.model(),
            // すでに軽量で動いているなら何もしない。
            self.engine_model.load(Ordering::Relaxed),
            paths::find_model_choice(paths::ModelChoice::Fast, &self.resource_dirs).is_some(),
        );
        if !ok {
            return false;
        }

        self.forced_model
            .store(paths::ModelChoice::Fast.code(), Ordering::Relaxed);
        // ここで組めなくても `forced_model` の指定は残るので、次に
        // `update` / `activate` が来たときに軽量で組み直される。
        self.activate_engine()
    }
}

/// 自動フォールバックをしてよいか（純関数。テストしやすいように分離）。
///
///   * `env_model`: `DPDFNET_MODEL` でモデルを固定している
///   * `chosen`: ユーザーの設定（「自動」のときだけ落とす）
///   * `current`: 今のエンジンのモデル（`u8::MAX` = エンジン未構築）
///   * `light_available`: 軽量モデル (`dpdfnet2`) が探索先にある
fn can_fallback_to_light(
    env_model: bool,
    chosen: paths::ModelChoice,
    current: u8,
    light_available: bool,
) -> bool {
    !env_model
        && chosen == paths::ModelChoice::Auto
        && current != paths::ModelChoice::Fast.code()
        && light_available
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_needs_auto_model_and_a_light_model() {
        use paths::ModelChoice::{Auto, Fast, Quality};

        // 自動 + 未構築 + 軽量あり → 落としてよい。
        assert!(can_fallback_to_light(false, Auto, u8::MAX, true));
        // 高品質を明示指定している → 落とさない（警告だけ）。
        assert!(!can_fallback_to_light(false, Quality, u8::MAX, true));
        // 軽量を明示指定 → 落とす必要がない。
        assert!(!can_fallback_to_light(false, Fast, u8::MAX, true));
        // すでに軽量で動いている → 何もしない。
        assert!(!can_fallback_to_light(false, Auto, Fast.code(), true));
        // 軽量モデルが置かれていない → 落とせない。
        assert!(!can_fallback_to_light(false, Auto, u8::MAX, false));
        // `DPDFNET_MODEL` で固定している（検証時）→ 触らない。
        assert!(!can_fallback_to_light(true, Auto, u8::MAX, true));
    }

    /// レート表記（ログとパネルで同じものを使う）。
    #[test]
    fn rate_label_prefers_khz_for_round_numbers() {
        assert_eq!(rate_label(48_000), "48 kHz");
        assert_eq!(rate_label(96_000), "96 kHz");
        assert_eq!(rate_label(8_000), "8 kHz");
        // 1000 の倍数でないレートは Hz のまま（44.1 kHz は `44100 Hz`）。
        assert_eq!(rate_label(44_100), "44100 Hz");
    }
}
