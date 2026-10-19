//! 自前 Win32 パネル（OBS 版）のホスト結線。
//!
//! 見た目・操作・数値は共通クレート [`panel_ui`] 側にある（VST3 版と共有）。
//! ここは OBS 固有の部分だけを担う:
//!
//!   * パネル**専用スレッド**を作り、そこで [`Panel`] + [`panel_ui::message_loop`]
//!     （OBS 側のメッセージループ（Qt）に依存しないため。ウィンドウの寿命と
//!     メッセージポンプを自分で持つ）
//!   * 共有状態 [`Shared`] を [`Host`] に合わせる（[`ObsHost`]）。パネルは
//!     毎ティック [`Host::state`] でアトミックを読むだけ、操作は
//!     [`Host::event`] で押し込まれる
//!   * ユーザー操作を OBS のパラメータ / 設定へ書き戻す（`push_settings`）
//!
//! `Shared` は `Arc` なので OBS 側のフィルタが先に破棄されてもぶら下がり
//! ポインタにならない。`obs_source_t*` だけは OBS が所有するので、
//! `Shared::alive` が false になったら触らない。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use panel_ui::{Config, Event, Host, Panel, Parent, State};
use panel_ui::preset::FILE_NAME;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;

use crate::dsp::params as dsp;
use crate::filter::Shared;

// --------------------------------------------------------------------------
// 公開 API
// --------------------------------------------------------------------------

/// `Shared::panel` の「開いている途中」を表す番兵。本物の `HWND` はこれより大きい。
const PANEL_OPENING: usize = 1;

/// パネルを開く（既に開いていれば前面に出す）。OBS の UI スレッドから呼ぶ。
pub fn open_panel(shared: &Arc<Shared>, source: *mut std::ffi::c_void) -> Result<(), String> {
    // 0 = 閉じている。開いている/開いている途中は前面に出すだけ
    // （ボタン連打でウィンドウが 2 枚出るのを防ぐための CAS）。
    if shared
        .panel
        .compare_exchange(0, PANEL_OPENING, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        let h = shared.panel.load(Ordering::SeqCst);
        if h > PANEL_OPENING {
            unsafe {
                let _ = SetForegroundWindow(HWND(h as *mut std::ffi::c_void));
            }
        }
        return Ok(());
    }

    // オーナーは「今アクティブなウィンドウ」= OBS のプロパティ画面。
    let owner = SendPtr(panel_ui::active_window());
    let src = SendPtr(source);

    let thread_shared = Arc::clone(shared);
    let handle = match std::thread::Builder::new()
        .name("obs-noise-suppressor-panel".into())
        .stack_size(1 << 20)
        .spawn(move || run_panel(thread_shared, owner, src))
    {
        Ok(h) => h,
        Err(e) => {
            // 失敗したら「閉じている」に戻して、次回また試せるようにする。
            shared.panel.store(0, Ordering::SeqCst);
            return Err(format!("スレッドを作れません: {e}"));
        }
    };

    if let Ok(mut g) = PANEL_THREAD.lock() {
        *g = Some(handle);
    }

    Ok(())
}

/// パネルを閉じる（開いていなければ何もしない）。
pub fn close_panel(shared: &Shared) {
    let h = shared.panel.load(Ordering::SeqCst);
    if h > PANEL_OPENING {
        panel_ui::post_close(h as *mut std::ffi::c_void);
    }
}

/// モジュールのアンロード前にパネルスレッドの終了を待つ。
///
/// パネルスレッドは我々のコード上を走っているので、`FreeLibrary` より先に
/// 終わらせておきたい。最大 `timeout_ms` だけ待ち、終わっていれば join する
/// （終わっていなければハンドルを捨てる。ハングさせるよりは安全側）。
pub fn shutdown(timeout_ms: u64) {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while PANEL_RUNNING.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }

    if !PANEL_RUNNING.load(Ordering::SeqCst) {
        if let Ok(mut g) = PANEL_THREAD.lock() {
            if let Some(h) = g.take() {
                let _ = h.join();
            }
        }
    }
}

