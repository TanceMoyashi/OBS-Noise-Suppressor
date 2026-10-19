//! 自前 Win32 パネル（GDI + DIB 描画）を OBS 版 / VST3 版で共有する。
//!
//! 旧 VST3 版の `DPDFNetVoiceEditor`（JUCE）を Rust + 生 Win32 で描き直したもの。
//! OBS 版にあったものをそのまま切り出してあるので、見た目・数値・操作は同じ。
//!
//! ホストとの境目は [`Host`] トレイトだけ:
//!
//!   * [`Host::state`] … パネルに描く値を**引く**（毎ティック呼ぶ）
//!   * [`Host::event`] … ユーザー操作を**押し込む**
//!
//! こうしてあるので、このクレートは OBS のヘッダにも libobs にも依存しない
//! （`dpdfnet` のパラメータ定義だけを借りる）。Win32 / GDI も `windows`
//! クレートを使わず、`ffi` モジュールに必要な分だけ生 FFI で宣言している。
//!
//! ## スレッドモデル
//!
//! **パネルは音声スレッドから触らない。** 描画・入力・メーターの弾道はすべて
//! ウィンドウを持つスレッド（= メッセージループを回すスレッド）だけで動く。
//! 音声スレッドが更新する共有状態は [`Host::state`] 経由でアトミックに読む。
//!
//! * OBS 版: パネル専用スレッドを作り、そこで [`Panel::new`] + [`message_loop`]
//! * VST3 版: ホストの UI スレッドで [`Panel::new`]（子ウィンドウ）。ループは
//!            ホストのものを使うので、`WM_TIMER` で [`Panel::tick`] を回す
//!
//! ## メーターの動き
//!
//! 針の弾道（VU の時定数・ピークホールド・GR の戻り）は [`meter`] にまとめて
//! あり、ここでは 40 ms ごとに 1 ティック進めるだけ。**変化があったときだけ
//! 再描画する**（数値は [`PaintKey`]、文字列は直前の描画と直接比較する）。

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU16, Ordering};

use dpdfnet::params as dsp;
use dpdfnet::paths::{DeviceChoice, ModelChoice};

use crate::ffi::*;

mod draw;
mod ffi;

pub mod meter;
pub mod preset;

pub use preset::{Preset, SLOTS};

use draw::{fader_text, mode_message, Dib, Fonts, FADERS, Layout, Snapshot};
use meter::{MeterState, Reading};

/// ステータスが正常（`Host::state` の `status`）。OBS 版の `filter::ST_OK` と
/// 同じ値。`panel_ui` が OBS に依存しないよう、ここにも定義しておく。
pub const STATUS_OK: u32 = 0;

/// 表示用のレート表記（1000 の倍数は kHz）。ログとパネルで使う。
pub fn rate_label(rate: u32) -> String {
    if rate % 1000 == 0 {
        format!("{} kHz", rate / 1000)
    } else {
        format!("{rate} Hz")
    }
}

/// パネルの論理サイズ（物理サイズ = これ × `Config::scale`）。
///
/// 子ウィンドウ（VST3）はホストにサイズを伝える必要があるので公開している。
pub fn logical_size() -> (u32, u32) {
    (draw::CLIENT_W as u32, draw::CLIENT_H as u32)
}

/// パネルが 1 ティック進める間隔 [ms]。`meter::TICK_MS` と一致させること。
pub const TIMER_MS: u32 = 40;

/// メモリー操作のメッセージを出しておく時間 [ms]。
const FLASH_MS: u64 = 1800;

/// 検証用のパネル画像（`DPDFNET_PANEL_SNAPSHOT`）を書くまでに待つ時間 [ms]。
/// 音が流れてメーターが動いてから撮るため。
const DUMP_AFTER_MS: u64 = 5000;

// --------------------------------------------------------------------------
// ホストとの境目
// --------------------------------------------------------------------------

/// パネルをどこに作るか。
#[derive(Clone, Copy, Debug)]
pub enum Parent {
    /// ホストのウィンドウの**子**として作る（VST3 版）。
    Child(*mut std::ffi::c_void),
    /// 単独のトップレベルウィンドウとして作る（OBS 版）。`owner` があれば
    /// その中央に置く（無ければ画面中央）。
    Standalone { owner: *mut std::ffi::c_void },
}

/// パネルの作り方。
#[derive(Clone, Debug)]
pub struct Config {
    pub parent: Parent,
    /// タイトルバーの文字（単独のときだけ見える）。
    pub title: String,
    /// 論理ピクセル -> 物理ピクセルの倍率。`0` 以下かつ `Standalone` のときは
    /// オーナーの DPI から推定する。
    pub scale: f32,
    /// メモリー（1〜8）の保存先。`None` なら保存しない（プロセス内だけ）。
    pub preset_path: Option<PathBuf>,
    /// フッタに MODEL キーを出すか（VST3 版は true）。
    pub model_key: bool,
    /// フッタに DEV キーを出すか（VST3 版は true）。
    pub device_key: bool,
}

/// パネルが描く値。毎ティック [`Host::state`] が作って返す。
///
/// 文字列は clone になるが、25 Hz なので無視できる（元の OBS 版も描画ごとに
/// 同じ clone をしていた）。
#[derive(Clone, Debug, PartialEq)]
pub struct State {
    /// フェーダー 3 本の値（AMBIENCE 0..1 / VOICE -12..+12 dB / REVERB 0..1）。
    pub values: [f32; 3],
    /// フェーダーごとのミュート / バイパス。
    pub modes: [dsp::FeatureMode; 3],
    pub model: ModelChoice,
    pub device: DeviceChoice,
    /// 0 VU の dBFS（正の数）。
    pub vref: f32,
    /// 2 出力モデル（dereverb あり）か。REVERB フェーダーの有効/無効に使う。
    pub dereverb: bool,
    /// エンジンのチャンネル数（1 = モノラル / 2 = ステレオ / 0 = 未構築）。
    pub channels: usize,
    /// エンジンが想定しているサンプルレート（0 = 未構築）。
    pub rate: u32,
    /// 正常なら [`STATUS_OK`]。
    pub status: u32,
    pub status_text: String,
    /// モデルの概要（パネルに出す 1 行）。
    pub summary: String,
    /// 音が通っているか（OBS のフィルタ有効 / VST3 の BYPASS）。
    pub enabled: bool,
    /// 音声ブロックの通し番号。前のティックと同じなら「無音」として針を戻す。
    pub seq: u64,
    pub in_level: [f32; 2],
    pub in_peak: [f32; 2],
    pub out_level: [f32; 2],
    pub out_peak: [f32; 2],
    /// 出力 3 バンドの生のレベル（0..1 のリニア RMS）。
    pub bands: [f32; 3],
}

