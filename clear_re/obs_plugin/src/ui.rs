//! 自前 Win32 パネル（GDI + DIB 描画）。
//!
//! 旧 VST3 版の `DPDFNetVoiceEditor`（JUCE）を Rust + 生 Win32 で描き直したもの。
//! トーン（暗い縦グラデ背景 + 上端の淡いアクセントグロー）は引き継いでいる。
//!
//!   * ヘッダ（タイトル + サブタイトル + 区切り線）
//!   * INPUT   : 入力 L / R のアナログ VU メーター（針 + VU 目盛り、300 ms 特性）
//!   * CONTROL : 縦フェーダー 3 本（AMBIENCE / VOICE / REVERB）
//!   * EFFECT  : GR メーター（コンプの GR と同じく 0 から下へ振れる）+ 出力 3 バンド
//!   * ステータスドット + テキスト、有効 / 無効のピル型トグル
//!
//! ## スレッドモデル
//!
//! パネルは**専用スレッド**で作る。OBS 側のメッセージループ（Qt）に依存せず、
//! ウィンドウの寿命とメッセージポンプを自分で持つため。`Shared` は `Arc` なので
//! OBS 側のフィルタが先に破棄されてもぶら下がりポインタにならない。
//! `obs_source_t*` だけは OBS が所有するので、`Shared::alive` が false に
//! なったら触らない。
//!
//! 描画は「DIB セクションに描いて `BitBlt`」方式。グラデーションはピクセルを
//! 直接書き、枠・文字・針は GDI に任せる。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, DeleteDC,
    DeleteObject, DrawTextW, EndPaint, ExtCreatePen, GetStockObject,
    InvalidateRect, LineTo, MoveToEx, RoundRect, ScreenToClient,
    SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BS_SOLID,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DRAW_TEXT_FORMAT,
    DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    FF_DONTCARE, FONT_CHARSET, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION, FONT_QUALITY, HBRUSH,
    HBITMAP, HDC, HFONT, HGDIOBJ, HPEN, LOGBRUSH, NULL_BRUSH, OUT_TT_PRECIS, PEN_STYLE,
    PS_ENDCAP_ROUND, PS_GEOMETRIC, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetActiveWindow, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, KillTimer, LoadCursorW,
    PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, CREATESTRUCTW, CS_DBLCLKS,
    CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, MSG, SM_CXSCREEN, SM_CYSCREEN,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOW, WM_CLOSE, WM_CREATE, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SIZE, WM_TIMER,
    WNDCLASSW, WS_CAPTION, WS_EX_TOOLWINDOW, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
};

use crate::dsp::params as dsp;
use crate::filter::{Shared, ST_OK};

// --------------------------------------------------------------------------
// パレット（PluginEditor.cpp と同じ値）
// --------------------------------------------------------------------------

const BG_TOP: u32 = 0x191e24;
const BG_BOTTOM: u32 = 0x0c0f12;
const PANEL: u32 = 0x1b2129;
const SLOT: u32 = 0x12171c;
const STROKE: u32 = 0x2b333d;
const TRACK: u32 = 0x242b34;
const TEXT: u32 = 0xe8ecf1;
const TEXT_DIM: u32 = 0x8b95a3;
const TEXT_FAINT: u32 = 0x5b6572;
const ACCENT: u32 = 0x57cdbc;
const ACCENT2: u32 = 0x86ecdc;
const WARM: u32 = 0xe0a24b;

// --------------------------------------------------------------------------
// レイアウト（論理ピクセル）
// --------------------------------------------------------------------------

const K_RADIUS: f32 = 10.0;
const K_MARGIN: f32 = 22.0;
const K_GAP: f32 = 8.0;
const K_HEADER_H: f32 = 44.0;
const K_FOOTER_H: f32 = 36.0;

/// 各セクションの見出し（INPUT / CONTROL / EFFECT）の高さ。
const K_SECTION_TITLE_H: f32 = 16.0;
/// セクション同士の横間隔。
const K_SECTION_GAP: f32 = 12.0;
/// 左（入力 VU）セクションの幅。残りを CONTROL / EFFECT で分ける。
const K_INPUT_W: f32 = 296.0;
const K_CONTROL_W: f32 = 256.0;

/// 入力 VU メーター 2 本の縦間隔。
const K_VU_GAP: f32 = 8.0;

/// フェーダー 1 本ぶん。
const K_FADER_LABEL_H: f32 = 16.0;
const K_FADER_VALUE_H: f32 = 16.0;
const K_FADER_TRACK_W: f32 = 8.0;
const K_FADER_HANDLE_W: f32 = 26.0;
const K_FADER_HANDLE_H: f32 = 13.0;

/// GR メーター（縦・0 から下へ振れるコンプの GR 風）。
const K_GR_BAR_W: f32 = 18.0;
const K_GR_SCALE_W: f32 = 26.0;

/// 3 バンド（低 / 中 / 高）バー。
const K_BAND_W: f32 = 10.0;
const K_BAND_GAP: f32 = 6.0;
const K_BAND_LABEL_H: f32 = 14.0;

const K_BUTTON_W: f32 = 104.0;
const K_BUTTON_H: f32 = 30.0;

const CLIENT_W: f32 = 720.0;
const CLIENT_H: f32 = 380.0;

/// 入力 VU の表示範囲（dBFS）と針の振れ角（度、0 = 真上）。
/// 0 VU = -18 dBFS とし、-20 VU (-38 dBFS) 〜 +3 VU (-15 dBFS) を出す。
const VU_DBFS_AT_MIN: f32 = -38.0;
const VU_DBFS_AT_MAX: f32 = -15.0;
const VU_ANGLE_MIN: f32 = -55.0;
const VU_ANGLE_MAX: f32 = 55.0;

/// GR メーターの振れ幅（0 〜 -30 dB）。
const GR_DB_RANGE: f32 = 30.0;

/// リペイント間隔 (ms)。JUCE 版の `startTimerHz(24)` に合わせる。
const TIMER_MS: u32 = 40;

// --------------------------------------------------------------------------
// 公開 API
// --------------------------------------------------------------------------

