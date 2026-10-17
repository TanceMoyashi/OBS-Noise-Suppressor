//! 自前 Win32 パネル（GDI + DIB 描画）。
//!
//! 旧 VST3 版の `DPDFNetVoiceEditor`（JUCE）を Rust + 生 Win32 で描き直したもの。
//! 見た目はできるだけ同じにしてある:
//!
//!   * 暗い縦グラデ背景 + 上端の淡いアクセントグロー
//!   * ヘッダ（タイトル + サブタイトル + 区切り線）
//!   * LOW / MID / HIGH の 3 バンドメータ（アクセント→暖色のグラデ + ピークホールド）
//!   * ノブ 3 つ（外周トラック / 値アーク / 本体のグラデ / インジケータ）
//!   * ステータスドット + テキスト、BYPASS のピル型トグル
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
//! 直接書き、枠・文字・円弧は GDI に任せる。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, DeleteDC,
    DeleteObject, DrawTextW, Ellipse, EndPaint, ExtCreatePen, GetStockObject,
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
// レイアウト（PluginEditor.cpp の resized() と同じ寸法。論理ピクセル）
// --------------------------------------------------------------------------

const K_RADIUS: f32 = 10.0;
const K_MARGIN: f32 = 22.0;
const K_GAP: f32 = 8.0;
const K_HEADER_H: f32 = 44.0;
const K_FOOTER_H: f32 = 36.0;
const K_METER_H: f32 = 46.0;
const K_METER_GAP: f32 = 5.0;
const K_METER_LABEL_W: f32 = 40.0;
const K_KNOB_LABEL_H: f32 = 18.0;
const K_VALUE_H: f32 = 18.0;
const K_CELL_PAD_X: f32 = 14.0;
const K_CELL_PAD_Y: f32 = 2.0;
const K_BUTTON_W: f32 = 104.0;
const K_BUTTON_H: f32 = 30.0;

const CLIENT_W: f32 = 620.0;
const CLIENT_H: f32 = 340.0;