impl Default for State {
    fn default() -> Self {
        Self {
            values: [
                dsp::AMBIENCE_DEFAULT as f32,
                dsp::VOICE_DEFAULT as f32,
                dsp::VOICE_REVERB_DEFAULT as f32,
            ],
            modes: [dsp::FeatureMode::Active; 3],
            model: ModelChoice::default(),
            device: DeviceChoice::default(),
            vref: dsp::VuRef::default().dbfs(),
            dereverb: false,
            channels: 0,
            rate: 0,
            status: STATUS_OK,
            status_text: String::new(),
            summary: String::new(),
            enabled: true,
            seq: 0,
            in_level: [0.0; 2],
            in_peak: [0.0; 2],
            out_level: [0.0; 2],
            out_peak: [0.0; 2],
            bands: [0.0; 3],
        }
    }
}

/// ユーザー操作。ホストはこれを自分の流儀で反映する。
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// フェーダーが動いた（ドラッグ中も逐次）。
    Fader { index: usize, value: f32 },
    /// フェーダーの操作が確定した（マウスを離した / ホイール 1 目盛り）。
    /// ホストは設定の永続化と自動化の締めを行う。
    FaderCommit { index: usize },
    /// MUTE / BYP キーが押された。
    Mode {
        index: usize,
        mode: dsp::FeatureMode,
    },
    /// BYPASS キーが押された（全体の有効 / 無効）。
    ToggleBypass,
    Model(ModelChoice),
    Device(DeviceChoice),
    /// メモリーから呼び出した（モデルの作り直しで数十 ms 音が途切れる）。
    ApplyPreset(Preset),
}

/// パネルとホストの境目。
///
/// `Send + Sync` なのは、パネルが `Box<dyn Host>` を持ち、VST3 の `Editor` が
/// `Send` を要求するため（ホスト側は `Arc` などで共有し、内部で同期すること）。
pub trait Host: Send + Sync {
    /// 今描くべき値。毎ティック呼ばれる。
    fn state(&self) -> State;
    /// ユーザー操作。**UI スレッドから**呼ばれる（戻ってきたら反映済みが望ましい）。
    fn event(&self, ev: Event);
}

// --------------------------------------------------------------------------
// 再描画が要るかの判定
// --------------------------------------------------------------------------

/// 数値だけを見た「画面の内容」。`PartialEq` で比較して、変わったときだけ
/// 再描画する（静かな画面を 25 Hz で描き直さない）。
#[derive(Clone, Copy, PartialEq)]
struct PaintKey {
    reading: Reading,
    vref: f32,
    bands: [f32; 3],
    bands_peak: [f32; 3],
    values: [f32; 3],
    modes: [dsp::FeatureMode; 3],
    status: u32,
    dereverb: bool,
    channels: usize,
    rate: u32,
    enabled: bool,
    model: ModelChoice,
    device: DeviceChoice,
    slots_filled: [bool; SLOTS],
    current_slot: Option<usize>,
    save_armed: bool,
    hover_slot: Option<usize>,
    hover_save: bool,
    hover_button: bool,
    press_button: bool,
    hover_fader: Option<usize>,
    hover_mute: Option<usize>,
    hover_byp: Option<usize>,
    hover_model: bool,
    hover_device: bool,
}

// --------------------------------------------------------------------------
// ウィンドウクラス
// --------------------------------------------------------------------------

/// ウィンドウクラスは 1 度だけ登録する。
static CLASS: OnceLock<u16> = OnceLock::new();

/// クラス登録の結果（`u16::MAX` = 未登録）。
static CLASS_FAILED: AtomicU16 = AtomicU16::new(u16::MAX);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn class_name() -> Vec<u16> {
    wide("DpdfnetPanel")
}

fn ensure_class() -> Result<u16, String> {
    if let Some(a) = CLASS.get() {
        return Ok(*a);
    }

    let name = class_name();
    let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
    if hinst.0.is_null() {
        return Err("GetModuleHandleW に失敗".into());
    }

    let atom = unsafe {
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.style = CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS;
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.hCursor = LoadCursorW(HINSTANCE::default(), IDC_ARROW as *const u16);
        wc.hbrBackground = HBRUSH::default();
        wc.lpszClassName = name.as_ptr();
        RegisterClassW(&wc)
    };

    if atom == 0 {
        CLASS_FAILED.store(0, Ordering::SeqCst);
        return Err("RegisterClassW に失敗".into());
    }
    Ok(*CLASS.get_or_init(|| atom))
}

fn screen_w() -> i32 {
    unsafe { GetSystemMetrics(SM_CXSCREEN) }
}

fn screen_h() -> i32 {
    unsafe { GetSystemMetrics(SM_CYSCREEN) }
}

/// オーナーの位置と大きさ `(x, y, w, h)`。無ければ全部 0。
fn owner_rect(hwnd: HWND) -> (i32, i32, i32, i32) {
    if hwnd.0.is_null() {
        return (0, 0, 0, 0);
    }
    let mut r = RECT::default();
    unsafe {
        if GetWindowRect(hwnd, &mut r) == 0 {
            return (0, 0, 0, 0);
        }
    }
    (r.left, r.top, r.right - r.left, r.bottom - r.top)
}

// --------------------------------------------------------------------------
// パネル
// --------------------------------------------------------------------------

struct Drag {
    fader: usize,
}

/// パネル本体。
///
/// `new` が返す `Box<Panel>` はウィンドウと同じアロケーションで、`WM_NCCREATE`
/// から `GWLP_USERDATA` 経由で参照される。**移動しないこと**。
pub struct Panel {
    host: Box<dyn Host>,
    hwnd: HWND,
    scale: f32,
    child: bool,
    model_key: bool,
    device_key: bool,
    preset_path: Option<PathBuf>,

    /// 直近にホストから引いた値。入力の処理はこれを楽観更新して即時反映する。
    state: State,
    dib: Option<Dib>,
    fonts: Option<Fonts>,
    layout: Layout,

    /// メーターの弾道（[`meter`]）。
    meter: MeterState,
    /// 直近のティックで計算した表示値。
    reading: Reading,
    /// 最後に見た音声ブロックの通し番号（無音検出）。
    last_seq: u64,
    /// 直近のティックで見たモデル。変わったらエンジンが作り直されているので
    /// メーターの状態を捨てる。
    last_model: ModelChoice,
    bands_display: [f32; 3],
    bands_peak: [f32; 3],

    /// メモリー（プリセット）1〜8。
    slots: [Option<Preset>; SLOTS],
    /// 最後に呼び出した / 保存したスロット。
    current_slot: Option<usize>,
    /// SAVE を押して「次に押したスロットへ保存」待ち。
    save_armed: bool,
    hover_slot: Option<usize>,
    hover_save: bool,
    /// 一時メッセージと、その表示を始めた時刻（`GetTickCount64` の ms）。
    flash: Option<(String, u64)>,