/// パネルを開く（既に開いていれば前面に出す）。OBS の UI スレッドから呼ぶ。
pub fn open_panel(shared: &Arc<Shared>, source: *mut std::ffi::c_void) -> Result<(), String> {
    // 0 = 閉じている。開いている/開いている途中は前面に出すだけ。
    // （ボタン連打でウィンドウが 2 枚出るのを防ぐための CAS）
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
    let owner = SendHwnd(unsafe { GetActiveWindow() });

    let shared = Arc::clone(shared);
    let src = SendPtr(source);
    let thread_shared = Arc::clone(&shared);

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
        unsafe {
            let hwnd = HWND(h as *mut std::ffi::c_void);
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// モジュールのアンロード前にパネルスレッドの終了を待つ。
///
/// パネルスレッドは我々のコード上を走っているので、`FreeLibrary` より先に
/// 終わらせておきたい。最大 `timeout_ms` だけ待ち、終わっていれば join する
/// （終わっていなければハンドルを捨てる。ハングさせるよりは安全側）。
pub fn shutdown(timeout_ms: u64) {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    while PANEL_RUNNING.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
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
// スレッド / ウィンドウクラス
// --------------------------------------------------------------------------

static PANEL_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static PANEL_RUNNING: AtomicBool = AtomicBool::new(false);

/// `Shared::panel` の「開いている途中」を表す番兵。本物の `HWND` はこれより大きい。
const PANEL_OPENING: usize = 1;

/// ウィンドウクラスは 1 度だけ登録する。
static CLASS: OnceLock<u16> = OnceLock::new();

/// 生ポインタをスレッドへ渡すためのラッパ。
///
/// パネルは `Shared::alive` が true の間だけ `source` を使う。
struct SendPtr(*mut std::ffi::c_void);
unsafe impl Send for SendPtr {}

/// `HWND` は `*mut c_void` を含むため `Send` でない。ウィンドウハンドルは
/// どのスレッドから触っても安全なので包んで渡す。
struct SendHwnd(HWND);
unsafe impl Send for SendHwnd {}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn class_name() -> Vec<u16> {
    wide("ObsNoiseSuppressorPanel")
}

fn ensure_class() -> Result<u16, String> {
    if let Some(a) = CLASS.get() {
        return Ok(*a);
    }

    let name = class_name();
    let hinst = unsafe { GetModuleHandleW(None) }.map_err(|e| format!("GetModuleHandleW: {e}"))?;

    let atom = unsafe {
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.style = CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS;
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = windows::Win32::Foundation::HINSTANCE(hinst.0);
        wc.hCursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        wc.hbrBackground = HBRUSH(std::ptr::null_mut());
        wc.lpszClassName = PCWSTR(name.as_ptr());
        RegisterClassW(&wc)
    };

    if atom == 0 {
        return Err("RegisterClassW に失敗".into());
    }
    Ok(*CLASS.get_or_init(|| atom))
}

fn run_panel(shared: Arc<Shared>, owner: SendHwnd, source: SendPtr) {
    PANEL_RUNNING.store(true, Ordering::SeqCst);
    if let Err(e) = create_and_run(Arc::clone(&shared), owner, source) {
        crate::obs_log!(crate::ffi::LOG_WARNING, "パネル: {}", e);
    }
    // ウィンドウが閉じた（あるいは作れなかった）。次の「開く」で開けるようにする。
    shared.panel.store(0, Ordering::SeqCst);
    PANEL_RUNNING.store(false, Ordering::SeqCst);
}

fn create_and_run(shared: Arc<Shared>, owner: SendHwnd, source: SendPtr) -> Result<(), String> {
    ensure_class()?;

    let owner = owner.0;
    let class = class_name();
    let title = wide("OBS Noise Suppressor");

    // オーナーから DPI を推定する（ウィンドウが無いので GetDpiForWindow が使えるのは
    // オーナーだけ）。
    let mut ratio = 1.0f32;
    unsafe {
        let dpi = GetDpiForWindow(owner);
        if dpi > 0 {
            ratio = dpi as f32 / 96.0;
        }
    }

    let panel = Box::new(Panel::new(Arc::clone(&shared), source.0, ratio));
    let param = Box::into_raw(panel);

    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

    // 画面中央（オーナーがあればその中央）に置く。サイズは WM_CREATE で
    // クライアント 620x340 に補正する。
    let (ox, oy, ow, oh) = owner_rect(owner);
    let w = (CLIENT_W * ratio).round() as i32 + 64;
    let h = (CLIENT_H * ratio).round() as i32 + 64;
    let x = if ow > 0 { ox + (ow - w) / 2 } else { (screen_w() - w) / 2 };
    let y = if oh > 0 { oy + (oh - h) / 2 } else { (screen_h() - h) / 2 };

    let created = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
            x,
            y,
            w,
            h,
            Some(owner),
            None,
            None,
            Some(param as *const std::ffi::c_void),
        )
    };

    let hwnd = match created {
        Ok(h) => h,
        Err(e) => {
            // WM_NCCREATE に到達していないので、こちらで解放する。
            unsafe { drop(Box::from_raw(param)) };
            shared.panel.store(0, Ordering::SeqCst);
            return Err(format!("CreateWindowExW: {e}"));
        }
    };

    shared.panel.store(hwnd.0 as usize, Ordering::SeqCst);

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }

    // このスレッド専用のメッセージループ。OBS のループには依存しない。
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    shared.panel.store(0, Ordering::SeqCst);
    Ok(())
}

fn screen_w() -> i32 {
    unsafe { GetSystemMetrics(SM_CXSCREEN) }
}

fn screen_h() -> i32 {
    unsafe { GetSystemMetrics(SM_CYSCREEN) }
}

fn owner_rect(hwnd: HWND) -> (i32, i32, i32, i32) {
    if hwnd.0.is_null() {
        return (0, 0, 0, 0);
    }
    let mut r = RECT::default();
    unsafe {
        if GetWindowRect(hwnd, &mut r).is_err() {
            return (0, 0, 0, 0);
        }
    }
    (r.left, r.top, r.right - r.left, r.bottom - r.top)
}