// --------------------------------------------------------------------------
// スレッド
// --------------------------------------------------------------------------

static PANEL_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static PANEL_RUNNING: AtomicBool = AtomicBool::new(false);

/// 生ポインタをスレッドへ渡すためのラッパ。パネルは `Shared::alive` が true の
/// 間だけ `source` を使い、`owner` はウィンドウ生成にしか使わない。
struct SendPtr(*mut std::ffi::c_void);
unsafe impl Send for SendPtr {}

fn run_panel(shared: Arc<Shared>, owner: SendPtr, source: SendPtr) {
    PANEL_RUNNING.store(true, Ordering::SeqCst);
    if let Err(e) = create_and_run(Arc::clone(&shared), owner.0, source.0) {
        crate::obs_log!(crate::ffi::LOG_WARNING, "パネル: {}", e);
    }
    // ウィンドウが閉じた（あるいは作れなかった）。次の「開く」で開けるようにする。
    shared.panel.store(0, Ordering::SeqCst);
    PANEL_RUNNING.store(false, Ordering::SeqCst);
}

fn create_and_run(
    shared: Arc<Shared>,
    owner: *mut std::ffi::c_void,
    source: *mut std::ffi::c_void,
) -> Result<(), String> {
    let host = ObsHost {
        shared: Arc::clone(&shared),
        source,
    };

    // 保存先は OBS のプラグイン設定ディレクトリ。倍率はオーナーの DPI から
    // 推定させる（`scale <= 0` かつ単独ウィンドウのとき）。
    let config = Config {
        parent: Parent::Standalone { owner },
        title: "OBS Noise Suppressor".into(),
        scale: 0.0,
        preset_path: crate::paths::config_file(FILE_NAME),
        // モデル / デバイスのキーは VST3 版だけ。OBS 版はプロパティ画面で選ぶ。
        model_key: false,
        device_key: false,
    };

    let panel = Panel::new(Box::new(host), config)?;

    shared.panel.store(panel.hwnd() as usize, Ordering::SeqCst);
    crate::obs_log!(crate::ffi::LOG_INFO, "パネルを表示しました");

    // このスレッド専用のメッセージループ。OBS のループには依存しない。
    panel_ui::message_loop();

    shared.panel.store(0, Ordering::SeqCst);
    // `Drop` でウィンドウを片付ける（既に閉じていれば何もしない）。
    drop(panel);
    Ok(())
}

// --------------------------------------------------------------------------
// ホスト結線
// --------------------------------------------------------------------------

/// `Shared` を [`Host`] に合わせる。
///
/// `Send + Sync` は [`Host`] の要件（パネルが `Box<dyn Host>` を持ち、VST3 の
/// `Editor` が `Send` を要求する）。`source` は `alive` の下でしか触らない。
struct ObsHost {
    shared: Arc<Shared>,
    source: *mut std::ffi::c_void,
}

unsafe impl Send for ObsHost {}
unsafe impl Sync for ObsHost {}