/// ノブの回転範囲。JUCE の `getRotaryParameters()` と同じ
/// （0° = 12 時、時計回り、216° から 288° ぶん）。
const ROTARY_START: f32 = 216.0;
const ROTARY_SWEEP: f32 = 288.0;

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
    fn inset(&self, x: i32, y: i32) -> IR {
        IR { l: self.l + x, t: self.t + y, r: self.r - x, b: self.b - y }
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
// ノブ定義
// --------------------------------------------------------------------------

struct KnobSpec {
    name: &'static str,
    min: f32,
    max: f32,
    default: f32,
    percent: bool,
}

const KNOBS: [KnobSpec; 3] = [
    KnobSpec {
        name: "AMBIENCE",
        min: dsp::AMBIENCE_MIN as f32,
        max: dsp::AMBIENCE_MAX as f32,
        default: dsp::AMBIENCE_DEFAULT as f32,
        percent: true,
    },
    KnobSpec {
        name: "VOICE",
        min: dsp::VOICE_MIN as f32,
        max: dsp::VOICE_MAX as f32,
        default: dsp::VOICE_DEFAULT as f32,
        percent: false,
    },
    KnobSpec {
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
    meter: IR,
    status: IR,
    button: IR,
    knob: [IR; 3],
    value: [IR; 3],
    label: [IR; 3],
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

    /// メータ用: 左 accent → 62% で accent2 → 右 warm
    /// （JUCE の `ColourGradient` + `addColour(0.62, accent2)` と同じ）。
    fn fill_round_rect_hgrad(&mut self, r: IR, rad: i32, c0: u32, c1: u32, c2: u32) {
        let span = (r.w() - 1).max(1) as f32;
        for y in r.t.max(0)..r.b.min(self.h) {
            for x in r.l.max(0)..r.r.min(self.w) {
                if !inside_round_rect(x, y, r, rad) {
                    continue;
                }
                let t = (x - r.l) as f32 / span;
                let c = if t < 0.62 {
                    lerp_rgb(c0, c1, t / 0.62)
                } else {
                    lerp_rgb(c1, c2, (t - 0.62) / 0.38)
                };
                self.put(x, y, c);
            }
        }
    }

    /// ノブ本体用: 上から下への縦グラデ。
    fn fill_ellipse_vgrad(&mut self, cx: i32, cy: i32, r: f32, top: u32, bottom: u32) {
        let r2 = r * r;
        let y0 = ((cy as f32 - r).floor() as i32).max(0);
        let y1 = ((cy as f32 + r).ceil() as i32).min(self.h);
        for y in y0..y1 {
            let dy = y as f32 + 0.5 - cy as f32;
            let t = ((dy + r) / (2.0 * r)).clamp(0.0, 1.0);
            let c = lerp_rgb(top, bottom, t);
            let half = (r2 - dy * dy).max(0.0).sqrt();
            let x0 = ((cx as f32 - half).floor() as i32).max(0);
            let x1 = ((cx as f32 + half).ceil() as i32).min(self.w);
            for x in x0..x1 {
                self.put(x, y, c);
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
    knob: usize,
    start_y: i32,
    start_value: f32,
}

/// 描画に必要な値を `self` から先に取り出しておくための束
/// （`&mut self.dib` を握ったままメソッドを呼べないため）。
struct Snapshot {
    scale: f32,
    layout: Layout,
    meter: [f32; 3],
    peak: [f32; 3],
    status: u32,
    status_text: String,
    summary: String,
    values: [f32; 3],
    dereverb: bool,
    enabled: bool,
    hover_button: bool,
    press_button: bool,
}

struct Panel {
    shared: Arc<Shared>,
    source: *mut std::ffi::c_void,
    hwnd: HWND,
    scale: f32,
    dib: Option<Dib>,
    fonts: Option<Fonts>,
    layout: Layout,
    meter_display: [f32; 3],
    meter_peak: [f32; 3],
    drag: Option<Drag>,
    /// ドラッグ中に触ったノブ（離したときに OBS 設定へ書き戻す）。
    pending_push: Option<usize>,
    hover_button: bool,
    press_button: bool,
    hover_knob: Option<usize>,
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
            meter_display: [0.0; 3],
            meter_peak: [0.0; 3],
            drag: None,
            pending_push: None,
            hover_button: false,
            press_button: false,
            hover_knob: None,
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
        for b in 0..3 {
            let target = rms_to_display(self.shared.meters.get(0, b).max(self.shared.meters.get(1, b)));

            // 立ち上がりは即時、戻りは滑らかに（JUCE 版と同じ）。
            let cur = self.meter_display[b];
            self.meter_display[b] = if target > cur { target } else { cur + (target - cur) * 0.25 };
            self.meter_peak[b] = (self.meter_peak[b] - 0.02).max(self.meter_display[b]);
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
            if hit(&self.layout.knob[i], x, y) && self.knob_enabled(i) {
                self.drag = Some(Drag { knob: i, start_y: y, start_value: self.knob_value(i) });
                unsafe {
                    SetCapture(self.hwnd);
                }
                return;
            }
        }
    }

    fn on_mouse_dbl(&mut self, x: i32, y: i32) {
        for i in 0..3 {
            if hit(&self.layout.knob[i], x, y) && self.knob_enabled(i) {
                self.apply_knob(i, KNOBS[i].default, true);
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
            let spec = &KNOBS[d.knob];
            let span = spec.max - spec.min;
            let delta = (d.start_y - y) as f32 * span / (200.0 * self.scale.max(0.1));
            let v = (d.start_value + delta).clamp(spec.min, spec.max);
            let knob = d.knob;
            self.apply_knob(knob, v, false);
            return;
        }

        let hb = hit(&self.layout.button, x, y);
        let hk = (0..3).find(|i| hit(&self.layout.knob[*i], x, y) && self.knob_enabled(*i));
        if hb != self.hover_button || hk != self.hover_knob {
            self.hover_button = hb;
            self.hover_knob = hk;
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
            if hit(&self.layout.knob[i], pt.x, pt.y) && self.knob_enabled(i) {
                let span = KNOBS[i].max - KNOBS[i].min;
                let step = span * 0.02 * (delta as f32 / 120.0);
                let v = (self.knob_value(i) + step).clamp(KNOBS[i].min, KNOBS[i].max);
                self.apply_knob(i, v, true);
                return;
            }
        }
    }

    // ----------------------------------------------------------------------
    // パラメータ
    // ----------------------------------------------------------------------

    fn knob_value(&self, i: usize) -> f32 {
        match i {
            0 => self.shared.params.ambient(),
            1 => self.shared.params.voice(),
            _ => self.shared.params.voice_reverb(),
        }
    }

    fn knob_enabled(&self, i: usize) -> bool {
        // REVERB は 2 出力モデルのときだけ有効（JUCE 版と同じ）。
        i != 2 || self.shared.dereverb.load(Ordering::Relaxed)
    }

    fn apply_knob(&mut self, i: usize, v: f32, push: bool) {
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

        out.meter = area.bottom(self.sc(K_METER_H));
        area.b -= self.sc(K_METER_H) + self.sc(K_GAP);

        let cw = area.w() / 3;
        for i in 0..3usize {
            let cell = IR {
                l: area.l + cw * i as i32,
                t: area.t,
                r: area.l + cw * (i as i32 + 1),
                b: area.b,
            }
            .inset(self.sc(K_CELL_PAD_X), self.sc(K_CELL_PAD_Y));

            out.label[i] = cell.top(self.sc(K_KNOB_LABEL_H));
            let body = IR { t: cell.t + self.sc(K_KNOB_LABEL_H), ..cell };
            out.value[i] = body.bottom(self.sc(K_VALUE_H));
            out.knob[i] = IR { b: body.b - self.sc(K_VALUE_H), ..body }.inset(self.sc(3.0), self.sc(3.0));
        }

        out
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            scale: self.scale,
            layout: self.layout,
            meter: self.meter_display,
            peak: self.meter_peak,
            status: self.shared.status(),
            status_text: self.shared.status_detail(),
            summary: self.shared.summary(),
            values: [self.knob_value(0), self.knob_value(1), self.knob_value(2)],
            dereverb: self.shared.dereverb.load(Ordering::Relaxed),
            enabled: if self.shared.alive.load(Ordering::SeqCst) && !self.source.is_null() {
                unsafe { crate::ffi::obs_source_enabled(self.source as *mut _) }
            } else {
                true
            },
            hover_button: self.hover_button,
            press_button: self.press_button,
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

            // ---- 3 バンドメータ ----
            let mg = sc(K_METER_GAP);
            let row_h = (lay.meter.h() - 2 * mg) / 3;
            let lw = sc(K_METER_LABEL_W);
            let th = row_h.min(sc(10.0)).max(2);
            let names = ["LOW", "MID", "HIGH"];

            for b in 0..3usize {
                let row_t = lay.meter.t + b as i32 * (row_h + mg);
                let row = IR { l: lay.meter.l, t: row_t, r: lay.meter.r, b: row_t + row_h };
                let lab = IR { r: row.l + lw, ..row };
                let track = IR { l: row.l + lw, r: row.r, t: row.cy() - th / 2, b: row.cy() + th / 2 };

                draw_text(
                    dc,
                    names[b],
                    lab,
                    fonts.tiny,
                    TEXT_DIM,
                    DT_RIGHT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
                );

                dib.fill_round_rect(track, th / 2, SLOT);

                let level = s.meter[b].clamp(0.0, 1.0);
                if level > 0.002 {
                    let lit = IR { r: track.l + (track.w() as f32 * level) as i32, ..track };
                    dib.fill_round_rect_hgrad(lit, th / 2, ACCENT, ACCENT2, WARM);
                }

                let pk = s.peak[b].clamp(0.0, 1.0);
                if pk > 0.004 {
                    let x = track.l + (track.w() as f32 * pk) as i32;
                    dib.fill_rect_alpha(IR { l: x - 1, r: x + 1, ..track }, TEXT, 0.7);
                }

                stroke_round_rect(dc, track, th / 2, dim(STROKE, 0.55), 1);
            }

            // ---- ノブ ----
            for i in 0..3usize {
                let on = i != 2 || s.dereverb;
                paint_knob(
                    dib, dc, fonts, s.scale, lay.knob[i], lay.value[i], lay.label[i], i,
                    s.values[i], on,
                );
            }

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
// ノブ描画（PluginEditor.cpp の drawRotarySlider と同じ手順）
// --------------------------------------------------------------------------

unsafe fn paint_knob(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    knob: IR,
    value_r: IR,
    label_r: IR,
    index: usize,
    value: f32,
    on: bool,
) {
    let spec = &KNOBS[index];
    let pos = ((value - spec.min) / (spec.max - spec.min).max(1e-9)).clamp(0.0, 1.0);

    let diameter = (knob.w() as f32).min(knob.h() as f32).max(8.0);
    let radius = diameter * 0.5;
    let (cx, cy) = (knob.cx() as f32, knob.cy() as f32);

    let line_w = (radius * 0.095).max(3.0 * scale);
    let arc_r = (radius - line_w * 0.5).max(1.0);
    let angle = ROTARY_START + pos * ROTARY_SWEEP;
    let a = if on { 1.0 } else { 0.4 };

    // 外周トラック
    stroke_arc(
        dc,
        cx,
        cy,
        arc_r,
        ROTARY_START,
        ROTARY_START + ROTARY_SWEEP,
        line_w,
        dim(TRACK, if on { 1.0 } else { 0.5 }),
    );

    // 値のアーク（x 位置で accent -> accent2）
    if pos > 0.0005 {
        let n = 28;
        for k in 0..n {
            let t0 = k as f32 / n as f32;
            let t1 = (k + 1) as f32 / n as f32;
            let a0 = ROTARY_START + (angle - ROTARY_START) * t0;
            let a1 = ROTARY_START + (angle - ROTARY_START) * t1;
            let mid = ((a0 + a1) * 0.5).to_radians();
            let mx = cx + arc_r * mid.sin();
            let tx = ((mx - (cx - arc_r)) / (2.0 * arc_r)).clamp(0.0, 1.0);
            stroke_arc(dc, cx, cy, arc_r, a0, a1, line_w, dim(lerp_rgb(ACCENT, ACCENT2, tx), a));
        }
    }

    // ノブ本体
    let body_r = arc_r - line_w * 0.95;
    if body_r > 3.0 * scale {
        let d = if on { 1.0 } else { 0.78 };
        dib.fill_ellipse_vgrad(
            cx.round() as i32,
            cy.round() as i32,
            body_r,
            dim(0x303a44, d),
            dim(0x15191e, d),
        );
        let br = IR {
            l: (cx - body_r).round() as i32,
            t: (cy - body_r).round() as i32,
            r: (cx + body_r).round() as i32,
            b: (cy + body_r).round() as i32,
        };
        stroke_ellipse(dc, br, dim(0x39424d, if on { 0.85 } else { 0.4 }), 1);
    }

    // インジケータ
    {
        let r_in = body_r * 0.30;
        let r_out = body_r * 0.84;
        let (sn, cs) = (angle.to_radians().sin(), angle.to_radians().cos());
        stroke_line(
            dc,
            cx + r_in * sn,
            cy - r_in * cs,
            cx + r_out * sn,
            cy - r_out * cs,
            (body_r * 0.12).max(2.0 * scale),
            dim(TEXT, if on { 1.0 } else { 0.35 }),
        );
    }

    // 値
    let text = if spec.percent {
        format!("{} %", (value * 100.0).round() as i32)
    } else {
        format!("{:.1} dB", value)
    };
    draw_text(
        dc,
        &text,
        value_r,
        fonts.value,
        if on { ACCENT } else { dim(ACCENT, 0.5) },
        DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );

    // ラベル
    draw_text(
        dc,
        spec.name,
        label_r,
        fonts.label,
        if on { TEXT_DIM } else { dim(TEXT_DIM, 0.4) },
        DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );
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

unsafe fn stroke_ellipse(dc: HDC, r: IR, color: u32, width: i32) {
    let pen = CreatePen(PS_SOLID, width.max(1), COLORREF(colorref(color)));
    if pen.0.is_null() {
        return;
    }
    let old_pen = SelectObject(dc, HGDIOBJ(pen.0));
    let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
    let _ = Ellipse(dc, r.l, r.t, r.r, r.b);
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
        // それらしいレベル（LOW 強め / MID 中 / HIGH 弱め）。
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
        shared.set_status_detail(
            ST_OK,
            "48 kHz / ch=2 / latency 2880 samples / dpdfnet2_48khz_hr_2out.onnx | n_fft=960 hop=480 state=56436 outputs=2",
        );
        shared.set_summary("2 ch  ·  48 kHz  ·  遅延 2880 samples (60 ms)  ·  dereverb あり".into());

        let w = (CLIENT_W * scale).round() as i32;
        let h = (CLIENT_H * scale).round() as i32;

        let mut panel = Panel::new(Arc::clone(&shared), std::ptr::null_mut(), scale);
        panel.dib = Some(unsafe { Dib::new(w, h) }.expect("DIB"));
        panel.fonts = Some(unsafe { Fonts::new(scale) });
        panel.layout = panel.compute_layout(w, h);
        panel.meter_display = [0.62, 0.30, 0.08];
        panel.meter_peak = [0.71, 0.36, 0.11];
        panel.paint();

        let dib = panel.dib.as_ref().unwrap();
        write_bmp(path, w, h, dib.bits);
        eprintln!(
            "snapshot: {} ({}x{})\n  title={:?}\n  meter={:?}\n  status={:?}\n  button={:?}\n  label={:?}\n  knob={:?}\n  value={:?}",
            path.display(),
            w,
            h,
            panel.layout.title,
            panel.layout.meter,
            panel.layout.status,
            panel.layout.button,
            panel.layout.label,
            panel.layout.knob,
            panel.layout.value
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

    /// REVERB 無効（1 出力モデル）+ バイパス中の見た目。
    #[test]
    fn snapshot_panel_disabled() {
        let meters = Arc::new(Meters::new());
        meters.set(0, 0, 0.30);
        let shared = Arc::new(Shared::new(meters));
        shared.params.set_ambience(1.0);
        shared.set_status_detail(
            crate::filter::ST_MODEL_NOT_FOUND,
            "DPDFNet モデルが見つかりません (探索先: C:\\ProgramData\\obs-studio\\plugins\\obs_noise_suppressor\\data\\models)",
        );
        let mut panel = Panel::new(Arc::clone(&shared), std::ptr::null_mut(), 1.0);
        panel.dib = Some(unsafe { Dib::new(620, 340) }.expect("DIB"));
        panel.fonts = Some(unsafe { Fonts::new(1.0) });
        panel.layout = panel.compute_layout(620, 340);
        panel.meter_display = [0.22, 0.0, 0.0];
        panel.meter_peak = [0.30, 0.0, 0.0];
        panel.paint();
        let dib = panel.dib.as_ref().unwrap();
        write_bmp(
            &out_dir().join("dpdfnet_panel_disabled.bmp"),
            620,
            340,
            dib.bits,
        );
    }
}