    drag: Option<Drag>,
    /// ドラッグ中に触ったフェーダー（離したときに確定を通知する）。
    pending_push: Option<usize>,
    hover_button: bool,
    press_button: bool,
    hover_fader: Option<usize>,
    /// マウスが乗っているミュート / バイパスキー。
    hover_mute: Option<usize>,
    hover_byp: Option<usize>,
    /// マウスが乗っているモデル / デバイスキー。
    hover_model: bool,
    hover_device: bool,

    /// 前回描いた内容（変化の検出だけに使う）。
    paint_key: Option<PaintKey>,
    last_status_text: String,
    last_summary: String,
    last_flash: Option<String>,

    /// 検証用の画像ダンプ: パネルを作った時刻と、もう書いたか。
    opened_at_ms: u64,
    dumped: bool,
}

// `HWND` / `HFONT` などは生ポインタを含むので `Send` でない。ウィンドウを
// 持つスレッドでしか触らないことを我々が保証するので、包んで渡せるようにする。
unsafe impl Send for Panel {}

impl Panel {
    /// パネルを作って表示する。**メッセージループを回すスレッドから呼ぶこと**
    /// （Win32 のウィンドウは作ったスレッドに所属する）。
    pub fn new(host: Box<dyn Host>, config: Config) -> Result<Box<Panel>, String> {
        ensure_class()?;

        let child = matches!(config.parent, Parent::Child(_));

        // 倍率。単独のときだけ DPI から推定できる（オーナーが要る）。
        let mut scale = config.scale;
        if !(scale > 0.0) {
            scale = if let Parent::Standalone { owner } = config.parent {
                let dpi = unsafe { GetDpiForWindow(Handle(owner)) };
                if dpi > 0 {
                    dpi as f32 / 96.0
                } else {
                    1.0
                }
            } else {
                1.0
            };
        }

        let slots = match &config.preset_path {
            Some(p) => preset::load(p),
            None => Default::default(),
        };

        let panel = Box::new(Panel {
            host,
            hwnd: HWND::default(),
            scale,
            child,
            model_key: config.model_key,
            device_key: config.device_key,
            preset_path: config.preset_path.clone(),
            state: State::default(),
            dib: None,
            fonts: None,
            layout: Layout::default(),
            meter: MeterState::new(),
            reading: Reading::default(),
            last_seq: u64::MAX,
            last_model: ModelChoice::default(),
            bands_display: [0.0; 3],
            bands_peak: [0.0; 3],
            slots,
            current_slot: None,
            save_armed: false,
            hover_slot: None,
            hover_save: false,
            flash: None,
            drag: None,
            pending_push: None,
            hover_button: false,
            press_button: false,
            hover_fader: None,
            hover_mute: None,
            hover_byp: None,
            hover_model: false,
            hover_device: false,
            paint_key: None,
            last_status_text: String::new(),
            last_summary: String::new(),
            last_flash: None,
            opened_at_ms: unsafe { GetTickCount64() },
            dumped: false,
        });
        let param = Box::into_raw(panel);

        let class = class_name();
        let title = wide(&config.title);
        let sc = |v: f32| (v * scale).round() as i32;

        let (style, ex_style, x, y, w, h, parent) = if child {
            let p = match config.parent {
                Parent::Child(p) => Handle(p),
                _ => unreachable!(),
            };
            (
                WS_CHILD | WS_VISIBLE,
                0u32,
                0,
                0,
                sc(draw::CLIENT_W),
                sc(draw::CLIENT_H),
                p,
            )
        } else {
            let owner = match config.parent {
                Parent::Standalone { owner } => Handle(owner),
                _ => HWND::default(),
            };
            let (ox, oy, ow, oh) = owner_rect(owner);
            let w = sc(draw::CLIENT_W) + 64;
            let h = sc(draw::CLIENT_H) + 64;
            let x = if ow > 0 { ox + (ow - w) / 2 } else { (screen_w() - w) / 2 };
            let y = if oh > 0 { oy + (oh - h) / 2 } else { (screen_h() - h) / 2 };
            (
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX,
                WS_EX_TOOLWINDOW,
                x,
                y,
                w,
                h,
                owner,
            )
        };

        let created = unsafe {
            CreateWindowExW(
                ex_style,
                class.as_ptr(),
                title.as_ptr(),
                style,
                x,
                y,
                w,
                h,
                parent,
                Handle::default(),
                Handle::default(),
                param as *const std::ffi::c_void,
            )
        };

        if created.0.is_null() {
            // `WM_NCCREATE` に到達していないので、こちらで解放する。
            unsafe { drop(Box::from_raw(param)) };
            return Err("CreateWindowExW に失敗".into());
        }

        // `WM_NCCREATE` / `WM_CREATE` は同期で走る。同じアロケーションを
        // そのまま回収する（移動しない）。
        let mut panel = unsafe { Box::from_raw(param) };

        // 最初の 1 枚をここで描いておく（`WM_PAINT` を待たない）。
        panel.mark_painted();

        if !child {
            unsafe {
                let _ = ShowWindow(created, SW_SHOW);
                let _ = SetForegroundWindow(created);
            }
        }

        Ok(panel)
    }

    /// ウィンドウハンドル（単独ウィンドウのときはホストが前面に出すのにも使う）。
    pub fn hwnd(&self) -> *mut std::ffi::c_void {
        self.hwnd.0
    }

    /// 「閉じる」を要求する（メッセージループへ `WM_CLOSE` を投げるだけ）。
    pub fn request_close(&self) {
        if !self.hwnd.0.is_null() {
            unsafe {
                let _ = PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
            }
        }
    }

    /// 明示的に破棄する。`Drop` からも呼ばれる。
    pub fn destroy(&mut self) {
        if !self.hwnd.0.is_null() {
            // 先に `GWLP_USERDATA` を外す（`WM_NCDESTROY` で自分を触らない）。
            unsafe {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                let _ = DestroyWindow(self.hwnd);
            }
            self.hwnd = HWND::default();
        }
    }

    /// ホスト側から「描き直して」と頼む（パラメータが外から変わったときなど）。
    /// 実際に描くかどうかは次のティックの比較で決まる。
    pub fn invalidate(&self) {
        if !self.hwnd.0.is_null() {
            unsafe {
                let _ = InvalidateRect(self.hwnd, std::ptr::null(), 0);
            }
        }
    }

    /// 1 ティック進める（メーターの弾道 + 変化があれば再描画）。
    ///
    /// `now_ms` は単調増加のミリ秒（`GetTickCount64` など）。メッセージループを
    /// 自分で持たないホスト（VST3）は `WM_TIMER` から呼ぶ。
    pub fn tick(&mut self, now_ms: u64) {
        self.state = self.host.state();
        self.advance_meter();
        if let Some((_, t)) = &self.flash {
            if now_ms.wrapping_sub(*t) > FLASH_MS {
                self.flash = None;
            }
        }
        self.maybe_dump(now_ms);
        if self.needs_repaint() {
            self.invalidate();
        }
    }