impl ObsHost {
    /// OBS の設定にも同じ値を書き戻す。
    ///
    /// `obs_source_update` が我々の `update` コールバックを呼び、同じ値が
    /// アトミックに再入力される（冪等）。これでプロパティ画面の表示と
    /// シーンコレクションへの保存もパネルに追従する。
    /// **モデルも一緒に書き戻すこと**（書き忘れると既定へ戻ってしまう）。
    fn push_settings(&self) {
        if !self.shared.alive.load(Ordering::SeqCst) || self.source.is_null() {
            return;
        }
        unsafe {
            let d = crate::ffi::obs_data_create();
            if d.is_null() {
                return;
            }
            crate::ffi::obs_data_set_double(
                d,
                dsp::AMBIENCE.as_ptr(),
                self.shared.params.ambient() as f64,
            );
            crate::ffi::obs_data_set_double(
                d,
                dsp::VOICE.as_ptr(),
                self.shared.params.voice() as f64,
            );
            crate::ffi::obs_data_set_double(
                d,
                dsp::VOICE_REVERB.as_ptr(),
                self.shared.params.voice_reverb() as f64,
            );
            let model =
                std::ffi::CString::new(self.shared.params.model().as_str()).unwrap_or_default();
            crate::ffi::obs_data_set_string(d, dsp::MODEL.as_ptr(), model.as_ptr());
            // パネルにデバイスの UI は無いが、ここで書き戻さないと
            // `read_settings` が空文字を読んで CPU に戻ってしまう
            // （つまみを動かしただけで GPU 設定が消える）。
            let device =
                std::ffi::CString::new(self.shared.params.device().as_str()).unwrap_or_default();
            crate::ffi::obs_data_set_string(d, dsp::DEVICE.as_ptr(), device.as_ptr());
            let vref =
                std::ffi::CString::new(self.shared.params.vu_ref().as_str()).unwrap_or_default();
            crate::ffi::obs_data_set_string(d, dsp::VU_REF.as_ptr(), vref.as_ptr());
            // ミュート / バイパス（つまみの値はこの上で書き終えている）。
            for (key, m) in [
                (dsp::AMBIENCE_MODE, self.shared.params.ambience_mode()),
                (dsp::VOICE_MODE, self.shared.params.voice_mode()),
                (dsp::VOICE_REVERB_MODE, self.shared.params.voice_reverb_mode()),
            ] {
                let s = std::ffi::CString::new(m.as_str()).unwrap_or_default();
                crate::ffi::obs_data_set_string(d, key.as_ptr(), s.as_ptr());
            }
            crate::ffi::obs_source_update(self.source as *mut _, d);
            crate::ffi::obs_data_release(d);
        }
    }
}

impl Host for ObsHost {
    fn state(&self) -> State {
        let p = &self.shared.params;
        let m = &self.shared.meters;
        State {
            values: [p.ambient(), p.voice(), p.voice_reverb()],
            modes: [p.ambience_mode(), p.voice_mode(), p.voice_reverb_mode()],
            model: p.model(),
            device: p.device(),
            vref: p.vu_ref().dbfs(),
            dereverb: self.shared.dereverb.load(Ordering::Relaxed),
            channels: self.shared.channels.load(Ordering::Relaxed),
            rate: self.shared.rate.load(Ordering::Relaxed),
            status: self.shared.status(),
            status_text: self.shared.status_detail(),
            summary: self.shared.summary(),
            enabled: if self.shared.alive.load(Ordering::SeqCst) && !self.source.is_null() {
                unsafe { crate::ffi::obs_source_enabled(self.source as *mut _) }
            } else {
                true
            },
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
        let p = &self.shared.params;
        match ev {
            Event::Fader { index, value } => match index {
                0 => p.set_ambience(value),
                1 => p.set_voice(value),
                _ => p.set_voice_reverb(value),
            },
            // ドラッグ / ホイール 1 目盛りの確定。OBS の設定へ書き戻す。
            Event::FaderCommit { .. } => self.push_settings(),
            Event::Mode { index, mode } => {
                match index {
                    0 => p.set_ambience_mode(mode),
                    1 => p.set_voice_mode(mode),
                    _ => p.set_voice_reverb_mode(mode),
                }
                self.push_settings();
            }
            Event::ToggleBypass => {
                if self.shared.alive.load(Ordering::SeqCst) && !self.source.is_null() {
                    unsafe {
                        let en = crate::ffi::obs_source_enabled(self.source as *mut _);
                        crate::ffi::obs_source_set_enabled(self.source as *mut _, !en);
                    }
                }
            }
            Event::Model(m) => {
                p.set_model(m);
                self.push_settings();
            }
            Event::Device(d) => {
                p.set_device(d);
                self.push_settings();
            }
            // モデルはエンジンを作り直すので、数十 ms 音が途切れる。
            Event::ApplyPreset(pr) => {
                p.set_ambience(pr.ambience);
                p.set_voice(pr.voice_db);
                p.set_voice_reverb(pr.reverb);
                p.set_ambience_mode(pr.ambience_mode);
                p.set_voice_mode(pr.voice_mode);
                p.set_voice_reverb_mode(pr.reverb_mode);
                p.set_model(pr.model_choice());
                self.push_settings();
            }
        }
    }
}