// --------------------------------------------------------------------------
// ウィンドウプロシージャ
// --------------------------------------------------------------------------

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(l.0 as *const CREATESTRUCTW);
        let panel = cs.lpCreateParams as *mut Panel;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, panel as isize);
        if !panel.is_null() {
            (*panel).hwnd = hwnd;
        }
        // 0 を返すとウィンドウ生成が失敗するので 1 を返す。
        return LRESULT(1);
    }

    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Panel;
    if p.is_null() {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let panel = &mut *p;

    let x = (l.0 & 0xFFFF) as i16 as i32;
    let y = ((l.0 >> 16) & 0xFFFF) as i16 as i32;

    match msg {
        WM_CREATE => {
            panel.on_create();
            LRESULT(0)
        }
        WM_SIZE => {
            panel.on_size();
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (w.0 & 0xFFFF) as u32;
            panel.set_scale(dpi as f32 / 96.0);
            panel.on_size();
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            panel.blit(hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_TIMER => {
            panel.on_timer();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            panel.on_mouse_down(x, y);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            panel.on_mouse_dbl(x, y);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            panel.on_mouse_up();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            panel.on_mouse_move(x, y);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((w.0 >> 16) & 0xFFFF) as i16 as i32;
            panel.on_wheel(delta);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if w.0 == 0x1B {
                // ESC で閉じる。
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            panel.on_destroy();
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(p));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

// --------------------------------------------------------------------------
// 小物
// --------------------------------------------------------------------------

#[derive(Clone, Copy, Default, Debug)]
struct IR {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

impl IR {
    fn w(&self) -> i32 {
        self.r - self.l
    }
    fn h(&self) -> i32 {
        self.b - self.t
    }
    fn cx(&self) -> i32 {
        (self.l + self.r) / 2
    }
    fn cy(&self) -> i32 {
        (self.t + self.b) / 2
    }
    fn top(&self, h: i32) -> IR {
        IR { b: self.t + h, ..*self }
    }
    fn bottom(&self, h: i32) -> IR {
        IR { t: self.b - h, ..*self }
    }
}

#[inline]
fn hit(r: &IR, x: i32, y: i32) -> bool {
    x >= r.l && x < r.r && y >= r.t && y < r.b
}

/// 線形 RMS -> 表示用 0..1（-60..0 dB）。
fn rms_to_display(rms: f32) -> f32 {
    let v = rms.max(1.0e-6) as f64;
    let db = 20.0 * v.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f32
}

/// 線形 RMS -> VU の針位置 0..1。
///
/// 0 VU = -18 dBFS とし、-20 VU (-38 dBFS) 〜 +3 VU (-15 dBFS) を表示する。
fn vu_to_pos(rms: f32) -> f32 {
    if !(rms > 0.0) {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db - VU_DBFS_AT_MIN) / (VU_DBFS_AT_MAX - VU_DBFS_AT_MIN)).clamp(0.0, 1.0)
}

/// 入力 / 出力 RMS -> GR メーターの振れ 0..1（抑圧量 [dB] / 30）。
///
/// 帰還量が読めない（無音）ときは 0。VOICE で持ち上げて出力の方が大きい場合も 0。
fn gr_to_display(in_rms: f32, out_rms: f32) -> f32 {
    const FLOOR: f32 = 1.0e-5;
    if !(in_rms > FLOOR) || !(out_rms > 0.0) {
        return 0.0;
    }
    let db = 20.0 * (out_rms / in_rms).log10();
    (-db / GR_DB_RANGE).clamp(0.0, 1.0)
}

#[inline]
fn lerp_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |shift: u32| -> u32 {
        let x = ((a >> shift) & 0xFF) as f32;
        let y = ((b >> shift) & 0xFF) as f32;
        ((x + (y - x) * t).round() as u32) & 0xFF
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// 無効状態を「背景に寄せる」ことで表現する（JUCE の withMultipliedAlpha 相当）。
#[inline]
fn dim(c: u32, amount: f32) -> u32 {
    lerp_rgb(BG_TOP, c, amount)
}

#[inline]
fn lighten(c: u32, t: f32) -> u32 {
    lerp_rgb(c, 0xFFFFFF, t)
}

#[inline]
fn colorref(c: u32) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | (c >> 16 & 0xFF)
}

fn inside_round_rect(x: i32, y: i32, r: IR, rad: i32) -> bool {
    if x < r.l || x >= r.r || y < r.t || y >= r.b {
        return false;
    }
    // `clamp` は min > max で panic するので、必ず `min <= max` になるよう
    // 角の半径を「短辺 - 1」の半分までに抑える。
    let max_rad = ((r.w().min(r.h()) - 1) / 2).max(0);
    let rad = rad.min(max_rad);
    if rad == 0 {
        return true;
    }
    let cx = x.clamp(r.l + rad, r.r - 1 - rad) as f32;
    let cy = y.clamp(r.t + rad, r.b - 1 - rad) as f32;
    let dx = x as f32 - cx;
    let dy = y as f32 - cy;
    dx * dx + dy * dy <= (rad as f32) * (rad as f32)
}

// --------------------------------------------------------------------------
// フェーダー定義
// --------------------------------------------------------------------------

struct FaderSpec {
    name: &'static str,
    min: f32,
    max: f32,
    default: f32,
    percent: bool,
}

const FADERS: [FaderSpec; 3] = [
    FaderSpec {
        name: "AMBIENCE",
        min: dsp::AMBIENCE_MIN as f32,
        max: dsp::AMBIENCE_MAX as f32,
        default: dsp::AMBIENCE_DEFAULT as f32,
        percent: true,
    },
    FaderSpec {
        name: "VOICE",
        min: dsp::VOICE_MIN as f32,
        max: dsp::VOICE_MAX as f32,
        default: dsp::VOICE_DEFAULT as f32,
        percent: false,
    },
    FaderSpec {
        name: "REVERB",
        min: dsp::VOICE_REVERB_MIN as f32,
        max: dsp::VOICE_REVERB_MAX as f32,
        default: dsp::VOICE_REVERB_DEFAULT as f32,
        percent: true,
    },
];

#[derive(Clone, Copy, Debug, Default)]
struct Layout {
    title: IR,
    status: IR,
    button: IR,

    /// セクション見出し（INPUT / CONTROL / EFFECT）を描く帯。
    section: [IR; 3],
    /// 入力 VU（左 = ch0 / 右 = ch1）。
    vu: [IR; 2],
    /// フェーダーの当たり判定（ラベル・値表示を含む柱）。
    fader: [IR; 3],
    /// フェーダーのトラック（つまみが動く範囲）。
    fader_track: [IR; 3],
    label: [IR; 3],
    value: [IR; 3],
    /// GR バー本体と目盛り。
    gr: IR,
    gr_scale: IR,
    /// 3 バンドバー（低 / 中 / 高）とラベル。
    bands: IR,
    bands_label: IR,
}

// --------------------------------------------------------------------------
// GDI リソース
// --------------------------------------------------------------------------

struct Fonts {
    scale: f32,
    title: HFONT,
    subtitle: HFONT,
    value: HFONT,
    label: HFONT,
    tiny: HFONT,
    button: HFONT,
    status: HFONT,
}

impl Fonts {
    unsafe fn new(scale: f32) -> Self {
        Self {
            scale,
            title: make_font(17.0 * scale, 600),
            subtitle: make_font(9.5 * scale, 400),
            value: make_font(13.0 * scale, 400),
            label: make_font(10.5 * scale, 600),
            tiny: make_font(9.0 * scale, 400),
            button: make_font(11.0 * scale, 600),
            status: make_font(11.0 * scale, 400),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [
            self.title, self.subtitle, self.value, self.label, self.tiny, self.button, self.status,
        ] {
            if !f.0.is_null() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(f.0));
                }
            }
        }
    }
}

struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
}

impl Dib {
    unsafe fn new(w: i32, h: i32) -> Result<Self, String> {
        let dc = CreateCompatibleDC(None);
        if dc.0.is_null() {
            return Err("CreateCompatibleDC に失敗".into());
        }

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let bmp = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(b) => b,
            Err(e) => {
                let _ = DeleteDC(dc);
                return Err(format!("CreateDIBSection: {e}"));
            }
        };
        if bits.is_null() {
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(dc);
            return Err("CreateDIBSection: 画素が取れない".into());
        }

        let old = SelectObject(dc, HGDIOBJ(bmp.0));
        Ok(Dib { dc, bmp, old, bits: bits as *mut u32, w, h })
    }

    #[inline]
    fn put(&mut self, x: i32, y: i32, c: u32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        unsafe {
            *self.bits.add((y * self.w + x) as usize) = c & 0x00FF_FFFF;
        }
    }

    #[inline]
    fn get(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return 0;
        }
        unsafe { *self.bits.add((y * self.w + x) as usize) & 0x00FF_FFFF }
    }

    fn fill_rect(&mut self, r: IR, c: u32) {
        for y in r.t.max(0)..r.b.min(self.h) {
            for x in r.l.max(0)..r.r.min(self.w) {
                self.put(x, y, c);
            }
        }
    }

    fn fill_rect_alpha(&mut self, r: IR, c: u32, a: f32) {
        for y in r.t.max(0)..r.b.min(self.h) {
            for x in r.l.max(0)..r.r.min(self.w) {
                let bg = self.get(x, y);
                self.put(x, y, lerp_rgb(bg, c, a));
            }
        }
    }

    fn fill_round_rect(&mut self, r: IR, rad: i32, c: u32) {
        for y in r.t.max(0)..r.b.min(self.h) {
            for x in r.l.max(0)..r.r.min(self.w) {
                if inside_round_rect(x, y, r, rad) {
                    self.put(x, y, c);
                }
            }
        }
    }

    /// 縦バー用: 上 `top` → 下 `bottom` の縦グラデ（角丸）。
    fn fill_round_rect_vgrad(&mut self, r: IR, rad: i32, top: u32, bottom: u32) {
        let span = (r.h() - 1).max(1) as f32;
        for y in r.t.max(0)..r.b.min(self.h) {
            let t = (y - r.t) as f32 / span;
            let c = lerp_rgb(top, bottom, t);
            for x in r.l.max(0)..r.r.min(self.w) {
                if inside_round_rect(x, y, r, rad) {
                    self.put(x, y, c);
                }
            }
        }
    }

    fn fill_ellipse(&mut self, cx: i32, cy: i32, r: i32, c: u32) {
        let r2 = (r * r) as f32;
        for y in (cy - r).max(0)..(cy + r + 1).min(self.h) {
            for x in (cx - r).max(0)..(cx + r + 1).min(self.w) {
                let dx = (x - cx) as f32;
                let dy = (y - cy) as f32;
                if dx * dx + dy * dy <= r2 {
                    self.put(x, y, c);
                }
            }
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bmp.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

// --------------------------------------------------------------------------
// パネル
// --------------------------------------------------------------------------

struct Drag {
    fader: usize,
}

/// 描画に必要な値を `self` から先に取り出しておくための束
/// （`&mut self.dib` を握ったままメソッドを呼べないため）。
struct Snapshot {
    scale: f32,
    layout: Layout,
    /// 入力 VU の針位置 0..1（ch ごと）。
    vu: [f32; 2],
    /// GR の振れ 0..1（0 = 抑圧なし / 1 = -30 dB）。
    gr: f32,
    /// 出力 3 バンドの表示値 0..1。
    bands: [f32; 3],
    bands_peak: [f32; 3],
    status: u32,
    status_text: String,
    summary: String,
    values: [f32; 3],
    dereverb: bool,
    /// エンジンのチャンネル数（1 = モノラル / 2 = ステレオ / 0 = 未構築）。
    channels: usize,
    enabled: bool,
    hover_button: bool,
    press_button: bool,
    hover_fader: Option<usize>,
}

struct Panel {
    shared: Arc<Shared>,
    source: *mut std::ffi::c_void,
    hwnd: HWND,
    scale: f32,
    dib: Option<Dib>,
    fonts: Option<Fonts>,
    layout: Layout,
    /// 入力 VU の針（ボールistics を掛けた現在位置）。
    vu_display: [f32; 2],
    /// GR メーターの現在の振れ。
    gr_display: f32,
    bands_display: [f32; 3],
    bands_peak: [f32; 3],
    drag: Option<Drag>,
    /// ドラッグ中に触ったフェーダー（離したときに OBS 設定へ書き戻す）。
    pending_push: Option<usize>,
    hover_button: bool,
    press_button: bool,
    hover_fader: Option<usize>,
}

impl Panel {
    fn new(shared: Arc<Shared>, source: *mut std::ffi::c_void, scale: f32) -> Self {
        Self {
            shared,
            source,
            hwnd: HWND(std::ptr::null_mut()),
            scale,
            dib: None,
            fonts: None,
            layout: Layout::default(),
            vu_display: [0.0; 2],
            gr_display: 0.0,
            bands_display: [0.0; 3],
            bands_peak: [0.0; 3],
            drag: None,
            pending_push: None,
            hover_button: false,
            press_button: false,
            hover_fader: None,
        }
    }

    fn set_scale(&mut self, s: f32) {
        if (s - self.scale).abs() > 0.01 {
            self.scale = s;
            self.fonts = None;
        }
    }

    #[inline]
    fn sc(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    // ----------------------------------------------------------------------
    // ウィンドウイベント
    // ----------------------------------------------------------------------

    unsafe fn on_create(&mut self) {
        let _ = SetTimer(Some(self.hwnd), 1, TIMER_MS, None);

        // クライアント領域を論理 620x340 ちょうどにする。
        let mut rc = RECT::default();
        if GetClientRect(self.hwnd, &mut rc).is_ok() {
            let dw = self.sc(CLIENT_W) - (rc.right - rc.left);
            let dh = self.sc(CLIENT_H) - (rc.bottom - rc.top);
            if dw != 0 || dh != 0 {
                let mut wr = RECT::default();
                if GetWindowRect(self.hwnd, &mut wr).is_ok() {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        0,
                        0,
                        (wr.right - wr.left) + dw,
                        (wr.bottom - wr.top) + dh,
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
        }

        self.on_size();
    }

    unsafe fn on_size(&mut self) {
        let mut rc = RECT::default();
        if GetClientRect(self.hwnd, &mut rc).is_err() {
            return;
        }
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        if w <= 0 || h <= 0 {
            return;
        }

        if self.dib.as_ref().map(|d| (d.w, d.h)) != Some((w, h)) {
            match Dib::new(w, h) {
                Ok(d) => self.dib = Some(d),
                Err(e) => {
                    crate::obs_log!(crate::ffi::LOG_WARNING, "パネルの DIB: {}", e);
                    return;
                }
            }
        }

        if self.fonts.as_ref().map(|f| f.scale) != Some(self.scale) {
            self.fonts = None;
        }
        if self.fonts.is_none() {
            self.fonts = Some(Fonts::new(self.scale));
        }

        self.layout = self.compute_layout(w, h);
        let _ = InvalidateRect(Some(self.hwnd), None, false);
    }

    unsafe fn on_destroy(&mut self) {
        let _ = KillTimer(Some(self.hwnd), 1);
        // DIB / フォントは `Panel` の Drop（WM_NCDESTROY）で解放される。
        self.dib = None;
        self.fonts = None;
    }

    fn on_timer(&mut self) {
        // ---- 入力 VU（針） ----
        // VU は平均値メーターなので、立ち上がりも戻りもゆっくり (300 ms 特性)。
        const VU_K: f32 = 0.32;
        for ch in 0..2 {
            let target = vu_to_pos(self.shared.meters.in_level(ch));
            let cur = &mut self.vu_display[ch];
            *cur += (target - *cur) * VU_K;
        }

        // ---- GR（入力 - 出力） ----
        // コンプの GR と同じで、掛かりは即時・戻りはゆっくり。
        let in_lv = self
            .shared
            .meters
            .in_level(0)
            .max(self.shared.meters.in_level(1));
        let out_lv = self
            .shared
            .meters
            .out_level(0)
            .max(self.shared.meters.out_level(1));
        let gr_target = gr_to_display(in_lv, out_lv);
        if gr_target > self.gr_display {
            self.gr_display = gr_target;
        } else {
            self.gr_display += (gr_target - self.gr_display) * 0.10;
        }

        // ---- 出力 3 バンド ----
        for b in 0..3 {
            let target =
                rms_to_display(self.shared.meters.get(0, b).max(self.shared.meters.get(1, b)));

            // 立ち上がりは即時、戻りは滑らかに（JUCE 版と同じ）。
            let cur = self.bands_display[b];
            self.bands_display[b] = if target > cur { target } else { cur + (target - cur) * 0.25 };
            self.bands_peak[b] = (self.bands_peak[b] - 0.02).max(self.bands_display[b]);
        }

        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn on_mouse_down(&mut self, x: i32, y: i32) {
        if hit(&self.layout.button, x, y) {
            self.press_button = true;
            unsafe {
                SetCapture(self.hwnd);
            }
            return;
        }
        for i in 0..3 {
            if hit(&self.layout.fader[i], x, y) && self.fader_enabled(i) {
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
        for i in 0..3 {
            if hit(&self.layout.fader[i], x, y) && self.fader_enabled(i) {
                self.apply_fader(i, FADERS[i].default, true);
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
            if self.shared.alive.load(Ordering::SeqCst) && !self.source.is_null() {
                unsafe {
                    let en = crate::ffi::obs_source_enabled(self.source as *mut _);
                    crate::ffi::obs_source_set_enabled(self.source as *mut _, !en);
                }
            }
        }

        self.drag = None;

        // ドラッグで動かした値はここで OBS の設定にも書き戻す（永続化）。
        if let Some(i) = self.pending_push.take() {
            self.push_settings(i);
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

        let hb = hit(&self.layout.button, x, y);
        let hf = (0..3).find(|i| hit(&self.layout.fader[*i], x, y) && self.fader_enabled(*i));
        if hb != self.hover_button || hf != self.hover_fader {
            self.hover_button = hb;
            self.hover_fader = hf;
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
        }
    }

    fn on_wheel(&mut self, delta: i32) {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(self.hwnd, &mut pt);
        }
        for i in 0..3 {
            if hit(&self.layout.fader[i], pt.x, pt.y) && self.fader_enabled(i) {
                let span = FADERS[i].max - FADERS[i].min;
                let step = span * 0.02 * (delta as f32 / 120.0);
                let v = (self.fader_value(i) + step).clamp(FADERS[i].min, FADERS[i].max);
                self.apply_fader(i, v, true);
                return;
            }
        }
    }

    /// フェーダーのトラック上の y から値を決める（クリック / ドラッグ共用）。
    fn set_fader_from_y(&mut self, i: usize, y: i32, push: bool) {
        let tr = self.layout.fader_track[i];
        let hh = self.sc(K_FADER_HANDLE_H);
        let travel = (tr.h() - hh).max(1) as f32;
        let top = (tr.t + hh / 2) as f32;
        let t = (1.0 - (y as f32 - top) / travel).clamp(0.0, 1.0);

        let spec = &FADERS[i];
        self.apply_fader(i, spec.min + t * (spec.max - spec.min), push);
    }

    // ----------------------------------------------------------------------
    // パラメータ
    // ----------------------------------------------------------------------

    fn fader_value(&self, i: usize) -> f32 {
        match i {
            0 => self.shared.params.ambient(),
            1 => self.shared.params.voice(),
            _ => self.shared.params.voice_reverb(),
        }
    }

    fn fader_enabled(&self, i: usize) -> bool {
        // REVERB は 2 出力モデルのときだけ有効（JUCE 版と同じ）。
        i != 2 || self.shared.dereverb.load(Ordering::Relaxed)
    }

    fn apply_fader(&mut self, i: usize, v: f32, push: bool) {
        match i {
            0 => self.shared.params.set_ambience(v),
            1 => self.shared.params.set_voice(v),
            _ => self.shared.params.set_voice_reverb(v),
        }
        if push {
            self.push_settings(i);
        } else {
            self.pending_push = Some(i);
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// OBS の設定にも同じ値を書き戻す。
    ///
    /// `obs_source_update` が我々の `update` コールバックを呼び、同じ値が
    /// アトミックに再入力される（冪等）。これでプロパティ画面のスライダ表示と
    /// シーンコレクションへの保存もパネルに追従する。
    fn push_settings(&self, _i: usize) {
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
            crate::ffi::obs_source_update(self.source as *mut _, d);
            crate::ffi::obs_data_release(d);
        }
    }

    // ----------------------------------------------------------------------
    // レイアウト / 描画
    // ----------------------------------------------------------------------

    fn compute_layout(&self, w: i32, h: i32) -> Layout {
        let m = self.sc(K_MARGIN);
        let mut area = IR { l: m, t: m, r: w - m, b: h - m };
        let mut out = Layout::default();

        out.title = area.top(self.sc(K_HEADER_H));
        // ヘッダぶんを消費してからギャップを空ける（JUCE 版の
        // `removeFromTop(44)` + `removeFromTop(8)` と同じ）。
        area.t += self.sc(K_HEADER_H) + self.sc(K_GAP);

        let mut footer = area.bottom(self.sc(K_FOOTER_H));
        let bw = self.sc(K_BUTTON_W);
        let bh = self.sc(K_BUTTON_H);
        out.button = IR {
            l: footer.r - bw,
            t: footer.cy() - bh / 2,
            r: footer.r,
            b: footer.cy() + bh / 2,
        };
        footer.r -= bw + self.sc(K_GAP);
        out.status = footer;
        // `removeFromBottom` と同じく、フッタぶんを消費する。
        area.b -= self.sc(K_FOOTER_H) + self.sc(K_GAP);

        // ---- 横を 3 セクションに分ける（INPUT / CONTROL / EFFECT） ----
        let gap = self.sc(K_SECTION_GAP);
        let input = IR { r: area.l + self.sc(K_INPUT_W), ..area };
        let control = IR {
            l: input.r + gap,
            r: input.r + gap + self.sc(K_CONTROL_W),
            ..area
        };
        let effect = IR { l: control.r + gap, ..area };

        let title_h = self.sc(K_SECTION_TITLE_H);
        out.section[0] = input.top(title_h);
        out.section[1] = control.top(title_h);
        out.section[2] = effect.top(title_h);

        // ---- INPUT: 入力 VU 2 本（L が上 / R が下） ----
        let vu_area = IR { t: input.t + title_h, ..input };
        let vg = self.sc(K_VU_GAP);
        let vh = (vu_area.h() - vg) / 2;
        out.vu[0] = IR { b: vu_area.t + vh, ..vu_area };
        out.vu[1] = IR {
            t: vu_area.t + vh + vg,
            ..vu_area
        };

        // ---- CONTROL: フェーダー 3 本 ----
        let ctrl_area = IR {
            t: control.t + title_h,
            ..control
        };
        let cw = ctrl_area.w() / 3;
        let fader_hh = self.sc(K_FADER_HANDLE_H);
        for i in 0..3usize {
            let cell = IR {
                l: ctrl_area.l + cw * i as i32,
                t: ctrl_area.t,
                r: ctrl_area.l + cw * (i as i32 + 1),
                b: ctrl_area.b,
            };
            out.label[i] = cell.top(self.sc(K_FADER_LABEL_H));
            out.value[i] = IR {
                t: out.label[i].b,
                b: out.label[i].b + self.sc(K_FADER_VALUE_H),
                ..cell
            };
            // つまみが上下にはみ出さないよう、トラックを内側に寄せる。
            out.fader_track[i] = IR {
                t: out.value[i].b + self.sc(4.0) + fader_hh / 2,
                b: cell.b - fader_hh / 2,
                ..cell
            };
            // 当たり判定は値表示より下の柱ぜんぶ（ミキサーのストリップ風）。
            out.fader[i] = IR {
                t: out.value[i].b,
                ..cell
            };
        }

        // ---- EFFECT: GR（縦・0 から下へ）+ 3 バンド ----
        let fx = IR {
            t: effect.t + title_h,
            ..effect
        };
        let gr_bar_w = self.sc(K_GR_BAR_W);
        let gr_scale_w = self.sc(K_GR_SCALE_W);
        let band_w = self.sc(K_BAND_W) * 3 + self.sc(K_BAND_GAP) * 2;
        let total = gr_bar_w + gr_scale_w + self.sc(K_GAP) + band_w;
        let x0 = fx.l + ((fx.w() - total) / 2).max(0);

        let top = fx.t + self.sc(12.0);
        let band_label_h = self.sc(K_BAND_LABEL_H);
        let bottom = fx.b - band_label_h;

        out.gr = IR {
            l: x0,
            t: top,
            r: x0 + gr_bar_w,
            b: bottom,
        };
        out.gr_scale = IR {
            l: out.gr.r,
            r: out.gr.r + gr_scale_w,
            ..out.gr
        };
        let band_l = out.gr_scale.r + self.sc(K_GAP);
        out.bands = IR {
            l: band_l,
            t: top,
            r: band_l + band_w,
            b: bottom,
        };
        out.bands_label = IR {
            l: out.bands.l,
            t: bottom,
            r: out.bands.r,
            b: fx.b,
        };

        out
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            scale: self.scale,
            layout: self.layout,
            vu: self.vu_display,
            gr: self.gr_display,
            bands: self.bands_display,
            bands_peak: self.bands_peak,
            status: self.shared.status(),
            status_text: self.shared.status_detail(),
            summary: self.shared.summary(),
            values: [self.fader_value(0), self.fader_value(1), self.fader_value(2)],
            dereverb: self.shared.dereverb.load(Ordering::Relaxed),
            channels: self.shared.channels.load(Ordering::Relaxed),
            enabled: if self.shared.alive.load(Ordering::SeqCst) && !self.source.is_null() {
                unsafe { crate::ffi::obs_source_enabled(self.source as *mut _) }
            } else {
                true
            },
            hover_button: self.hover_button,
            press_button: self.press_button,
            hover_fader: self.hover_fader,
        }
    }

    fn blit(&mut self, target: HDC) {
        // 実際の描画は DIB に対して行う。
        self.paint();

        let Some(dib) = &self.dib else {
            return;
        };
        let (w, h) = (dib.w, dib.h);
        unsafe {
            let _ = BitBlt(target, 0, 0, w, h, Some(dib.dc), 0, 0, SRCCOPY);
        }
    }

    fn paint(&mut self) {
        if self.dib.is_none() || self.fonts.is_none() {
            return;
        }
        let s = self.snapshot();

        // `&mut self.dib` と `&self.fonts` は別フィールドなので同時に借りられる。
        let Some(dib) = self.dib.as_mut() else { return };
        let Some(fonts) = self.fonts.as_ref() else { return };
        let dc = dib.dc;
        let (w, h) = (dib.w, dib.h);
        let sc = |v: f32| (v * s.scale).round() as i32;
        let lay = s.layout;

        unsafe {
            SetBkMode(dc, TRANSPARENT);

            // ---- 背景（縦グラデ + 上部の淡いアクセントグロー） ----
            let glow_end = (h as f32 * 0.42).max(1.0);
            for y in 0..h {
                let t = y as f32 / (h.max(2) - 1) as f32;
                let base = lerp_rgb(BG_TOP, BG_BOTTOM, t);
                let ga = if (y as f32) < glow_end {
                    0.09 * (1.0 - y as f32 / glow_end)
                } else {
                    0.0
                };
                let c = if ga > 0.0 { lerp_rgb(base, ACCENT, ga) } else { base };
                for x in 0..w {
                    dib.put(x, y, c);
                }
            }

            // ---- 外枠 ----
            stroke_round_rect(dc, IR { l: 0, t: 0, r: w, b: h }, sc(K_RADIUS), dim(STROKE, 0.85), 1);

            // ---- ヘッダ ----
            draw_text(
                dc,
                "OBS Noise Suppressor",
                lay.title.top(sc(21.0)),
                fonts.title,
                TEXT,
                DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
            );
            draw_text(
                dc,
                "NEURAL DE-NOISE  ·  DE-REVERB  ·  48 kHz",
                IR { t: lay.title.t + sc(21.0), b: lay.title.t + sc(38.0), ..lay.title },
                fonts.subtitle,
                TEXT_FAINT,
                DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
            );

            let sep_y = lay.title.b + sc(K_GAP) / 2;
            dib.fill_rect(
                IR { l: lay.title.l, t: sep_y, r: lay.title.r, b: sep_y + 1 },
                dim(STROKE, 0.7),
            );

            // ---- セクション見出し ----
            let sec_names = ["INPUT", "CONTROL", "EFFECT"];
            for i in 0..3usize {
                let r = lay.section[i];
                draw_text(
                    dc,
                    sec_names[i],
                    r,
                    fonts.tiny,
                    TEXT_FAINT,
                    DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
                );
                let y = r.b - 1;
                dib.fill_rect(IR { l: r.l, t: y, r: r.r, b: y + 1 }, dim(STROKE, 0.55));
            }

            // ---- 入力 VU（アナログ針・L / R） ----
            let n_ch = s.channels.max(1);
            let vu_names = ["L", "R"];
            for ch in 0..2usize {
                paint_vu(
                    dib,
                    dc,
                    fonts,
                    s.scale,
                    lay.vu[ch],
                    s.vu[ch],
                    vu_names[ch],
                    ch < n_ch,
                );
            }

            // ---- フェーダー 3 本 ----
            for i in 0..3usize {
                let on = i != 2 || s.dereverb;
                paint_fader(
                    dib,
                    dc,
                    fonts,
                    s.scale,
                    lay.fader_track[i],
                    lay.value[i],
                    lay.label[i],
                    i,
                    s.values[i],
                    on,
                    s.hover_fader == Some(i),
                );
            }

            // ---- GR（コンプの GR と同じく 0 から下へ振れる） ----
            paint_gr(dib, dc, fonts, s.scale, lay.gr, lay.gr_scale, s.gr);

            // ---- 出力 3 バンド（GR の隣に細く） ----
            paint_bands(
                dib,
                dc,
                fonts,
                s.scale,
                lay.bands,
                lay.bands_label,
                &s.bands,
                &s.bands_peak,
            );

            // ---- ステータス ----
            let st = lay.status;
            let ok = s.status == ST_OK;
            dib.fill_ellipse(st.l + sc(5.0), st.cy(), sc(3.5).max(2), if ok { ACCENT } else { WARM });

            let text_r = IR { l: st.l + sc(18.0), ..st };
            // パネルには短い 1 行（モデル概要）を出す。まだエンジンが無いときは
            // 状態の詳細文をそのまま出す（何が足りないか分かるように）。
            let primary = if s.summary.is_empty() {
                &s.status_text
            } else {
                &s.summary
            };
            draw_text(
                dc,
                primary,
                text_r,
                fonts.status,
                TEXT_DIM,
                DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_END_ELLIPSIS.0,
            );

            // ---- BYPASS（点灯 = バイパス中） ----
            let on = !s.enabled;
            let br = lay.button;
            let rad = sc(6.0);
            let mut fill = if on { lerp_rgb(PANEL, WARM, 0.14) } else { PANEL };
            if s.hover_button {
                fill = if on { lerp_rgb(PANEL, WARM, 0.20) } else { lighten(PANEL, 0.07) };
            }
            if s.press_button {
                fill = lighten(fill, 0.06);
            }
            dib.fill_round_rect(br, rad, fill);
            stroke_round_rect(dc, br, rad, if on { lerp_rgb(STROKE, WARM, 0.8) } else { STROKE }, 1);
            draw_text(
                dc,
                "BYPASS",
                br,
                fonts.button,
                if on { WARM } else { TEXT_DIM },
                DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
            );
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        // GDI リソースは各 Option の Drop で解放される。
    }
}

// --------------------------------------------------------------------------
// 描画: 縦フェーダー / 入力 VU / GR / 3 バンド
// --------------------------------------------------------------------------

/// 縦フェーダー。クリックした位置へ飛び、そのままドラッグできる。
#[allow(clippy::too_many_arguments)]
unsafe fn paint_fader(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    track: IR,
    value_r: IR,
    label_r: IR,
    index: usize,
    value: f32,
    on: bool,
    hover: bool,
) {
    let spec = &FADERS[index];
    let span = (spec.max - spec.min).max(1e-9);
    let pos = ((value - spec.min) / span).clamp(0.0, 1.0);
    let bipolar = spec.min < 0.0 && spec.max > 0.0;

    // ---- ラベル（上） ----
    draw_text(
        dc,
        spec.name,
        label_r,
        fonts.label,
        if on { TEXT_DIM } else { dim(TEXT_DIM, 0.4) },
        DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );

    // ---- 値（ラベルの下） ----
    let text = if spec.percent {
        format!("{} %", (value * 100.0).round() as i32)
    } else {
        format!("{value:+.1} dB")
    };
    draw_text(
        dc,
        &text,
        value_r,
        fonts.value,
        if on { ACCENT } else { dim(ACCENT, 0.5) },
        DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );

    // ---- スロット ----
    let tw = ((K_FADER_TRACK_W * scale).round() as i32).max(4);
    let cx = track.cx();
    let slot = IR {
        l: cx - tw / 2,
        r: cx - tw / 2 + tw,
        t: track.t,
        b: track.b,
    };
    let rad = (tw as f32 * 0.5).round() as i32;
    dib.fill_round_rect(slot, rad, dim(TRACK, if on { 0.9 } else { 0.5 }));
    stroke_round_rect(dc, slot, rad, dim(STROKE, if on { 0.9 } else { 0.5 }), 1);

    // ---- 目盛り（スロットの右側） ----
    let tick_x = slot.r + (4.0 * scale).round() as i32;
    for k in 0..=4i32 {
        let t = k as f32 / 4.0;
        let y = (track.b as f32 - t * track.h() as f32).round() as i32;
        let major = k % 2 == 0;
        let len = if major {
            (8.0 * scale).round() as i32
        } else {
            (4.0 * scale).round() as i32
        };
        let c = dim(TEXT_FAINT, if major { 0.9 } else { 0.5 });
        dib.fill_rect(
            IR {
                l: tick_x,
                t: y,
                r: tick_x + len.max(1),
                b: y + 1,
            },
            c,
        );
    }

    // ---- つまみの位置 ----
    let handle_h = ((K_FADER_HANDLE_H * scale).round() as i32).max(6);
    let cy = (track.b as f32 - pos * track.h() as f32).round() as i32;

    // ---- 値のぶんだけ塗る（VOICE は中央 = 0 dB から） ----
    let anchor = if bipolar {
        (track.b as f32 - ((0.0 - spec.min) / span) * track.h() as f32).round() as i32
    } else {
        track.b
    };
    let (y0, y1) = (anchor.min(cy), anchor.max(cy));
    if y1 - y0 > 1 {
        let fill = IR {
            l: slot.l,
            t: y0,
            r: slot.r,
            b: y1,
        };
        let (c0, c1) = if on {
            (ACCENT, ACCENT2)
        } else {
            (dim(ACCENT, 0.55), dim(ACCENT2, 0.55))
        };
        // つまみ側を明るくする。
        let (ct, cb) = if cy <= anchor { (c1, c0) } else { (c0, c1) };
        dib.fill_round_rect_vgrad(fill, rad, ct, cb);
    }

    // 両極性（VOICE）は 0 dB の線を出す。
    if bipolar {
        dib.fill_rect(
            IR {
                l: slot.l - 2,
                t: anchor,
                r: slot.r + 2,
                b: anchor + 1,
            },
            dim(TEXT_FAINT, 0.85),
        );
    }

    // ---- つまみ ----
    let hw = ((K_FADER_HANDLE_W * scale).round() as i32).max(10);
    let h = IR {
        l: cx - hw / 2,
        r: cx - hw / 2 + hw,
        t: cy - handle_h / 2,
        b: cy - handle_h / 2 + handle_h,
    };
    let hrad = (3.0 * scale).round() as i32;
    let (ct, cb) = if on {
        if hover {
            (0x4e5a6a, 0x242a31)
        } else {
            (0x3d4753, 0x1c2127)
        }
    } else {
        (dim(0x3d4753, 0.5), dim(0x1c2127, 0.5))
    };
    dib.fill_round_rect_vgrad(h, hrad, ct, cb);
    stroke_round_rect(dc, h, hrad, dim(0x4e5a6a, if on { 0.9 } else { 0.4 }), 1);
    // つまみの中央線（ミキサーのフェーダー風）。
    let ly = h.cy();
    dib.fill_rect_alpha(
        IR {
            l: h.l + (3.0 * scale) as i32,
            t: ly,
            r: h.r - (3.0 * scale) as i32,
            b: ly + 1,
        },
        TEXT,
        if on { 0.5 } else { 0.2 },
    );
}

/// 入力レベルをアナログ VU の針で描く。`pos` は 0..1（0 = -20 VU / 1 = +3 VU）。
#[allow(clippy::too_many_arguments)]
unsafe fn paint_vu(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    r: IR,
    pos: f32,
    name: &str,
    on: bool,
) {
    let rad = (8.0 * scale).round() as i32;

    // ---- 盤面 ----
    dib.fill_round_rect(r, rad, dim(SLOT, if on { 1.0 } else { 0.7 }));
    stroke_round_rect(dc, r, rad, dim(STROKE, if on { 0.95 } else { 0.5 }), 1);

    // 支点は下中央（実機と同じく盤面の下端ぎりぎり）。
    let px = r.cx() as f32;
    let py = r.b as f32 - 4.0 * scale;
    let arc_r = (r.h() as f32 - 20.0 * scale).max(18.0 * scale);
    let label_r = arc_r - 14.0 * scale;

    // VU 値 -> 角度 [度] / 角度 + 半径 -> 座標。
    let ang = |vu: f32| -> f32 {
        let t = ((vu + 20.0) / 23.0).clamp(0.0, 1.0);
        VU_ANGLE_MIN + t * (VU_ANGLE_MAX - VU_ANGLE_MIN)
    };
    let pt = |a: f32, rr: f32| -> (f32, f32) {
        let rad = a.to_radians();
        (px + rr * rad.sin(), py - rr * rad.cos())
    };

    // ---- 目盛り弧 ----
    stroke_arc(
        dc,
        px,
        py,
        arc_r,
        VU_ANGLE_MIN,
        VU_ANGLE_MAX,
        (1.4 * scale).max(1.0),
        dim(TEXT_FAINT, if on { 0.85 } else { 0.4 }),
    );

    // ---- 目盛り ----
    // VU の目盛りは 0 付近が細かい（-3 / -2 / -1 / 0 / +1 / +2 / +3）。
    const VU_TICKS: [f32; 11] = [-20.0, -10.0, -7.0, -5.0, -3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0];
    const VU_LABELS: [i32; 5] = [-20, -10, -5, 0, 3];
    let tick_c = dim(if on { TEXT_DIM } else { TEXT_FAINT }, if on { 0.9 } else { 0.4 });

    for &v in VU_TICKS.iter() {
        let a = ang(v);
        let major = v == -20.0 || v == -10.0 || v == -5.0 || v == 0.0 || v == 3.0;
        let len = if major { 8.0 * scale } else { 4.5 * scale };
        let (x0, y0) = pt(a, arc_r);
        let (x1, y1) = pt(a, arc_r - len);
        stroke_line(dc, x0, y0, x1, y1, (1.2 * scale).max(1.0), tick_c);
    }

    for &v in VU_LABELS.iter() {
        let a = ang(v as f32);
        let (x, y) = pt(a, label_r - 2.0 * scale);
        let lr = IR {
            l: (x - 13.0 * scale).round() as i32,
            r: (x + 13.0 * scale).round() as i32,
            t: (y - 5.0 * scale).round() as i32,
            b: (y + 5.0 * scale).round() as i32,
        };
        draw_text(
            dc,
            &format!("{v}"),
            lr,
            fonts.tiny,
            dim(TEXT_FAINT, if on { 1.0 } else { 0.4 }),
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
    }

    // ---- 針 ----
    let a = VU_ANGLE_MIN + pos.clamp(0.0, 1.0) * (VU_ANGLE_MAX - VU_ANGLE_MIN);
    let (x1, y1) = pt(a, arc_r - 3.0 * scale);
    stroke_line(
        dc,
        px,
        py,
        x1,
        y1,
        (1.8 * scale).max(1.0),
        dim(0xe0655a, if on { 1.0 } else { 0.3 }),
    );
    dib.fill_ellipse(
        px.round() as i32,
        py.round() as i32,
        (3.0 * scale).max(2.0) as i32,
        dim(0x39424d, if on { 1.0 } else { 0.5 }),
    );

    // ---- キャプション（VU / チャンネル名） ----
    let cap = IR {
        l: r.l + (8.0 * scale) as i32,
        t: r.b - (15.0 * scale) as i32,
        r: r.cx(),
        b: r.b - (3.0 * scale) as i32,
    };
    draw_text(
        dc,
        "VU",
        cap,
        fonts.tiny,
        dim(TEXT_FAINT, if on { 1.0 } else { 0.4 }),
        DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );

    let nr = IR {
        l: r.cx(),
        t: cap.t,
        r: r.r - (8.0 * scale) as i32,
        b: cap.b,
    };
    draw_text(
        dc,
        name,
        nr,
        fonts.label,
        if on { TEXT_DIM } else { dim(TEXT_DIM, 0.4) },
        DT_RIGHT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );
}

/// GR（抑圧量）メーター。コンプの GR と同じく 0 の線から下へ伸びる。
unsafe fn paint_gr(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    bar: IR,
    scale_r: IR,
    amount: f32,
) {
    let rad = (4.0 * scale).round() as i32;

    // 見出し
    let cap = IR {
        t: bar.t - (11.0 * scale).round() as i32,
        b: bar.t,
        ..bar
    };
    draw_text(
        dc,
        "GR",
        cap,
        fonts.tiny,
        TEXT_FAINT,
        DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );

    // トラック
    dib.fill_round_rect(bar, rad, SLOT);
    stroke_round_rect(dc, bar, rad, dim(STROKE, 0.85), 1);

    // 0 dB の基準線
    dib.fill_rect(
        IR {
            l: bar.l - 2,
            t: bar.t - 1,
            r: bar.r + 2,
            b: bar.t + 1,
        },
        dim(TEXT_FAINT, 0.9),
    );

    // 塗り（下へ = 抑圧量）
    let h = (bar.h() as f32 * amount.clamp(0.0, 1.0)).round() as i32;
    if h > 1 {
        let fill = IR {
            t: bar.t,
            b: bar.t + h,
            ..bar
        };
        dib.fill_round_rect_vgrad(fill, rad, ACCENT2, WARM);
    }

    // 目盛り（0 / -6 / -12 / -18 / -24 / -30 dB）
    const GR_TICKS: [i32; 6] = [0, 6, 12, 18, 24, 30];
    for &db in GR_TICKS.iter() {
        let t = db as f32 / GR_DB_RANGE;
        let y = (bar.t as f32 + t * bar.h() as f32).round() as i32;
        let major = db % 12 == 0;
        let len = if major { 7.0 * scale } else { 4.0 * scale };
        dib.fill_rect(
            IR {
                l: bar.r + 1,
                t: y,
                r: bar.r + 1 + (len.round() as i32).max(1),
                b: y + 1,
            },
            dim(TEXT_FAINT, if major { 0.85 } else { 0.45 }),
        );
        if major {
            let tr = IR {
                l: bar.r + (10.0 * scale).round() as i32,
                r: scale_r.r,
                t: (y as f32 - 5.0 * scale).round() as i32,
                b: (y as f32 + 5.0 * scale).round() as i32,
            };
            let s = if db == 0 {
                "0".to_string()
            } else {
                format!("-{db}")
            };
            draw_text(
                dc,
                &s,
                tr,
                fonts.tiny,
                TEXT_FAINT,
                DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
            );
        }
    }
}

/// 出力 3 バンド（低 / 中 / 高）の細い縦バー。
unsafe fn paint_bands(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    bars: IR,
    labels: IR,
    levels: &[f32; 3],
    peaks: &[f32; 3],
) {
    let bw = ((K_BAND_W * scale).round() as i32).max(4);
    let gap = ((K_BAND_GAP * scale).round() as i32).max(2);
    let rad = (bw as f32 * 0.5).round() as i32;
    let names = ["L", "M", "H"];

    let cap = IR {
        t: bars.t - (11.0 * scale).round() as i32,
        b: bars.t,
        ..bars
    };
    draw_text(
        dc,
        "OUT",
        cap,
        fonts.tiny,
        TEXT_FAINT,
        DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );

    for b in 0..3usize {
        let l = bars.l + b as i32 * (bw + gap);
        let r = IR {
            l,
            r: l + bw,
            ..bars
        };
        dib.fill_round_rect(r, rad, SLOT);
        stroke_round_rect(dc, r, rad, dim(STROKE, 0.6), 1);

        let h = (r.h() as f32 * levels[b].clamp(0.0, 1.0)).round() as i32;
        if h > 1 {
            let fill = IR {
                t: r.b - h,
                ..r
            };
            // 下 accent → 上 warm（横向きメーターと同じ配色）。
            let mid = fill.b - (fill.h() as f32 * 0.62).round() as i32;
            dib.fill_round_rect_vgrad(
                IR {
                    b: mid,
                    ..fill
                },
                rad,
                ACCENT2,
                ACCENT,
            );
            dib.fill_round_rect_vgrad(
                IR {
                    t: mid,
                    ..fill
                },
                rad,
                WARM,
                ACCENT2,
            );
        }

        let pk = peaks[b].clamp(0.0, 1.0);
        if pk > 0.004 {
            let y = r.b - (r.h() as f32 * pk).round() as i32;
            dib.fill_rect_alpha(
                IR {
                    l: r.l,
                    t: y,
                    r: r.r,
                    b: y + 1,
                },
                TEXT,
                0.7,
            );
        }

        draw_text(
            dc,
            names[b],
            IR {
                l: r.l,
                t: labels.t,
                r: r.r,
                b: labels.b,
            },
            fonts.tiny,
            TEXT_DIM,
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
    }
}

// --------------------------------------------------------------------------
// GDI ヘルパ
// --------------------------------------------------------------------------

unsafe fn make_font(height: f32, weight: i32) -> HFONT {
    let name = wide("Segoe UI");
    CreateFontW(
        -(height.round().max(1.0) as i32),
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        FONT_CHARSET(DEFAULT_CHARSET.0),
        FONT_OUTPUT_PRECISION(OUT_TT_PRECIS.0),
        FONT_CLIP_PRECISION(CLIP_DEFAULT_PRECIS.0),
        // DIB 上では ClearType が正しく乗らないのでグレースケール AA にする。
        FONT_QUALITY(4),
        (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
        PCWSTR(name.as_ptr()),
    )
}

unsafe fn draw_text(dc: HDC, text: &str, r: IR, font: HFONT, color: u32, fmt: u32) {
    if text.is_empty() || font.0.is_null() {
        return;
    }
    let mut w: Vec<u16> = text.encode_utf16().collect();
    let mut rc = RECT { left: r.l, top: r.t, right: r.r, bottom: r.b };
    let old = SelectObject(dc, HGDIOBJ(font.0));
    SetTextColor(dc, COLORREF(colorref(color)));
    DrawTextW(dc, &mut w, &mut rc, DRAW_TEXT_FORMAT(fmt | DT_NOPREFIX.0));
    SelectObject(dc, old);
}

/// 丸いキャップの幾何ペン。
unsafe fn round_pen(width: f32, color: u32) -> HPEN {
    let brush = LOGBRUSH { lbStyle: BS_SOLID, lbColor: COLORREF(colorref(color)), lbHatch: 0 };
    ExtCreatePen(
        PEN_STYLE(PS_GEOMETRIC.0 | PS_ENDCAP_ROUND.0),
        width.max(1.0).round() as u32,
        &brush,
        None,
    )
}

unsafe fn stroke_line(dc: HDC, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: u32) {
    if width <= 0.0 {
        return;
    }
    let pen = round_pen(width, color);
    if pen.0.is_null() {
        return;
    }
    let old = SelectObject(dc, HGDIOBJ(pen.0));
    let _ = MoveToEx(dc, x0.round() as i32, y0.round() as i32, None);
    let _ = LineTo(dc, x1.round() as i32, y1.round() as i32);
    SelectObject(dc, old);
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

/// 円弧を折れ線で描く。
///
/// 角度は JUCE と同じ「0° = 12 時、時計回り」。点は `(cx + r·sin a, cy - r·cos a)`。
/// GDI の `AngleArc` は 0° = 3 時で向きも逆なので使わない。
unsafe fn stroke_arc(dc: HDC, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, width: f32, color: u32) {
    if r <= 1.0 || width <= 0.0 {
        return;
    }
    let pen = round_pen(width, color);
    if pen.0.is_null() {
        return;
    }
    let old = SelectObject(dc, HGDIOBJ(pen.0));

    let steps = (((a1 - a0).abs() / 2.0).ceil() as i32).clamp(2, 180);
    for k in 0..=steps {
        let t = k as f32 / steps as f32;
        let a = (a0 + (a1 - a0) * t).to_radians();
        let x = (cx + r * a.sin()).round() as i32;
        let y = (cy - r * a.cos()).round() as i32;
        if k == 0 {
            let _ = MoveToEx(dc, x, y, None);
        } else {
            let _ = LineTo(dc, x, y);
        }
    }

    SelectObject(dc, old);
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

unsafe fn stroke_round_rect(dc: HDC, r: IR, rad: i32, color: u32, width: i32) {
    let pen = CreatePen(PS_SOLID, width.max(1), COLORREF(colorref(color)));
    if pen.0.is_null() {
        return;
    }
    let old_pen = SelectObject(dc, HGDIOBJ(pen.0));
    let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));

    // GDI の RoundRect は角の楕円の幅・高さを取る。
    let d = (rad * 2).max(0);
    let _ = RoundRect(dc, r.l, r.t, r.r, r.b, d, d);

    SelectObject(dc, old_brush);
    SelectObject(dc, old_pen);
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

// --------------------------------------------------------------------------
// オフラインの見た目確認（OBS を起動せずパネルを描いて BMP に落とす）
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::engine::Meters;

    /// 32 bpp の BMP（ボトムアップ）で書き出す。Windows のフォトビューアで開ける。
    fn write_bmp(path: &std::path::Path, w: i32, h: i32, bits: *const u32) {
        let pix_bytes = (w * h * 4) as u32;
        let mut out: Vec<u8> = Vec::with_capacity((14 + 40 + pix_bytes) as usize);

        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(14u32 + 40 + pix_bytes).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(14u32 + 40).to_le_bytes());

        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&pix_bytes.to_le_bytes());
        out.extend_from_slice(&2835i32.to_le_bytes());
        out.extend_from_slice(&2835i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());

        for y in (0..h).rev() {
            for x in 0..w {
                let c = unsafe { *bits.add((y * w + x) as usize) };
                out.push((c & 0xFF) as u8);
                out.push((c >> 8 & 0xFF) as u8);
                out.push((c >> 16 & 0xFF) as u8);
                out.push(0);
            }
        }

        std::fs::write(path, out).expect("BMP を書けない");
    }

    fn out_dir() -> std::path::PathBuf {
        std::env::var_os("DPDFNET_SNAPSHOT_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }

    fn render(scale: f32, path: &std::path::Path) {
        let meters = Arc::new(Meters::new());
        // 入力レベル（アナログ VU 用）。
        meters.set_in(0, 0.09);
        meters.set_in(1, 0.055);
        // 出力レベル（GR 用）。入力より 10 dB 以上下 = 抑圧が効いている状態。
        meters.set_out(0, 0.022);
        meters.set_out(1, 0.014);
        // 出力 3 バンド（LOW 強め / MID 中 / HIGH 弱め）。
        meters.set(0, 0, 0.55);
        meters.set(1, 0, 0.42);
        meters.set(0, 1, 0.18);
        meters.set(1, 1, 0.13);
        meters.set(0, 2, 0.03);
        meters.set(1, 2, 0.02);

        let shared = Arc::new(Shared::new(meters));
        shared.params.set_ambience(0.62);
        shared.params.set_voice(2.5);
        shared.params.set_voice_reverb(0.28);
        shared.dereverb.store(true, Ordering::Relaxed);
        shared.channels.store(2, Ordering::Relaxed);
        shared.set_status_detail(
            ST_OK,
            "48 kHz / ch=2 / latency 2880 samples / dpdfnet8_48khz_hr_2out.onnx | n_fft=960 hop=480 state=90228 outputs=2",
        );
        shared.set_summary("2 ch  ·  48 kHz  ·  遅延 2880 samples (60 ms)  ·  dereverb あり".into());

        let w = (CLIENT_W * scale).round() as i32;
        let h = (CLIENT_H * scale).round() as i32;

        let mut panel = Panel::new(Arc::clone(&shared), std::ptr::null_mut(), scale);
        panel.dib = Some(unsafe { Dib::new(w, h) }.expect("DIB"));
        panel.fonts = Some(unsafe { Fonts::new(scale) });
        panel.layout = panel.compute_layout(w, h);
        // ボールistics を掛けたあとの値を直接入れて見た目を確認する。
        panel.vu_display = [vu_to_pos(0.09), vu_to_pos(0.055)];
        panel.gr_display = gr_to_display(0.09, 0.022);
        panel.bands_display = [0.62, 0.30, 0.08];
        panel.bands_peak = [0.71, 0.36, 0.11];
        panel.paint();

        let dib = panel.dib.as_ref().unwrap();
        write_bmp(path, w, h, dib.bits);
        eprintln!(
            "snapshot: {} ({}x{})\n  title={:?}\n  status={:?}\n  button={:?}\n  section={:?}\n  vu={:?}\n  fader={:?}\n  fader_track={:?}\n  gr={:?}\n  bands={:?}",
            path.display(),
            w,
            h,
            panel.layout.title,
            panel.layout.status,
            panel.layout.button,
            panel.layout.section,
            panel.layout.vu,
            panel.layout.fader,
            panel.layout.fader_track,
            panel.layout.gr,
            panel.layout.bands
        );
    }

    #[test]
    fn snapshot_panel_1x() {
        render(1.0, &out_dir().join("dpdfnet_panel_1x.bmp"));
    }

    #[test]
    fn snapshot_panel_150() {
        render(1.5, &out_dir().join("dpdfnet_panel_150.bmp"));
    }

    /// REVERB 無効（モデル未配置）+ モノラル時の見た目。
    #[test]
    fn snapshot_panel_disabled() {
        let meters = Arc::new(Meters::new());
        meters.set(0, 0, 0.30);
        let shared = Arc::new(Shared::new(meters));
        shared.params.set_ambience(1.0);
        // モノラル（R 側の VU が休む）も確認する。
        shared.channels.store(1, Ordering::Relaxed);
        shared.set_status_detail(
            crate::filter::ST_MODEL_NOT_FOUND,
            "DPDFNet モデルが見つかりません (探索先: C:\\ProgramData\\obs-studio\\plugins\\obs_noise_suppressor\\data\\models)",
        );
        let (w, h) = (CLIENT_W.round() as i32, CLIENT_H.round() as i32);
        let mut panel = Panel::new(Arc::clone(&shared), std::ptr::null_mut(), 1.0);
        panel.dib = Some(unsafe { Dib::new(w, h) }.expect("DIB"));
        panel.fonts = Some(unsafe { Fonts::new(1.0) });
        panel.layout = panel.compute_layout(w, h);
        panel.bands_display = [0.22, 0.0, 0.0];
        panel.bands_peak = [0.30, 0.0, 0.0];
        panel.paint();
        let dib = panel.dib.as_ref().unwrap();
        write_bmp(&out_dir().join("dpdfnet_panel_disabled.bmp"), w, h, dib.bits);
    }
}