    // ----------------------------------------------------------------------
    // 変化の検出
    // ----------------------------------------------------------------------

    fn current_key(&self) -> PaintKey {
        let mut slots_filled = [false; SLOTS];
        for i in 0..SLOTS {
            slots_filled[i] = self.slots[i].is_some();
        }
        PaintKey {
            reading: self.reading,
            vref: self.state.vref,
            bands: self.bands_display,
            bands_peak: self.bands_peak,
            values: self.state.values,
            modes: self.state.modes,
            status: self.state.status,
            dereverb: self.state.dereverb,
            channels: self.state.channels,
            rate: self.state.rate,
            enabled: self.state.enabled,
            model: self.state.model,
            device: self.state.device,
            slots_filled,
            current_slot: self.current_slot,
            save_armed: self.save_armed,
            hover_slot: self.hover_slot,
            hover_save: self.hover_save,
            hover_button: self.hover_button,
            press_button: self.press_button,
            hover_fader: self.hover_fader,
            hover_mute: self.hover_mute,
            hover_byp: self.hover_byp,
            hover_model: self.hover_model,
            hover_device: self.hover_device,
        }
    }

    /// 「前回描いた内容」を今の内容として記録する。
    fn mark_painted(&mut self) {
        self.paint_key = Some(self.current_key());
        self.last_status_text = self.state.status_text.clone();
        self.last_summary = self.state.summary.clone();
        self.last_flash = self.flash.as_ref().map(|(s, _)| s.clone());
    }

    /// 前回描いた内容と違うか。
    fn needs_repaint(&self) -> bool {
        if self.paint_key != Some(self.current_key()) {
            return true;
        }
        if self.state.status_text != self.last_status_text
            || self.state.summary != self.last_summary
        {
            return true;
        }
        self.flash.as_ref().map(|(s, _)| s.as_str()) != self.last_flash.as_deref()
    }

    // ----------------------------------------------------------------------
    // メーター
    // ----------------------------------------------------------------------

    fn advance_meter(&mut self) {
        // 前回のティックから新しい音声ブロックが来ていなければ「無音」として
        // 扱う。こうすると親ソースが止まったときも針が静止位置へ戻る。
        let seq = self.state.seq;
        let live = seq != self.last_seq;
        self.last_seq = seq;

        // モデルが変わったらエンジンは作り直されている（数十 ms 無音）。
        // 前のモデルの値を針に残さないよう、弾道をいったん捨てる。
        if self.state.model != self.last_model {
            self.last_model = self.state.model;
            self.meter.reset();
            self.bands_display = [0.0; 3];
            self.bands_peak = [0.0; 3];
        }

        self.reading = self.meter.tick(
            live,
            self.state.in_level,
            self.state.in_peak,
            self.state.out_level,
            self.state.out_peak,
            self.state.channels,
        );

        // ---- 出力 3 バンド ----
        for b in 0..3 {
            let target = if live {
                meter::lin_to_norm(self.state.bands[b])
            } else {
                0.0
            };
            // 立ち上がりは即時、戻りは滑らかに（JUCE 版と同じ）。
            let cur = self.bands_display[b];
            self.bands_display[b] = if target > cur { target } else { cur + (target - cur) * 0.25 };
            self.bands_peak[b] = (self.bands_peak[b] - 0.02).max(self.bands_display[b]);
        }
    }

    // ----------------------------------------------------------------------
    // 描画
    // ----------------------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        let mut slots = [false; SLOTS];
        for i in 0..SLOTS {
            slots[i] = self.slots[i].is_some();
        }
        let v = self.state.values;
        Snapshot {
            scale: self.scale,
            layout: self.layout,
            reading: self.reading,
            vref: self.state.vref,
            bands: self.bands_display,
            bands_peak: self.bands_peak,
            values: v,
            fader_text: [fader_text(0, v[0]), fader_text(1, v[1]), fader_text(2, v[2])],
            modes: self.state.modes,
            status: self.state.status,
            status_text: self.state.status_text.clone(),
            summary: self.state.summary.clone(),
            flash: self.flash.as_ref().map(|(s, _)| s.clone()),
            dereverb: self.state.dereverb,
            channels: self.state.channels,
            rate: self.state.rate,
            enabled: self.state.enabled,
            model: self.state.model,
            slots,
            current_slot: self.current_slot,
            save_armed: self.save_armed,
            hover_slot: self.hover_slot,
            hover_save: self.hover_save,
            hover_button: self.hover_button,
            press_button: self.press_button,
            hover_fader: self.hover_fader,
            hover_mute: self.hover_mute,
            hover_byp: self.hover_byp,
            hover_model: self.hover_model,
            hover_device: self.hover_device,
            model_key: self.model_key,
            device_key: self.device_key,
            model_label: format!("MODEL {}", self.state.model.as_str().to_uppercase()),
            device_label: format!("DEV {}", self.state.device.label()),
        }
    }

    /// DIB に今の内容を描く（ウィンドウへは転送しない）。
    fn render(&mut self) {
        if self.dib.is_some() && self.fonts.is_some() {
            let snap = self.snapshot();
            let dib = self.dib.as_mut();
            let fonts = self.fonts.as_ref();
            if let (Some(dib), Some(fonts)) = (dib, fonts) {
                draw::paint(dib, fonts, &snap);
            }
        }
        self.mark_painted();
    }

    /// DIB に描いて `target` へ転送する。
    fn blit(&mut self, target: HDC) {
        self.render();

        let Some(dib) = &self.dib else {
            return;
        };
        let (w, h) = (dib.w, dib.h);
        unsafe {
            let _ = BitBlt(target, 0, 0, w, h, dib.dc, 0, 0, SRCCOPY);
        }
    }

    /// 検証用: 開いてから `DUMP_AFTER_MS` 経ったら 1 度だけ DIB を BMP に残す。
    ///
    /// `DPDFNET_PANEL_SNAPSHOT` にパスを入れたときだけ動く。スクリーンショットと
    /// 違い、他のウィンドウに隠されていても取れる。
    fn maybe_dump(&mut self, now_ms: u64) {
        if self.dumped || now_ms.wrapping_sub(self.opened_at_ms) < DUMP_AFTER_MS {
            return;
        }
        let Some(path) = std::env::var_os("DPDFNET_PANEL_SNAPSHOT") else {
            return;
        };
        self.dumped = true;
        self.render();
        if let Some(dib) = self.dib.as_ref() {
            let _ = draw::write_bmp(dib, std::path::Path::new(&path));
        }
    }

    #[inline]
    fn sc(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    fn flash_msg(&mut self, s: impl Into<String>) {
        self.flash = Some((s.into(), unsafe { GetTickCount64() }));
        self.invalidate();
    }

    /// ウィンドウを作らないパネル（テスト用）。入力と状態の確認だけに使う。
    /// `on_create` / `on_size` / 描画 / `SetCapture` は呼ばれない（`hwnd` が
    /// null のままだとそれらは何もしないか無害）。
    #[cfg(test)]
    fn new_windowless(host: Box<dyn Host>, config: Config) -> Box<Panel> {
        let scale = if config.scale > 0.0 { config.scale } else { 1.0 };
        let slots = match &config.preset_path {
            Some(p) => preset::load(p),
            None => Default::default(),
        };
        let w = (draw::CLIENT_W * scale).round() as i32;
        let h = (draw::CLIENT_H * scale).round() as i32;
        let layout = draw::compute_layout(scale, w, h, config.model_key, config.device_key);
        Box::new(Panel {
            host,
            hwnd: HWND::default(),
            scale,
            child: matches!(config.parent, Parent::Child(_)),
            model_key: config.model_key,
            device_key: config.device_key,
            preset_path: config.preset_path,
            state: State::default(),
            dib: None,
            fonts: None,
            layout,
            meter: MeterState::new(),
            reading: Reading::default(),
            last_seq: u64::MAX,
            last_model: ModelChoice::default(),
            bands_display: [0.0; 3],
            bands_peak: [0.0; 3],
            slots,
            current_slot: None,
            save_armed: false,
            hover_slot: None,
            hover_save: false,
            flash: None,
            drag: None,
            pending_push: None,
            hover_button: false,
            press_button: false,
            hover_fader: None,
            hover_mute: None,
            hover_byp: None,
            hover_model: false,
            hover_device: false,
            paint_key: None,
            last_status_text: String::new(),
            last_summary: String::new(),
            last_flash: None,
            opened_at_ms: unsafe { GetTickCount64() },
            dumped: false,
        })
    }

    // ----------------------------------------------------------------------
    // ウィンドウイベント
    // ----------------------------------------------------------------------

    unsafe fn on_create(&mut self) {
        let _ = SetTimer(self.hwnd, 1, TIMER_MS, None);

        // 単独ウィンドウは枠ぶんずれるので、クライアント領域を論理
        // CLIENT_W x CLIENT_H ちょうどにする。子ウィンドウは枠が無いので不要。
        if !self.child {
            let mut rc = RECT::default();
            if GetClientRect(self.hwnd, &mut rc) != 0 {
                let dw = self.sc(draw::CLIENT_W) - (rc.right - rc.left);
                let dh = self.sc(draw::CLIENT_H) - (rc.bottom - rc.top);
                if dw != 0 || dh != 0 {
                    let mut wr = RECT::default();
                    if GetWindowRect(self.hwnd, &mut wr) != 0 {
                        let _ = SetWindowPos(
                            self.hwnd,
                            HWND::default(),
                            0,
                            0,
                            (wr.right - wr.left) + dw,
                            (wr.bottom - wr.top) + dh,
                            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                }
            }
        }

        self.on_size();
    }

    unsafe fn on_size(&mut self) {
        let mut rc = RECT::default();
        if GetClientRect(self.hwnd, &mut rc) == 0 {
            return;
        }
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        if w <= 0 || h <= 0 {
            return;
        }

        if self.dib.as_ref().map(|d| (d.w, d.h)) != Some((w, h)) {
            match Dib::new(w, h) {
                Ok(d) => self.dib = Some(d),
                Err(_) => return,
            }
        }

        if self.fonts.as_ref().map(|f| f.scale) != Some(self.scale) {
            self.fonts = None;
        }
        if self.fonts.is_none() {
            self.fonts = Some(Fonts::new(self.scale));
        }

        self.layout = draw::compute_layout(self.scale, w, h, self.model_key, self.device_key);
        self.invalidate();
    }

    fn on_destroy(&mut self) {
        unsafe {
            let _ = KillTimer(self.hwnd, 1);
        }
        // DIB / フォントは `Drop` でも解放されるが、ここで先に手放しておく。
        self.dib = None;
        self.fonts = None;
    }

    fn set_scale(&mut self, s: f32) {
        if (s - self.scale).abs() > 0.01 {
            self.scale = s;
            self.fonts = None;
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        // GDI リソースは `Dib` / `Fonts` の Drop で解放される。
        self.destroy();
    }
}

impl Panel {
    // ----------------------------------------------------------------------
    // メッセージ
    // ----------------------------------------------------------------------

    /// 1 メッセージを処理する。`None` を返したら呼び出し側が `DefWindowProcW`    /// に流す（= 我々が扱わないメッセージ）。
    ///
    /// `WM_NCCREATE` は `wndproc` の側で先に処理する（そこだけ `self` がまだ
    /// 無い。`GWLP_USERDATA` にポインタを入れるのはそのため）。
    unsafe fn handle_message(&mut self, msg: u32, w: usize, l: isize) -> Option<isize> {
        // マウス座標（`WM_LBUTTON*` / `WM_MOUSEMOVE` の `lParam`）。
        let x = (l & 0xFFFF) as i16 as i32;
        let y = ((l >> 16) & 0xFFFF) as i16 as i32;

        match msg {
            WM_CREATE => {
                self.on_create();
                Some(0)
            }
            WM_SIZE => {
                self.on_size();
                Some(0)
            }
            WM_GETMINMAXINFO => {
                // 子ウィンドウの大きさはホストが決める。潰れたら読めなくなる
                // のは単独のときだけ防ぐ。
                if self.child {
                    return None;
                }
                let mm = &mut *(l as *mut MINMAXINFO);
                mm.ptMinTrackSize.x = self.sc(draw::MIN_W);
                mm.ptMinTrackSize.y = self.sc(draw::MIN_H);
                Some(0)
            }
            WM_DPICHANGED => {
                if self.child {
                    return None;
                }
                let dpi = (w & 0xFFFF) as u32;
                self.set_scale(dpi as f32 / 96.0);
                self.on_size();
                Some(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(self.hwnd, &mut ps);
                self.blit(hdc);
                let _ = EndPaint(self.hwnd, &ps);
                Some(0)
            }
            WM_ERASEBKGND => Some(1),
            WM_TIMER => {
                self.tick(GetTickCount64());
                Some(0)
            }
            WM_LBUTTONDOWN => {
                self.on_mouse_down(x, y, w);
                Some(0)
            }
            WM_LBUTTONDBLCLK => {
                self.on_mouse_dbl(x, y);
                Some(0)
            }
            WM_LBUTTONUP => {
                self.on_mouse_up();
                Some(0)
            }
            WM_MOUSEMOVE => {
                self.on_mouse_move(x, y);
                Some(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((w >> 16) & 0xFFFF) as i16 as i32;
                self.on_wheel(delta);
                Some(0)
            }
            WM_KEYDOWN => {
                // 子ウィンドウ（VST3 版）ではキーを取らない。ホストの
                // ショートカットを奪わないため。
                if self.child {
                    return Some(0);
                }
                match w as u32 {
                    // ESC で閉じる。
                    0x1B => {
                        let _ = DestroyWindow(self.hwnd);
                    }
                    // S = 保存モードの切り替え / R = 保存モード解除。
                    0x53 => self.toggle_save_mode(),
                    0x52 => {
                        self.save_armed = false;
                        self.invalidate();
                    }
                    _ => {}
                }
                Some(0)
            }
            WM_CLOSE => {
                // 子ウィンドウはホストが破棄する。ここで閉じると DAW の
                // 都合と食い違うので、何もしない。
                if self.child {
                    return Some(0);
                }
                let _ = DestroyWindow(self.hwnd);
                Some(0)
            }
            WM_DESTROY => {
                self.on_destroy();
                // 子ウィンドウではメッセージループを持たないので投げない。
                if !self.child {
                    PostQuitMessage(0);
                }
                Some(0)
            }
            WM_NCDESTROY => {
                // ポインタは呼び出し側（`Box<Panel>`）が所有する。ここでは
                // 参照を外すだけ。
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                self.hwnd = HWND::default();
                Some(0)
            }
            _ => None,
        }
    }

    // ----------------------------------------------------------------------
    // 入力
    // ----------------------------------------------------------------------

    fn on_mouse_down(&mut self, x: i32, y: i32, keys: usize) {
        self.state = self.host.state();

        // ---- メモリー（押した瞬間に効かせる） ----
        if draw::hit(&self.layout.save_rect(), x, y) {
            self.toggle_save_mode();
            return;
        }
        for i in 0..SLOTS {
            if draw::hit(&self.layout.slot_rect(i), x, y) {
                // Ctrl を押しながらでスロットを消す。
                let ctrl = keys & MK_CONTROL != 0;
                self.memory_click(i, ctrl);
                return;
            }
        }

        // ---- 有効 / 無効（BYPASS） ----
        if draw::hit(&self.layout.button_rect(), x, y) {
            self.press_button = true;
            unsafe {
                SetCapture(self.hwnd);
            }
            return;
        }

        // ---- MODEL / DEV キー（VST3 版のみ描いてある） ----
        if self.model_key && draw::hit(&self.layout.model_key_rect(), x, y) {
            self.cycle_model();
            return;
        }
        if self.device_key && draw::hit(&self.layout.device_key_rect(), x, y) {
            self.cycle_device();
            return;
        }

        // ---- フェーダーのミュート / バイパス（つまみの位置は保つ） ----
        for i in 0..3 {
            if !self.fader_enabled(i) {
                continue;
            }
            if draw::hit(&self.layout.mute_rect(i), x, y) {
                self.toggle_mode(i, dsp::FeatureMode::Mute);
                return;
            }
            if draw::hit(&self.layout.byp_rect(i), x, y) {
                self.toggle_mode(i, dsp::FeatureMode::Bypass);
                return;
            }
        }

        // ---- フェーダー ----
        for i in 0..3 {
            if self.fader_enabled(i) && draw::hit(&self.layout.fader_rect(i), x, y) {
                // フェーダーはクリックした位置へ飛ぶ（ミキサーと同じ）。
                self.drag = Some(Drag { fader: i });
                self.set_fader_from_y(i, y, false);
                unsafe {
                    SetCapture(self.hwnd);
                }
                return;
            }
        }
    }

    fn on_mouse_dbl(&mut self, x: i32, y: i32) {
        self.state = self.host.state();

        for i in 0..3 {
            if self.fader_enabled(i) && draw::hit(&self.layout.fader_rect(i), x, y) {
                // ダブルクリックで既定値へ（ミキサーのフェーダーと同じ）。
                self.apply_fader(i, FADERS[i].default(), true);
                return;
            }
        }

        // メーター（IN / OUT）のダブルクリックでピークホールドを消す。
        for ch in 0..2 {
            if draw::hit(&self.layout.vu_rect(ch), x, y)
                || draw::hit(&self.layout.vu_num_rect(ch), x, y)
            {
                self.meter.reset();
                self.invalidate();
                return;
            }
        }
    }

    fn on_mouse_up(&mut self) {
        unsafe {
            let _ = ReleaseCapture();
        }

        if self.press_button {
            self.press_button = false;
            self.host.event(Event::ToggleBypass);
        }

        self.drag = None;

        // ドラッグで動かした値はここで確定したことをホストへ伝える
        // （永続化と自動化の締め）。
        if let Some(i) = self.pending_push.take() {
            self.host.event(Event::FaderCommit { index: i });
        }
    }

    fn on_mouse_move(&mut self, x: i32, y: i32) {
        if let Some(d) = &self.drag {
            let fader = d.fader;
            self.set_fader_from_y(fader, y, false);
            // ドラッグ中はつまみを光らせ続ける。
            if self.hover_fader != Some(fader) {
                self.hover_fader = Some(fader);
            }
            return;
        }

        self.state = self.host.state();

        let hb = draw::hit(&self.layout.button_rect(), x, y);
        let hf = (0..3).find(|i| draw::hit(&self.layout.fader_rect(*i), x, y) && self.fader_enabled(*i));
        let hm = (0..3).find(|i| draw::hit(&self.layout.mute_rect(*i), x, y) && self.fader_enabled(*i));
        let hy = (0..3).find(|i| draw::hit(&self.layout.byp_rect(*i), x, y) && self.fader_enabled(*i));
        let hs = (0..SLOTS).find(|i| draw::hit(&self.layout.slot_rect(*i), x, y));
        let hv = draw::hit(&self.layout.save_rect(), x, y);
        let hmo = self.model_key && draw::hit(&self.layout.model_key_rect(), x, y);
        let hde = self.device_key && draw::hit(&self.layout.device_key_rect(), x, y);
        if hb != self.hover_button
            || hf != self.hover_fader
            || hm != self.hover_mute
            || hy != self.hover_byp
            || hs != self.hover_slot
            || hv != self.hover_save
            || hmo != self.hover_model
            || hde != self.hover_device
        {
            self.hover_button = hb;
            self.hover_fader = hf;
            self.hover_mute = hm;
            self.hover_byp = hy;
            self.hover_slot = hs;
            self.hover_save = hv;
            self.hover_model = hmo;
            self.hover_device = hde;
            self.invalidate();
        }
    }

    fn on_wheel(&mut self, delta: i32) {
        self.state = self.host.state();

        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(self.hwnd, &mut pt);
        }
        for i in 0..3 {
            if self.fader_enabled(i) && draw::hit(&self.layout.fader_rect(i), pt.x, pt.y) {
                let span = FADERS[i].max() - FADERS[i].min();
                let step = span * 0.02 * (delta as f32 / 120.0);
                let v = (self.state.values[i] + step).clamp(FADERS[i].min(), FADERS[i].max());
                self.apply_fader(i, v, true);
                return;
            }
        }
    }

    /// フェーダーのトラック上の y から値を決める（クリック / ドラッグ共用）。
    fn set_fader_from_y(&mut self, i: usize, y: i32, push: bool) {
        let tr = self.layout.fader_track_rect(i);
        let hh = self.sc(draw::K_FADER_HANDLE_H);
        let travel = (tr.h() - hh).max(1) as f32;
        let top = (tr.t + hh / 2) as f32;
        let t = (1.0 - (y as f32 - top) / travel).clamp(0.0, 1.0);

        let spec = &FADERS[i];
        self.apply_fader(i, spec.min() + t * (spec.max() - spec.min()), push);
    }

    // ----------------------------------------------------------------------
    // パラメータ
    // ----------------------------------------------------------------------

    #[inline]
    fn fader_enabled(&self, i: usize) -> bool {
        // REVERB は 2 出力モデルのときだけ有効（JUCE 版と同じ）。
        i != 2 || self.state.dereverb
    }

    #[inline]
    fn fader_mode(&self, i: usize) -> dsp::FeatureMode {
        self.state.modes[i]
    }

    /// MUTE / BYP キーを押したときの切り替え。
    ///
    /// 同じキーをもう一度押すと有効へ戻る。MUTE と BYP は排他で、片方を
    /// 押すともう片方は落ちる（3 状態のロータリースイッチのつもり）。
    /// **つまみの値には触らない。**
    fn toggle_mode(&mut self, i: usize, m: dsp::FeatureMode) {
        let next = if self.fader_mode(i) == m {
            dsp::FeatureMode::Active
        } else {
            m
        };
        self.state.modes[i] = next;
        // 手で触ったら「呼び出したスロット」の表示は消す。
        self.current_slot = None;
        self.host.event(Event::Mode { index: i, mode: next });
        self.flash_msg(mode_message(i, next));
    }

    fn apply_fader(&mut self, i: usize, v: f32, push: bool) {
        let v = v.clamp(FADERS[i].min(), FADERS[i].max());
        self.state.values[i] = v;
        // 手で触ったら「呼び出したスロット」の表示は消す。
        self.current_slot = None;
        self.host.event(Event::Fader { index: i, value: v });
        if push {
            self.host.event(Event::FaderCommit { index: i });
        } else {
            self.pending_push = Some(i);
        }
        self.invalidate();
    }

    /// MODEL キー: Auto -> dp8 -> dp2 -> Auto と巡回する。
    ///
    /// ドロップダウンは作らない（README の既知の制限に書いてある）。
    fn cycle_model(&mut self) {
        let next = match self.state.model {
            ModelChoice::Auto => ModelChoice::Quality,
            ModelChoice::Quality => ModelChoice::Fast,
            ModelChoice::Fast => ModelChoice::Auto,
        };
        self.state.model = next;
        self.current_slot = None;
        self.host.event(Event::Model(next));
        self.flash_msg(format!("MODEL: {}", next.as_str()));
    }

    /// DEV キー: CPU -> GPU -> CPU と巡回する。
    fn cycle_device(&mut self) {
        let next = match self.state.device {
            DeviceChoice::Cpu => DeviceChoice::Gpu,
            DeviceChoice::Gpu => DeviceChoice::Cpu,
        };
        self.state.device = next;
        self.current_slot = None;
        self.host.event(Event::Device(next));
        self.flash_msg(format!("DEVICE: {}", next.label()));
    }

    // ----------------------------------------------------------------------
    // メモリー（プリセット）
    // ----------------------------------------------------------------------

    /// 今の設定をプリセットにする。
    fn current_preset(&self) -> Preset {
        Preset {
            ambience: self.state.values[0],
            voice_db: self.state.values[1],
            reverb: self.state.values[2],
            ambience_mode: self.state.modes[0],
            voice_mode: self.state.modes[1],
            reverb_mode: self.state.modes[2],
            model: self.state.model.as_str().to_string(),
        }
    }

    /// プリセットを適用する（モデルの作り直しで数十 ms 音が途切れる）。
    fn apply_preset(&mut self, p: &Preset) {
        self.state.values = [p.ambience, p.voice_db, p.reverb];
        self.state.modes = [p.ambience_mode, p.voice_mode, p.reverb_mode];
        self.state.model = p.model_choice();
        self.host.event(Event::ApplyPreset(p.clone()));
    }

    fn toggle_save_mode(&mut self) {
        self.save_armed = !self.save_armed;
        if self.save_armed {
            self.flash_msg("SAVE 待機中 — 保存先のスロットを押してください");
        } else {
            self.flash_msg("保存を取り消しました");
        }
    }

    fn memory_click(&mut self, i: usize, erase: bool) {
        if erase {
            if self.slots[i].is_some() {
                self.slots[i] = None;
                if self.current_slot == Some(i) {
                    self.current_slot = None;
                }
                self.store_slots();
                self.flash_msg(format!("MEMORY {} を消去しました", i + 1));
            } else {
                self.flash_msg(format!("MEMORY {} は空です", i + 1));
            }
            return;
        }

        if self.save_armed {
            self.slots[i] = Some(self.current_preset());
            self.current_slot = Some(i);
            self.save_armed = false;
            self.store_slots();
            self.flash_msg(format!("MEMORY {} に保存しました", i + 1));
            return;
        }

        match self.slots[i].clone() {
            Some(p) => {
                self.apply_preset(&p);
                self.current_slot = Some(i);
                self.flash_msg(format!("MEMORY {} を呼び出しました", i + 1));
            }
            None => self.flash_msg(format!("MEMORY {} は空です（SAVE で保存）", i + 1)),
        }
    }

    fn store_slots(&mut self) {
        let Some(path) = self.preset_path.clone() else {
            return;
        };
        if let Err(e) = preset::store(&path, &self.slots) {
            self.flash_msg(format!("保存に失敗しました: {e}"));
        }
    }
}

// --------------------------------------------------------------------------
// ウィンドウプロシージャ
// --------------------------------------------------------------------------

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: usize, l: isize) -> isize {
    if msg == WM_NCCREATE {
        let cs = &*(l as *const CREATESTRUCTW);
        let panel = cs.lpCreateParams as *mut Panel;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, panel as isize);
        if !panel.is_null() {
            (*panel).hwnd = hwnd;
        }
        // 0 を返すとウィンドウ生成が失敗するので 1 を返す。
        return 1;
    }

    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Panel;
    if p.is_null() {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let panel = &mut *p;

    match panel.handle_message(msg, w, l) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, w, l),
    }
}

// --------------------------------------------------------------------------
// 公開ヘルパ
// --------------------------------------------------------------------------

/// メッセージループを回す（単独ウィンドウを使うホスト = OBS 版）。
/// `WM_QUIT` で戻り、その終了コードを返す。
pub fn message_loop() -> i32 {
    let mut msg = MSG::default();
    loop {
        let r = unsafe { GetMessageW(&mut msg, HWND::default(), 0, 0) };
        if r <= 0 {
            return r;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 呼び出し元スレッドの前面ウィンドウ（OBS 版が owner に使う）。
pub fn active_window() -> *mut std::ffi::c_void {
    unsafe { GetActiveWindow().0 }
}

/// 別スレッドからパネルへ「閉じろ」と投げる（OBS 版の `close_panel`）。
pub fn post_close(hwnd: *mut std::ffi::c_void) {
    if !hwnd.is_null() {
        unsafe {
            let _ = PostMessageW(Handle(hwnd), WM_CLOSE, 0, 0);
        }
    }
}

// --------------------------------------------------------------------------
// テスト
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// テスト用のホスト。`event` を状態に反映するので、パネルの楽観更新と
    /// 次の `state()` が食い違わない。
    pub(crate) struct MockHost {
        state: Mutex<State>,
        events: Mutex<Vec<Event>>,
    }

    impl MockHost {
        fn new() -> Self {
            Self {
                state: Mutex::new(State::default()),
                events: Mutex::new(Vec::new()),
            }
        }
        fn events(&self) -> Vec<Event> {
            self.events.lock().unwrap().clone()
        }
        fn take_events(&self) -> Vec<Event> {
            std::mem::take(&mut *self.events.lock().unwrap())
        }
    }

    impl Host for MockHost {
        fn state(&self) -> State {
            self.state.lock().unwrap().clone()
        }
        fn event(&self, ev: Event) {
            {
                let mut st = self.state.lock().unwrap();
                match &ev {
                    Event::Fader { index, value } => st.values[*index] = *value,
                    Event::Mode { index, mode } => st.modes[*index] = *mode,
                    Event::Model(m) => st.model = *m,
                    Event::Device(d) => st.device = *d,
                    Event::ApplyPreset(p) => {
                        st.values = [p.ambience, p.voice_db, p.reverb];
                        st.modes = [p.ambience_mode, p.voice_mode, p.reverb_mode];
                        st.model = p.model_choice();
                    }
                    Event::ToggleBypass => st.enabled = !st.enabled,
                    Event::FaderCommit { .. } => {}
                }
            }
            self.events.lock().unwrap().push(ev);
        }
    }

    #[cfg(test)]
    fn panel(host: &std::sync::Arc<MockHost>) -> Box<Panel> {
        // ウィンドウの無いパネル。描画はしない（入力と状態だけ）。
        Panel::new_windowless(
            Box::new(HostRef(std::sync::Arc::clone(host))),
            Config {
                parent: Parent::Child(std::ptr::null_mut()),
                title: String::new(),
                scale: 1.0,
                preset_path: None,
                model_key: true,
                device_key: true,
            },
        )
    }

    /// `Arc<MockHost>` を `Box<dyn Host>` にするための薄い包み。
    struct HostRef(std::sync::Arc<MockHost>);
    impl Host for HostRef {
        fn state(&self) -> State {
            self.0.state()
        }
        fn event(&self, ev: Event) {
            self.0.event(ev);
        }
    }

    #[test]
    fn fader_drag_emits_fader_then_commit() {
        let host = std::sync::Arc::new(MockHost::new());
        let mut p = panel(&host);

        let r = p.layout.fader_rect(0);
        p.on_mouse_down(r.l + 1, r.t + 5, 0);
        p.on_mouse_move(r.l + 1, r.b - 3);
        p.on_mouse_up();

        let ev = host.events();
        assert!(matches!(ev.first(), Some(Event::Fader { index: 0, .. })));
        assert!(matches!(ev.last(), Some(Event::FaderCommit { index: 0 })));
    }

    #[test]
    fn mute_and_bypass_are_exclusive() {
        let host = std::sync::Arc::new(MockHost::new());
        let mut p = panel(&host);

        let m = p.layout.mute_rect(0);
        p.on_mouse_down(m.l + 1, m.t + 1, 0);
        assert_eq!(p.state.modes[0], dsp::FeatureMode::Mute);

        let b = p.layout.byp_rect(0);
        p.on_mouse_down(b.l + 1, b.t + 1, 0);
        assert_eq!(p.state.modes[0], dsp::FeatureMode::Bypass);

        // もう一度 BYP を押すと有効へ戻る。
        p.on_mouse_down(b.l + 1, b.t + 1, 0);
        assert_eq!(p.state.modes[0], dsp::FeatureMode::Active);
    }

    #[test]
    fn model_and_device_keys_cycle() {
        let host = std::sync::Arc::new(MockHost::new());
        let mut p = panel(&host);

        let k = p.layout.model_key_rect();
        p.on_mouse_down(k.l + 1, k.t + 1, 0);
        assert_eq!(p.state.model, ModelChoice::Quality);
        p.on_mouse_down(k.l + 1, k.t + 1, 0);
        assert_eq!(p.state.model, ModelChoice::Fast);
        p.on_mouse_down(k.l + 1, k.t + 1, 0);
        assert_eq!(p.state.model, ModelChoice::Auto);

        let d = p.layout.device_key_rect();
        p.on_mouse_down(d.l + 1, d.t + 1, 0);
        assert_eq!(p.state.device, DeviceChoice::Gpu);

        let ev = host.events();
        assert!(ev.iter().any(|e| matches!(e, Event::Model(ModelChoice::Quality))));
        assert!(ev.contains(&Event::Device(DeviceChoice::Gpu)));
    }

    #[test]
    fn memory_saves_and_recalls_without_a_file() {
        let host = std::sync::Arc::new(MockHost::new());
        let mut p = panel(&host);
        p.apply_fader(0, 0.3, true);
        p.apply_fader(1, 4.0, true);
        p.apply_fader(2, 0.5, true);

        // SAVE を押してからスロット 2 へ保存。
        p.toggle_save_mode();
        let s = p.layout.slot_rect(1);
        p.on_mouse_down(s.l + 1, s.t + 1, 0);
        assert_eq!(p.current_slot, Some(1));
        assert!(p.slots[1].is_some());

        p.apply_fader(0, 0.0, true);
        p.apply_fader(1, 0.0, true);
        p.apply_fader(2, 0.0, true);
        p.on_mouse_down(s.l + 1, s.t + 1, 0);
        assert_eq!(p.state.values, [0.3, 4.0, 0.5]);
        assert!(host
            .take_events()
            .iter()
            .any(|e| matches!(e, Event::ApplyPreset(_))));
    }
}
