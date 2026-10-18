//! 自前 Win32 パネル（GDI + DIB 描画）。
//!
//! 旧 VST3 版の `DPDFNetVoiceEditor`（JUCE）を Rust + 生 Win32 で描き直したもの。
//! 見た目は「黒アルマイトの面板 + アンバーに灯る VU 盤面 + 彫り込み文字」の
//! 業務用オーディオ機器風（実機の VU メーターとコンソールを参考にしている）。
//!
//!   * ヘッダ（タイトル + サブタイトル + メモリー 1〜8 + SAVE）
//!   * IN / OUT: 入力（加工前）と出力（加工後）のアナログ VU メーター
//!               （針 + VU 目盛り）+ 現在値 / ピークの数値窓。ステレオは
//!               L/R の指標を 1 本にまとめる（`crate::meter` 参照）
//!   * CONTROL : 縦フェーダー 3 本（AMBIENCE / VOICE / REVERB）+ dB / % の併記、
//!               各フェーダーの下に MUTE / BYP キー（つまみの位置は保つ）
//!   * EFFECT  : GR メーター（コンプの GR と同じく 0 から下へ振れる）+ 出力 3 バンド
//!   * ステータス LED + テキスト、有効 / 無効のトグル
//!
//! 質感の作り方（参考写真から拾った要素）:
//!
//!   * 面板 = 縦のヘアライン + ざらつき + 面取りの縁 + 四隅の十字ネジ
//!   * VU   = 上から暖色に灯る盤面（光源のにじみ）、太い黒の弧、0 〜 +3 の赤帯、
//!            2 本目の細い弧（50 / 100 %）、ガラスの斜めの反射、細い黒針
//!
//! ## スレッドモデル
//!
//! パネルは**専用スレッド**で作る。OBS 側のメッセージループ（Qt）に依存せず、
//! ウィンドウの寿命とメッセージポンプを自分で持つため。`Shared` は `Arc` なので
//! OBS 側のフィルタが先に破棄されてもぶら下がりポインタにならない。
//! `obs_source_t*` だけは OBS が所有するので、`Shared::alive` が false に
//! なったら触らない。
//!
//! 描画は「DIB セクションに描いて `BitBlt`」方式。グラデーション・質感は
//! ピクセルを直接書き、枠・文字・針は GDI に任せる。
//!
//! ## メーターの動き
//!
//! 針の弾道（VU の時定数・ピークホールド・GR の戻り）は `crate::meter` に
//! まとめてあり、ここでは 40 ms ごとに 1 ティック進めるだけ。

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, DeleteDC,
    DeleteObject, DrawTextW, EndPaint, GetStockObject, InvalidateRect, LineTo, MoveToEx, RoundRect,
    ScreenToClient, SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_CENTER,
    DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FONT_QUALITY, HBRUSH, HBITMAP,
    HDC, HFONT, HGDIOBJ, NULL_BRUSH, OUT_TT_PRECIS, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetActiveWindow, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, KillTimer, LoadCursorW,
    PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, CREATESTRUCTW, CS_DBLCLKS,
    CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, MINMAXINFO, MSG, SM_CXSCREEN, SM_CYSCREEN,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOW, WM_CLOSE, WM_CREATE, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SIZE,
    WM_TIMER, WNDCLASSW, WS_CAPTION, WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_SYSMENU,
};

use crate::dsp::params as dsp;
use crate::filter::{Shared, ST_OK};
use crate::meter::{self, MeterState, Reading};
use crate::paths::ModelChoice;
use crate::preset::{Preset, SLOTS};

// --------------------------------------------------------------------------
// パレット: 黒アルマイト + アンバーに灯る VU + 彫り込み文字
// --------------------------------------------------------------------------

/// パネル全体（アルミの地）の上下端。
const PANEL_TOP: u32 = 0x23262a;
const PANEL_BOT: u32 = 0x16181b;
/// ウィンドウ外周の地（最外周 1 px）。
const BG: u32 = 0x0b0c0e;
/// 面取り（上左 = 光 / 下右 = 影）。
const BEVEL_HI: u32 = 0x3c4249;
const BEVEL_LO: u32 = 0x06070a;
/// 彫り込み文字の「影」（文字の下に入る暗い線）。
const ENGRAVE_HI: u32 = 0x07080a;
/// シルク印刷（静的なラベル）。
const SILK: u32 = 0xb6bcc3;
const SILK_DIM: u32 = 0x818a93;
const SILK_FAINT: u32 = 0x565d65;
/// 数値表示（動的な値）。
const VALUE: u32 = 0xdfe5ea;
const VALUE_DIM: u32 = 0x9aa3ac;
/// VU 盤面。実機と同じくランプで後ろから照らした「暖かいアイボリー」。
/// 上ほどアンバー、下へ行くほど淡いクリームになる。
const VU_FACE_TOP: u32 = 0xf6dda6;
const VU_FACE_BOT: u32 = 0xe4d9bd;
/// 盤面ににじむ光源（バックライトのムラ）。
const VU_HOT: u32 = 0xffc964;
/// 盤面のインク（目盛り・文字）。
const VU_INK: u32 = 0x1b1914;
const VU_INK_DIM: u32 = 0x6d6656;
/// 0 VU から上の赤（オーバーロード帯）。
const VU_RED: u32 = 0xb02c1f;
/// VU の針（実機は細い黒針。先端だけわずかに赤い）。
const NEEDLE: u32 = 0x14130f;
const NEEDLE_TIP: u32 = 0x8e2f26;
/// 赤（BYPASS / LED など）。
const RED: u32 = 0xa9362a;
/// アクセント（フェーダーの値 / 3 バンドの低域側）。
const ACCENT: u32 = 0x4bbfae;
const ACCENT_HI: u32 = 0x8ce9d8;
/// アンバー（GR / LED）。
const WARM: u32 = 0xe0a24b;
const WARM_HI: u32 = 0xf7d79b;
/// LED の色。
const LED_GREEN: u32 = 0x5fd07d;
const LED_RED: u32 = 0xd2453a;

// --------------------------------------------------------------------------
// レイアウト（論理ピクセル）
// --------------------------------------------------------------------------

/// クライアント領域の既定サイズ。リサイズもできる（最小は下の 2 つ）。
const CLIENT_W: f32 = 780.0;
const CLIENT_H: f32 = 400.0;
const MIN_W: f32 = 700.0;
const MIN_H: f32 = 360.0;

const K_MARGIN: f32 = 24.0;
const K_GAP: f32 = 10.0;
const K_HEADER_H: f32 = 46.0;
const K_FOOTER_H: f32 = 34.0;
const K_SECTION_TITLE_H: f32 = 16.0;
const K_SECTION_GAP: f32 = 16.0;

/// 左（入力 VU）セクションと、真ん中（フェーダー）セクションの幅。
/// 残りはすべて EFFECT（GR + 3 バンド）に渡す。
const K_INPUT_W: f32 = 232.0;
const K_CONTROL_W: f32 = 224.0;

/// 入力 VU 2 本の縦間隔と、数値欄の幅。
const K_VU_GAP: f32 = 8.0;
const K_VU_NUM_W: f32 = 58.0;

/// フェーダー 1 本ぶん。
const K_FADER_LABEL_H: f32 = 15.0;
const K_FADER_VALUE_H: f32 = 15.0;
const K_FADER_TRACK_W: f32 = 9.0;
const K_FADER_HANDLE_W: f32 = 30.0;
const K_FADER_HANDLE_H: f32 = 14.0;
/// MUTE / BYP キーの高さ（フェーダーの上に 1 段）。
const K_FADER_BTN_H: f32 = 15.0;

/// GR メーター（縦・0 から下へ振れるコンプの GR 風）。
const K_GR_BAR_W: f32 = 20.0;
const K_GR_SCALE_W: f32 = 28.0;
const K_GR_NUM_W: f32 = 56.0;

/// 3 バンド（低 / 中 / 高）バー。
const K_BAND_W: f32 = 14.0;
const K_BAND_GAP: f32 = 8.0;
const K_BAND_LABEL_H: f32 = 14.0;

/// メモリー（スロット 8 個 + SAVE）。
const K_SLOT_W: f32 = 24.0;
const K_SLOT_H: f32 = 20.0;
const K_SLOT_GAP: f32 = 4.0;
const K_SAVE_W: f32 = 48.0;
const K_MEMORY_LABEL_W: f32 = 56.0;

const K_BUTTON_W: f32 = 96.0;
const K_BUTTON_H: f32 = 24.0;

/// ネジ（四隅）。
const K_SCREW_R: f32 = 5.0;
const K_SCREW_INSET: f32 = 12.0;

/// VU の目盛り: 0 VU を基準に -20 VU 〜 +3 VU を出す（基準値で平行移動）。
const VU_VU_AT_MIN: f32 = -20.0;
const VU_VU_AT_MAX: f32 = 3.0;
const VU_ANGLE_MIN: f32 = -55.0;
const VU_ANGLE_MAX: f32 = 55.0;
/// 針の支点を盤面の下端からどれだけ上げるか。
const VU_PIVOT_LIFT: f32 = 5.0;

/// GR メーターの振れ幅（0 〜 -30 dB）。
const GR_DB_RANGE: f32 = 30.0;

/// リペイント間隔 (ms)。JUCE 版の `startTimerHz(24)` に合わせる。
const TIMER_MS: u32 = 40;

/// メモリー操作のメッセージを出しておく時間 (ms)。
const FLASH_MS: u64 = 1800;

/// 検証用のパネル画像（`DPDFNET_PANEL_SNAPSHOT`）を書くまでに待つ時間。
/// 音が流れてメーターが動いてから撮るため。
const DUMP_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

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
// スレッド / ウィンドウクラス
// --------------------------------------------------------------------------

static PANEL_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static PANEL_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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

    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;

    // 画面中央（オーナーがあればその中央）に置く。サイズは WM_CREATE で
    // クライアント CLIENT_W x CLIENT_H に補正する。
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
    crate::obs_log!(
        crate::ffi::LOG_INFO,
        "パネルを表示しました ({x}, {y}, {w}x{h}, scale {ratio:.2})"
    );

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
        WM_GETMINMAXINFO => {
            // 潰れて読めなくなるのを防ぐ（物理ピクセル指定）。
            let mm = &mut *(l.0 as *mut MINMAXINFO);
            mm.ptMinTrackSize.x = panel.sc(MIN_W);
            mm.ptMinTrackSize.y = panel.sc(MIN_H);
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
            panel.on_mouse_down(x, y, w.0 as u32);
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
            match w.0 as u32 {
                // ESC で閉じる。
                0x1B => {
                    let _ = DestroyWindow(hwnd);
                }
                // S = 保存モードの切り替え / R = 保存モード解除。
                0x53 => panel.toggle_save_mode(),
                0x52 => {
                    panel.save_armed = false;
                    panel.invalidate();
                }
                _ => {}
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
    /// 左から `w` ぶん切り出す。
    fn left(&self, w: i32) -> IR {
        IR { r: self.l + w, ..*self }
    }
    /// 右から `w` ぶん切り出す。
    fn right(&self, w: i32) -> IR {
        IR { l: self.r - w, ..*self }
    }
    fn inset(&self, d: i32) -> IR {
        IR { l: self.l + d, t: self.t + d, r: self.r - d, b: self.b - d }
    }
}

#[inline]
fn hit(r: &IR, x: i32, y: i32) -> bool {
    x >= r.l && x < r.r && y >= r.t && y < r.b
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

/// 無効状態を「面板の地に寄せる」ことで表現する（JUCE の withMultipliedAlpha 相当）。
#[inline]
fn dim(c: u32, amount: f32) -> u32 {
    lerp_rgb(PANEL_BOT, c, amount)
}

#[inline]
fn lighten(c: u32, t: f32) -> u32 {
    lerp_rgb(c, 0xFFFFFF, t)
}

#[inline]
fn colorref(c: u32) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | (c >> 16 & 0xFF)
}

/// チャンネルごとに符号付きのざらつきを足す（アルマイトの質感）。
#[inline]
fn grain(c: u32, n: i32) -> u32 {
    let ch = |shift: u32| -> u32 {
        let v = ((c >> shift) & 0xFF) as i32 + n;
        v.clamp(0, 255) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// 決定的な擬似ノイズ (-1..1)。同じ座標なら常に同じ値。
#[inline]
fn hash2(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x27d4_eb2d) ^ (y as u32).wrapping_mul(0x1656_67b1);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_f491);
    h ^= h >> 13;
    ((h & 0xFFFF) as f32 / 32767.5) - 1.0
}

/// `pos` が丸角長方形の内側か。`clamp` は min > max で panic するので注意。
fn inside_round_rect(x: i32, y: i32, r: IR, rad: i32) -> bool {
    if x < r.l || x >= r.r || y < r.t || y >= r.b {
        return false;
    }
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

/// リニア RMS -> VU の針位置 0..1。
///
/// 0 VU = `vref` dBFS とし、-20 VU 〜 +3 VU を表示する。
/// 基準値を変えると目盛り全体（= 対応する dBFS の範囲）が平行移動する。
fn vu_to_pos(lin: f32, vref: f32) -> f32 {
    if !(lin > 0.0) {
        return 0.0;
    }
    let db = 20.0 * lin.log10();
    let lo = -(vref + -VU_VU_AT_MIN); // -20 VU に当たる dBFS
    let hi = -(vref - VU_VU_AT_MAX); // +3 VU に当たる dBFS
    ((db - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// dB 表示用の文字列。下限以下は `-∞`。
fn db_text(db: f32) -> String {
    if db <= meter::DB_FLOOR + 0.05 {
        "-∞".to_string()
    } else {
        format!("{db:.1}")
    }
}

// --------------------------------------------------------------------------
// フェーダー定義
// --------------------------------------------------------------------------

struct FaderSpec {
    name: &'static str,
    min: f32,
    max: f32,
    default: f32,
    /// 上が % 表示か（dB を上に出すか）。
    percent_first: bool,
}

const FADERS: [FaderSpec; 3] = [
    FaderSpec {
        name: "AMBIENCE",
        min: dsp::AMBIENCE_MIN as f32,
        max: dsp::AMBIENCE_MAX as f32,
        default: dsp::AMBIENCE_DEFAULT as f32,
        percent_first: false,
    },
    FaderSpec {
        name: "VOICE",
        min: dsp::VOICE_MIN as f32,
        max: dsp::VOICE_MAX as f32,
        default: dsp::VOICE_DEFAULT as f32,
        percent_first: false,
    },
    FaderSpec {
        name: "REVERB",
        min: dsp::VOICE_REVERB_MIN as f32,
        max: dsp::VOICE_REVERB_MAX as f32,
        default: dsp::VOICE_REVERB_DEFAULT as f32,
        percent_first: true,
    },
];

/// フェーダーの表示文字列 `(上段, 下段)`。dB と % を併記する。
fn fader_text(i: usize, v: f32) -> (String, String) {
    let pc = |t: f32| format!("{} %", (t * 100.0).round() as i32);
    match i {
        // AMBIENCE: 上 = 抑圧の上限 [dB]（-48 〜 -6 dB）、下 = %.
        0 => {
            let db = -(dsp::ambience_to_attenuation_db(v) as f64);
            (format!("{} dB", db_text(db as f32)), pc(v))
        }
        // VOICE: 上 = dB、下 = -12..+12 dB を 0..100 % とみなした値。
        1 => {
            let t = (v - dsp::VOICE_MIN as f32) / (dsp::VOICE_MAX as f32 - dsp::VOICE_MIN as f32);
            (format!("{v:+.1} dB"), pc(t))
        }
        // REVERB: 上 = %、下 = 残響成分のレベル [dB]（0 % は -∞）。
        _ => {
            let db = if v > 0.0 { 20.0 * v.log10() } else { meter::DB_FLOOR };
            (pc(v), format!("{} dB", db_text(db)))
        }
    }
}

/// MUTE / BYP を押したときの一時メッセージ（何が起きたかを文章で出す）。
///
/// 効果はフェーダーごとに違う（`dsp::FeatureMode` の表のとおり）ので、
/// 押した直後にここで言葉にして出す。
fn mode_message(i: usize, m: dsp::FeatureMode) -> String {
    let name = FADERS[i].name;
    let body = match (i, m) {
        (_, dsp::FeatureMode::Active) => "有効（つまみの値）",
        (0, dsp::FeatureMode::Mute) => "ミュート — 暗騒音を残しません (0 %)",
        (0, dsp::FeatureMode::Bypass) => "バイパス — 暗騒音をそのまま (100 %)",
        (1, dsp::FeatureMode::Mute) => "ミュート — presence を最小 (-12 dB)",
        (1, dsp::FeatureMode::Bypass) => "バイパス — presence を掛けません (0 dB)",
        (2, dsp::FeatureMode::Mute) => "ミュート — 残響を残しません (0 %)",
        _ => "バイパス — 残響をそのまま (100 %)",
    };
    format!("{name}: {body}")
}

#[derive(Clone, Copy, Debug, Default)]
struct Layout {
    /// タイトル（左）とメモリー（右）を載せる帯。
    title: IR,
    /// メモリーの見出し / スロット 8 個 / SAVE。
    memory_label: IR,
    slot: [IR; SLOTS],
    save_btn: IR,
    /// フッタ（ステータス + 有効/無効）。
    status: IR,
    button: IR,

    /// セクション見出し（IN / OUT / CONTROL / EFFECT）を描く帯。
    section: [IR; 3],
    /// VU の盤面（上 = IN = 加工前 / 下 = OUT = 加工後）と、その右の数値窓。
    vu: [IR; 2],
    vu_num: [IR; 2],
    /// フェーダーの当たり判定（トラックの柱）。MUTE / BYP はこの外にある。
    fader: [IR; 3],
    /// フェーダーのトラック（つまみが動く範囲）。
    fader_track: [IR; 3],
    label: [IR; 3],
    value_db: [IR; 3],
    value_pc: [IR; 3],
    /// MUTE / BYP キー（フェーダーの上に 1 段）。
    mute: [IR; 3],
    byp: [IR; 3],
    /// GR バー本体 / 目盛り / 数値欄。
    gr: IR,
    gr_scale: IR,
    gr_num: IR,
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
    silk: HFONT,
    silk_small: HFONT,
    digital: HFONT,
    digital_small: HFONT,
    vu: HFONT,
    vu_big: HFONT,
    vu_tiny: HFONT,
    button: HFONT,
    status: HFONT,
}

impl Fonts {
    unsafe fn new(scale: f32) -> Self {
        Self {
            scale,
            title: make_font("Segoe UI", 17.0 * scale, 600),
            subtitle: make_font("Segoe UI", 9.0 * scale, 400),
            silk: make_font("Tahoma", 9.5 * scale, 700),
            silk_small: make_font("Tahoma", 8.0 * scale, 400),
            digital: make_font("Consolas", 13.0 * scale, 700),
            digital_small: make_font("Consolas", 10.0 * scale, 700),
            vu: make_font("Arial Narrow", 10.5 * scale, 700),
            vu_big: make_font("Arial Narrow", 15.0 * scale, 700),
            vu_tiny: make_font("Arial Narrow", 7.0 * scale, 700),
            button: make_font("Tahoma", 10.0 * scale, 700),
            status: make_font("Segoe UI", 10.0 * scale, 400),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [
            self.title,
            self.subtitle,
            self.silk,
            self.silk_small,
            self.digital,
            self.digital_small,
            self.vu,
            self.vu_big,
            self.vu_tiny,
            self.button,
            self.status,
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

    /// 上 `top` → 下 `bottom` の縦グラデーション。
    fn fill_vgrad(&mut self, r: IR, top: u32, bottom: u32) {
        let span = (r.h() - 1).max(1) as f32;
        for y in r.t.max(0)..r.b.min(self.h) {
            let t = (y - r.t) as f32 / span;
            let c = lerp_rgb(top, bottom, t);
            for x in r.l.max(0)..r.r.min(self.w) {
                self.put(x, y, c);
            }
        }
    }

    /// 角丸の塗り。
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

    /// 縦グラデの円（LED / ネジ）。
    fn fill_ellipse_vgrad(&mut self, cx: i32, cy: i32, r: i32, top: u32, bottom: u32) {
        let r2 = (r * r) as f32;
        let span = (2 * r).max(1) as f32;
        for y in (cy - r).max(0)..(cy + r + 1).min(self.h) {
            let t = (y - (cy - r)) as f32 / span;
            let c = lerp_rgb(top, bottom, t);
            for x in (cx - r).max(0)..(cx + r + 1).min(self.w) {
                let dx = (x - cx) as f32;
                let dy = (y - cy) as f32;
                if dx * dx + dy * dy <= r2 {
                    self.put(x, y, c);
                }
            }
        }
    }

    /// 内側の影（枠から内側へ `depth` px ぶん暗くする）。彫り込み / ガラスの奥行き。
    fn inner_shadow(&mut self, r: IR, rad: i32, color: u32, depth: i32, strength: f32) {
        if depth <= 0 {
            return;
        }
        let d = depth as f32;
        for y in r.t.max(0)..r.b.min(self.h) {
            for x in r.l.max(0)..r.r.min(self.w) {
                if !inside_round_rect(x, y, r, rad) {
                    continue;
                }
                let edge = (x - r.l).min(r.r - 1 - x).min(y - r.t).min(r.b - 1 - y) as f32;
                if edge >= d {
                    continue;
                }
                let t = 1.0 - (edge + 0.5) / d;
                let bg = self.get(x, y);
                self.put(x, y, lerp_rgb(bg, color, strength * t * t));
            }
        }
    }

    /// 上端から `depth` px ぶん明るくする（金属の照り）。
    fn top_sheen(&mut self, r: IR, color: u32, depth: i32, strength: f32) {
        if depth <= 0 {
            return;
        }
        let d = depth as f32;
        for y in r.t.max(0)..(r.t + depth).min(self.h) {
            let t = 1.0 - (y - r.t) as f32 / d;
            for x in r.l.max(0)..r.r.min(self.w) {
                let bg = self.get(x, y);
                self.put(x, y, lerp_rgb(bg, color, strength * t));
            }
        }
    }

    /// 光源のにじみ（中心から遠ざかるほど弱く混ぜる）。VU 盤面のバックライト用。
    fn glow(&mut self, mask: IR, rad: i32, cx: i32, cy: i32, radius: i32, color: u32, strength: f32) {
        if radius <= 0 || strength <= 0.0 {
            return;
        }
        let r2 = (radius * radius) as f32;
        for y in mask.t.max(0)..mask.b.min(self.h) {
            for x in mask.l.max(0)..mask.r.min(self.w) {
                if !inside_round_rect(x, y, mask, rad) {
                    continue;
                }
                let dx = (x - cx) as f32;
                let dy = (y - cy) as f32;
                let d2 = dx * dx + dy * dy;
                if d2 >= r2 {
                    continue;
                }
                // sqrt を使わず 2 乗のまま減衰させる。
                let t = 1.0 - d2 / r2;
                let bg = self.get(x, y);
                self.put(x, y, lerp_rgb(bg, color, strength * t * t));
            }
        }
    }

    /// ガラスの反射。`x0` = 上端での帯の左位置、`slope` = 1 px 下がるごとの右への
    /// ずれ、`width` = 帯の幅。
    fn glass_streak(
        &mut self,
        mask: IR,
        rad: i32,
        x0: f32,
        slope: f32,
        width: i32,
        color: u32,
        strength: f32,
    ) {
        if width <= 0 || strength <= 0.0 {
            return;
        }
        let w = width as f32;
        for y in mask.t.max(0)..mask.b.min(self.h) {
            let sx = x0 + (y - mask.t) as f32 * slope;
            for x in mask.l.max(0)..mask.r.min(self.w) {
                if !inside_round_rect(x, y, mask, rad) {
                    continue;
                }
                let t = 1.0 - ((x as f32 - sx) / w).abs();
                if t <= 0.0 {
                    continue;
                }
                let bg = self.get(x, y);
                self.put(x, y, lerp_rgb(bg, color, strength * t));
            }
        }
    }

    /// ヘアライン（金属の目）。`step` px ごとに 1 px だけ明暗を混ぜる。
    fn hairlines(&mut self, r: IR, step: i32, color: u32, strength: f32) {
        if step < 2 || strength <= 0.0 {
            return;
        }
        let mut x = r.l;
        let mut k = 0;
        while x < r.r {
            // 3 本に 1 本だけ引いて、細かく見せる。
            if k % 3 == 0 && x >= 0 && x < self.w {
                for y in r.t.max(0)..r.b.min(self.h) {
                    let t = 0.35 + 0.65 * hash2(x, y);
                    let bg = self.get(x, y);
                    self.put(x, y, lerp_rgb(bg, color, strength * t));
                }
            }
            x += step;
            k += 1;
        }
    }

    /// 面取り: 上左に光、下右に影を 1 px 入れる。
    fn bevel(&mut self, r: IR, hi: u32, lo: u32) {
        self.fill_rect(IR { t: r.t, b: r.t + 1, ..r }, hi);
        self.fill_rect(IR { l: r.l, r: r.l + 1, ..r }, hi);
        self.fill_rect(IR { t: r.b - 1, b: r.b, ..r }, lo);
        self.fill_rect(IR { l: r.r - 1, r: r.r, ..r }, lo);
    }

    /// アルマイトのざらつき（決定的ノイズ）。
    fn grain(&mut self, r: IR, amount: f32) {
        for y in r.t.max(0)..r.b.min(self.h) {
            for x in r.l.max(0)..r.r.min(self.w) {
                let n = (hash2(x, y) * amount).round() as i32;
                if n != 0 {
                    let c = self.get(x, y);
                    self.put(x, y, grain(c, n));
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
    /// メーターの読み（針のリニア値・dBFS・ピーク・GR）。
    reading: Reading,
    /// 0 VU の dBFS（正の数）。
    vref: f32,
    /// 出力 3 バンドの表示値 0..1 とピーク。
    bands: [f32; 3],
    bands_peak: [f32; 3],
    values: [f32; 3],
    /// フェーダーの表示文字列（上段 / 下段）。
    fader_text: [(String, String); 3],
    /// フェーダーごとのミュート / バイパス。
    modes: [dsp::FeatureMode; 3],
    status: u32,
    status_text: String,
    summary: String,
    /// 一時的なメッセージ（メモリー操作など）。
    flash: Option<String>,
    dereverb: bool,
    /// エンジンのチャンネル数（1 = モノラル / 2 = ステレオ / 0 = 未構築）。
    channels: usize,
    /// エンジンが想定している OBS のサンプルレート（0 = 未構築）。
    rate: u32,
    enabled: bool,
    model: ModelChoice,
    /// 埋まっているスロット。
    slots: [bool; SLOTS],
    /// 呼び出したスロット（LED を点ける）。
    current_slot: Option<usize>,
    save_armed: bool,
    hover_slot: Option<usize>,
    hover_save: bool,
    hover_button: bool,
    press_button: bool,
    hover_fader: Option<usize>,
    /// マウスが乗っているミュート / バイパスキー。
    hover_mute: Option<usize>,
    hover_byp: Option<usize>,
}

struct Panel {
    shared: Arc<Shared>,
    source: *mut std::ffi::c_void,
    hwnd: HWND,
    scale: f32,
    dib: Option<Dib>,
    fonts: Option<Fonts>,
    layout: Layout,

    /// メーターの弾道（`crate::meter`）。
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
    /// 一時メッセージと、その表示を始めた時刻。
    flash: Option<(String, Instant)>,

    drag: Option<Drag>,
    /// ドラッグ中に触ったフェーダー（離したときに OBS 設定へ書き戻す）。
    pending_push: Option<usize>,
    hover_button: bool,
    press_button: bool,
    hover_fader: Option<usize>,
    /// マウスが乗っているミュート / バイパスキー。
    hover_mute: Option<usize>,
    hover_byp: Option<usize>,

    /// パネルを作った時刻（検証用の画像ダンプの待ち時間に使う）。
    opened_at: Instant,
    /// 検証用の画像を書き終えたか（1 回だけ書く）。
    dumped: bool,
}

impl Panel {
    fn new(shared: Arc<Shared>, source: *mut std::ffi::c_void, scale: f32) -> Self {
        let slots = crate::preset::load();
        Self {
            shared,
            source,
            hwnd: HWND(std::ptr::null_mut()),
            scale,
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
            opened_at: Instant::now(),
            dumped: false,
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

    fn invalidate(&self) {
        if !self.hwnd.0.is_null() {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
        }
    }

    fn flash_msg(&mut self, s: impl Into<String>) {
        self.flash = Some((s.into(), Instant::now()));
        self.invalidate();
    }

    // ----------------------------------------------------------------------
    // ウィンドウイベント
    // ----------------------------------------------------------------------

    unsafe fn on_create(&mut self) {
        let _ = SetTimer(Some(self.hwnd), 1, TIMER_MS, None);

        // クライアント領域を論理 CLIENT_W x CLIENT_H ちょうどにする。
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
        // ---- メーター（針 / ピーク / GR） ----
        // 前回のティックから新しい音声ブロックが来ていなければ「無音」として
        // 扱う。こうすると親ソースが止まったときも針が静止位置へ戻る。
        let seq = self.shared.meters.seq();
        let live = seq != self.last_seq;
        self.last_seq = seq;

        // モデルが変わったらエンジンは作り直されている（数十 ms 無音）。
        // 前のモデルの値を針に残さないよう、弾道をいったん捨てる。
        let model = self.shared.params.model();
        if model != self.last_model {
            self.last_model = model;
            self.meter.reset();
            self.bands_display = [0.0; 3];
            self.bands_peak = [0.0; 3];
        }

        let channels = self.shared.channels.load(Ordering::Relaxed);
        let m = &self.shared.meters;
        self.reading = self.meter.tick(
            live,
            [m.in_level(0), m.in_level(1)],
            [m.in_peak(0), m.in_peak(1)],
            [m.out_level(0), m.out_level(1)],
            [m.out_peak(0), m.out_peak(1)],
            channels,
        );

        // ---- 出力 3 バンド ----
        for b in 0..3 {
            let target = if live {
                meter::lin_to_norm(m.get(0, b).max(m.get(1, b)))
            } else {
                0.0
            };
            // 立ち上がりは即時、戻りは滑らかに（JUCE 版と同じ）。
            let cur = self.bands_display[b];
            self.bands_display[b] = if target > cur { target } else { cur + (target - cur) * 0.25 };
            self.bands_peak[b] = (self.bands_peak[b] - 0.02).max(self.bands_display[b]);
        }

        // ---- 一時メッセージの寿命 ----
        if let Some((_, t)) = &self.flash {
            if t.elapsed().as_millis() as u64 > FLASH_MS {
                self.flash = None;
            }
        }

        self.invalidate();
    }

    fn on_mouse_down(&mut self, x: i32, y: i32, keys: u32) {
        // ---- メモリー（押した瞬間に効かせる） ----
        if hit(&self.layout.save_btn, x, y) {
            self.toggle_save_mode();
            return;
        }
        for i in 0..SLOTS {
            if hit(&self.layout.slot[i], x, y) {
                // Ctrl を押しながらでスロットを消す。
                let ctrl = keys & 0x0008 != 0;
                self.memory_click(i, ctrl);
                return;
            }
        }

        // ---- 有効 / 無効 ----
        if hit(&self.layout.button, x, y) {
            self.press_button = true;
            unsafe {
                SetCapture(self.hwnd);
            }
            return;
        }

        // ---- フェーダーのミュート / バイパス（つまみの位置は保つ） ----
        for i in 0..3 {
            if !self.fader_enabled(i) {
                continue;
            }
            if hit(&self.layout.mute[i], x, y) {
                self.toggle_mode(i, dsp::FeatureMode::Mute);
                return;
            }
            if hit(&self.layout.byp[i], x, y) {
                self.toggle_mode(i, dsp::FeatureMode::Bypass);
                return;
            }
        }

        // ---- フェーダー ----
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
                // ダブルクリックで既定値へ（ミキサーのフェーダーと同じ）。
                self.apply_fader(i, FADERS[i].default, true);
                return;
            }
        }

        // メーター（IN / OUT）のダブルクリックでピークホールドを消す。
        for ch in 0..2 {
            if hit(&self.layout.vu[ch], x, y) || hit(&self.layout.vu_num[ch], x, y) {
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
            if self.shared.alive.load(Ordering::SeqCst) && !self.source.is_null() {
                unsafe {
                    let en = crate::ffi::obs_source_enabled(self.source as *mut _);
                    crate::ffi::obs_source_set_enabled(self.source as *mut _, !en);
                }
            }
        }

        self.drag = None;

        // ドラッグで動かした値はここで OBS の設定にも書き戻す（永続化）。
        if self.pending_push.take().is_some() {
            self.push_settings();
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
        let hm = (0..3).find(|i| hit(&self.layout.mute[*i], x, y) && self.fader_enabled(*i));
        let hy = (0..3).find(|i| hit(&self.layout.byp[*i], x, y) && self.fader_enabled(*i));
        let hs = (0..SLOTS).find(|i| hit(&self.layout.slot[*i], x, y));
        let hv = hit(&self.layout.save_btn, x, y);
        if hb != self.hover_button
            || hf != self.hover_fader
            || hm != self.hover_mute
            || hy != self.hover_byp
            || hs != self.hover_slot
            || hv != self.hover_save
        {
            self.hover_button = hb;
            self.hover_fader = hf;
            self.hover_mute = hm;
            self.hover_byp = hy;
            self.hover_slot = hs;
            self.hover_save = hv;
            self.invalidate();
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

    /// フェーダーごとのミュート / バイパス。
    fn fader_mode(&self, i: usize) -> dsp::FeatureMode {
        match i {
            0 => self.shared.params.ambience_mode(),
            1 => self.shared.params.voice_mode(),
            _ => self.shared.params.voice_reverb_mode(),
        }
    }

    fn set_fader_mode(&self, i: usize, m: dsp::FeatureMode) {
        match i {
            0 => self.shared.params.set_ambience_mode(m),
            1 => self.shared.params.set_voice_mode(m),
            _ => self.shared.params.set_voice_reverb_mode(m),
        }
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
        self.set_fader_mode(i, next);
        // 手で触ったら「呼び出したスロット」の表示は消す。
        self.current_slot = None;
        // OBS の設定にも書き戻す（シーンコレクションに残す）。
        self.push_settings();
        self.flash_msg(mode_message(i, next));
    }

    fn apply_fader(&mut self, i: usize, v: f32, push: bool) {
        match i {
            0 => self.shared.params.set_ambience(v),
            1 => self.shared.params.set_voice(v),
            _ => self.shared.params.set_voice_reverb(v),
        }
        // 手で触ったら「呼び出したスロット」の表示は消す。
        self.current_slot = None;
        if push {
            self.push_settings();
        } else {
            self.pending_push = Some(i);
        }
        self.invalidate();
    }

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
            let model = std::ffi::CString::new(self.shared.params.model().as_str()).unwrap_or_default();
            crate::ffi::obs_data_set_string(d, dsp::MODEL.as_ptr(), model.as_ptr());
            let vref = std::ffi::CString::new(self.shared.params.vu_ref().as_str()).unwrap_or_default();
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

    // ----------------------------------------------------------------------
    // メモリー（プリセット）
    // ----------------------------------------------------------------------

    /// 今の設定をプリセットにする。
    fn current_preset(&self) -> Preset {
        Preset {
            ambience: self.shared.params.ambient(),
            voice_db: self.shared.params.voice(),
            reverb: self.shared.params.voice_reverb(),
            ambience_mode: self.shared.params.ambience_mode(),
            voice_mode: self.shared.params.voice_mode(),
            reverb_mode: self.shared.params.voice_reverb_mode(),
            model: self.shared.params.model().as_str().to_string(),
        }
    }

    /// プリセットを適用して OBS の設定へも書き戻す（モデルはエンジンを
    /// 作り直すので、数十 ms 音が途切れる）。
    fn apply_preset(&mut self, p: &Preset) {
        self.shared.params.set_ambience(p.ambience);
        self.shared.params.set_voice(p.voice_db);
        self.shared.params.set_voice_reverb(p.reverb);
        self.shared.params.set_ambience_mode(p.ambience_mode);
        self.shared.params.set_voice_mode(p.voice_mode);
        self.shared.params.set_voice_reverb_mode(p.reverb_mode);
        self.shared.params.set_model(p.model_choice());
        self.push_settings();
        self.invalidate();
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
        if let Err(e) = crate::preset::store(&self.slots) {
            crate::obs_log!(crate::ffi::LOG_WARNING, "プリセットを保存できません: {}", e);
            self.flash_msg(format!("保存に失敗しました: {e}"));
        }
    }

    // ----------------------------------------------------------------------
    // レイアウト / 描画
    // ----------------------------------------------------------------------

    fn compute_layout(&self, w: i32, h: i32) -> Layout {
        let sc = |v: f32| (v * self.scale).round() as i32;
        let mut out = Layout::default();
        let m = sc(K_MARGIN);
        let gap = sc(K_GAP);
        let mut area = IR { l: m, t: m, r: w - m, b: h - m };

        // ---- ヘッダ（左 = タイトル / 右 = メモリー） ----
        out.title = area.top(sc(K_HEADER_H));

        let slot_w = sc(K_SLOT_W);
        let slot_h = sc(K_SLOT_H);
        let slot_gap = sc(K_SLOT_GAP);
        let save_w = sc(K_SAVE_W);
        let label_w = sc(K_MEMORY_LABEL_W);
        let row_w = label_w + SLOTS as i32 * (slot_w + slot_gap) - slot_gap + gap + save_w;
        let row_h = slot_h.max(sc(K_BUTTON_H));
        let cy = out.title.cy();
        let mut x = out.title.r - row_w;
        out.memory_label = IR { l: x, t: cy - row_h / 2, r: x + label_w, b: cy + row_h / 2 };
        x += label_w;
        for i in 0..SLOTS {
            out.slot[i] = IR { l: x, t: cy - slot_h / 2, r: x + slot_w, b: cy + slot_h / 2 };
            x += slot_w + slot_gap;
        }
        x += gap - slot_gap;
        out.save_btn = IR { l: x, t: cy - slot_h / 2, r: x + save_w, b: cy + slot_h / 2 };

        area.t += sc(K_HEADER_H) + gap;

        // ---- フッタ ----
        let mut footer = area.bottom(sc(K_FOOTER_H));
        let bw = sc(K_BUTTON_W);
        let bh = sc(K_BUTTON_H);
        out.button = IR {
            l: footer.r - bw,
            t: footer.cy() - bh / 2,
            r: footer.r,
            b: footer.cy() + bh / 2,
        };
        footer.r -= bw + gap;
        out.status = footer;
        area.b -= sc(K_FOOTER_H) + gap;

        // ---- 横を 3 セクションに分ける（INPUT / CONTROL / EFFECT） ----
        let sgap = sc(K_SECTION_GAP);
        let input = IR { r: area.l + sc(K_INPUT_W), ..area };
        let control = IR {
            l: input.r + sgap,
            r: input.r + sgap + sc(K_CONTROL_W),
            ..area
        };
        let effect = IR { l: control.r + sgap, ..area };

        let title_h = sc(K_SECTION_TITLE_H);
        out.section[0] = input.top(title_h);
        out.section[1] = control.top(title_h);
        out.section[2] = effect.top(title_h);

        // ---- IN / OUT: VU 2 本（IN が上 / OUT が下）+ 数値窓 ----
        let vu_area = IR { t: input.t + title_h + sc(6.0), ..input };
        let vg = sc(K_VU_GAP);
        let row_h = (vu_area.h() - vg) / 2;
        let num_w = sc(K_VU_NUM_W);
        for ch in 0..2usize {
            let t = vu_area.t + ch as i32 * (row_h + vg);
            let row = IR { t, b: t + row_h, ..vu_area };
            out.vu[ch] = row.left(row.w() - num_w - sc(6.0));
            out.vu_num[ch] = row.right(num_w);
        }

        // ---- CONTROL: フェーダー 3 本（+ ミュート / バイパスキー） ----
        let ctrl_area = IR { t: control.t + title_h + sc(4.0), ..control };
        let cw = ctrl_area.w() / 3;
        for i in 0..3usize {
            let cell = IR {
                l: ctrl_area.l + cw * i as i32,
                t: ctrl_area.t,
                r: ctrl_area.l + cw * (i as i32 + 1),
                b: ctrl_area.b,
            };
            out.label[i] = cell.top(sc(K_FADER_LABEL_H));
            out.value_db[i] = IR {
                t: out.label[i].b,
                b: out.label[i].b + sc(K_FADER_VALUE_H),
                ..cell
            };
            out.value_pc[i] = IR {
                t: out.value_db[i].b,
                b: out.value_db[i].b + sc(K_FADER_VALUE_H),
                ..cell
            };
            // MUTE / BYP のキー（値表示の下、トラックの上に 1 段）。
            let by = out.value_pc[i].b + sc(2.0);
            let bh = sc(K_FADER_BTN_H);
            let bg = sc(2.0);
            let key_w = ((cell.w() - sc(8.0) - bg) / 2).max(sc(10.0));
            out.mute[i] = IR {
                l: cell.l + sc(4.0),
                t: by,
                r: cell.l + sc(4.0) + key_w,
                b: by + bh,
            };
            out.byp[i] = IR {
                l: out.mute[i].r + bg,
                t: by,
                r: cell.r - sc(4.0),
                b: by + bh,
            };
            // トラック = つまみが動ける範囲（つまみの張り出しを含む）。
            // つまみの中心が動けるのは [t + hh/2, b - hh/2]。
            out.fader_track[i] = IR {
                t: out.byp[i].b + sc(5.0),
                b: cell.b - sc(2.0),
                ..cell
            };
            // 当たり判定はトラックの柱ぜんぶ（ミキサーのストリップ風）。
            // キーの高さは含めない（キーを押しただけでフェーダーが飛ばない）。
            out.fader[i] = IR {
                t: out.fader_track[i].t,
                ..cell
            };
        }

        // ---- EFFECT: GR（縦・0 から下へ）+ 3 バンド ----
        let fx = IR { t: effect.t + title_h, ..effect };
        let gr_w = sc(K_GR_BAR_W);
        let gr_scale_w = sc(K_GR_SCALE_W);
        let gr_num_w = sc(K_GR_NUM_W);
        let band_w = sc(K_BAND_W);
        let band_gap = sc(K_BAND_GAP);
        let bands_w = band_w * 3 + band_gap * 2;
        let total = gr_w + gr_scale_w + gr_num_w + gap + bands_w;
        let x0 = fx.l + ((fx.w() - total) / 2).max(0);

        let top = fx.t + sc(14.0);
        let band_label_h = sc(K_BAND_LABEL_H);
        let bottom = fx.b - band_label_h;

        out.gr = IR { l: x0, t: top, r: x0 + gr_w, b: bottom };
        out.gr_scale = IR {
            l: out.gr.r,
            r: out.gr.r + gr_scale_w,
            ..out.gr
        };
        out.gr_num = IR {
            l: out.gr_scale.r,
            r: out.gr_scale.r + gr_num_w,
            ..out.gr
        };
        let band_l = out.gr_num.r + gap;
        out.bands = IR {
            l: band_l,
            t: top,
            r: band_l + bands_w,
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
        let mut slots = [false; SLOTS];
        for i in 0..SLOTS {
            slots[i] = self.slots[i].is_some();
        }
        Snapshot {
            scale: self.scale,
            layout: self.layout,
            reading: self.reading,
            vref: self.shared.params.vu_ref().dbfs(),
            bands: self.bands_display,
            bands_peak: self.bands_peak,
            values: [self.fader_value(0), self.fader_value(1), self.fader_value(2)],
            fader_text: [
                fader_text(0, self.fader_value(0)),
                fader_text(1, self.fader_value(1)),
                fader_text(2, self.fader_value(2)),
            ],
            modes: [
                self.fader_mode(0),
                self.fader_mode(1),
                self.fader_mode(2),
            ],
            status: self.shared.status(),
            status_text: self.shared.status_detail(),
            summary: self.shared.summary(),
            flash: self.flash.as_ref().map(|(s, _)| s.clone()),
            dereverb: self.shared.dereverb.load(Ordering::Relaxed),
            channels: self.shared.channels.load(Ordering::Relaxed),
            rate: self.shared.rate.load(Ordering::Relaxed),
            enabled: if self.shared.alive.load(Ordering::SeqCst) && !self.source.is_null() {
                unsafe { crate::ffi::obs_source_enabled(self.source as *mut _) }
            } else {
                true
            },
            model: self.shared.params.model(),
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

        self.dump_if_requested();
    }

    /// 検証用: 実機のパネルを画像（BMP）として残す。
    ///
    /// スクリーンショットと違い、他のウィンドウに隠されていても取れる。
    /// `DPDFNET_PANEL_SNAPSHOT` にファイルパスを入れたときだけ動くので、
    /// 通常運用では環境変数の読み取りすらしない（`dumped` で 1 回だけ）。
    fn dump_if_requested(&mut self) {
        if self.dumped || self.opened_at.elapsed() < DUMP_AFTER {
            return;
        }
        let Some(path) = std::env::var_os("DPDFNET_PANEL_SNAPSHOT") else {
            return;
        };
        self.dumped = true;
        let Some(dib) = self.dib.as_ref() else { return };
        match write_bmp(dib, std::path::Path::new(&path)) {
            Ok(()) => crate::obs_log!(
                crate::ffi::LOG_INFO,
                "パネル画像を保存しました: {}",
                path.to_string_lossy()
            ),
            Err(e) => crate::obs_log!(crate::ffi::LOG_WARNING, "パネル画像を書けません: {e}"),
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
        let scale = s.scale;
        let sc = |v: f32| (v * scale).round() as i32;
        let lay = s.layout;

        unsafe {
            SetBkMode(dc, TRANSPARENT);

            // ---- 面板（黒アルマイト） ----
            paint_panel(dib, w, h, scale);

            // ---- 四隅のネジ ----
            let sr = sc(K_SCREW_R);
            let si = sc(K_SCREW_INSET);
            for (cx, cy) in [
                (si, si),
                (w - si, si),
                (si, h - si),
                (w - si, h - si),
            ] {
                paint_screw(dib, cx, cy, sr);
            }

            // ---- ヘッダ ----
            draw_engraved(
                dc,
                "OBS NOISE SUPPRESSOR",
                lay.title.top(sc(20.0)),
                fonts.title,
                SILK,
                scale,
                DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
            );
            draw_text(
                dc,
                &format!(
                    "NEURAL DE-NOISE  ·  DE-REVERB  ·  {}",
                    if s.rate > 0 {
                        crate::filter::rate_label(s.rate)
                    } else {
                        "48 kHz".to_string()
                    }
                ),
                IR { t: lay.title.t + sc(19.0), b: lay.title.t + sc(35.0), ..lay.title },
                fonts.subtitle,
                SILK_FAINT,
                DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
            );

            // ヘッダ下の溝（彫り込み）。
            let gy = lay.title.b + sc(K_GAP) / 2;
            dib.fill_rect(IR { l: lay.title.l, t: gy, r: lay.title.r, b: gy + 1 }, BEVEL_LO);
            dib.fill_rect(IR { l: lay.title.l, t: gy + 1, r: lay.title.r, b: gy + 2 }, lerp_rgb(PANEL_BOT, BEVEL_HI, 0.35));

            // ---- メモリー ----
            paint_memory(dib, dc, fonts, &s, scale);

            // ---- セクション見出し ----
            let sec_names = ["IN / OUT", "CONTROL", "EFFECT"];
            for i in 0..3usize {
                let r = lay.section[i];
                draw_engraved(
                    dc,
                    sec_names[i],
                    r,
                    fonts.silk,
                    SILK_DIM,
                    scale,
                    DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
                );
                let y = r.b - 1;
                dib.fill_rect(IR { l: r.l, t: y, r: r.r, b: y + 1 }, BEVEL_LO);
                dib.fill_rect(IR { l: r.l, t: y + 1, r: r.r, b: y + 2 }, lerp_rgb(PANEL_BOT, BEVEL_HI, 0.35));
            }

            // セクションの区切り（彫り込みの縦溝）。実機のモジュール境界。
            let div_top = lay.title.b + sc(K_GAP) / 2;
            let div_bot = lay.status.t - sc(K_GAP);
            for i in 0..2usize {
                let x = (lay.section[i].r + lay.section[i + 1].l) / 2;
                dib.fill_rect(IR { l: x, t: div_top, r: x + 1, b: div_bot }, BEVEL_LO);
                dib.fill_rect(
                    IR { l: x + 1, t: div_top, r: x + 2, b: div_bot },
                    lerp_rgb(PANEL_BOT, BEVEL_HI, 0.35),
                );
            }
            // フッタの区切り（横溝）。
            let fy = lay.status.t - sc(K_GAP) / 2;
            dib.fill_rect(IR { l: lay.title.l, t: fy, r: lay.title.r, b: fy + 1 }, BEVEL_LO);
            dib.fill_rect(
                IR { l: lay.title.l, t: fy + 1, r: lay.title.r, b: fy + 2 },
                lerp_rgb(PANEL_BOT, BEVEL_HI, 0.35),
            );

            // 入力の基準値をセクション見出しの右端に出す。
            draw_text(
                dc,
                &format!("0 VU = -{:.0} dBFS", s.vref),
                lay.section[0],
                fonts.silk_small,
                SILK_FAINT,
                DT_RIGHT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
            );

            // ---- VU 2 本（アナログ針・上 = IN = 加工前 / 下 = OUT = 加工後） ----
            let on = s.channels > 0;
            let r = s.reading;
            // (針のリニア値, 現在値 dBFS, ピーク dBFS, 盤面の名前)
            let meters = [
                (r.vu_in_lin, r.vu_in_db, r.peak_in_db, "IN"),
                (r.vu_out_lin, r.vu_out_db, r.peak_out_db, "OUT"),
            ];
            for (m, (lin, db, peak, name)) in meters.iter().enumerate() {
                let pos = vu_to_pos(*lin, s.vref);
                paint_vu(dib, dc, fonts, scale, lay.vu[m], pos, name, on);
                paint_meter_num(dib, dc, fonts, scale, lay.vu_num[m], name, *db, *peak, on);
            }

            // ---- フェーダー 3 本（+ MUTE / BYP キー） ----
            for i in 0..3usize {
                let on = i != 2 || s.dereverb;
                let mode = s.modes[i];
                paint_fader(
                    dib,
                    dc,
                    fonts,
                    scale,
                    lay.fader_track[i],
                    lay.label[i],
                    lay.value_db[i],
                    lay.value_pc[i],
                    i,
                    s.values[i],
                    &s.fader_text[i].0,
                    &s.fader_text[i].1,
                    on,
                    // ミュート / バイパス中は値の表示を落として「効いていない」
                    // ことを見せる（つまみの位置そのものは変わらない）。
                    on && mode == dsp::FeatureMode::Active,
                    s.hover_fader == Some(i),
                );
                paint_switch(
                    dib,
                    dc,
                    fonts,
                    scale,
                    lay.mute[i],
                    "MUTE",
                    LED_RED,
                    mode == dsp::FeatureMode::Mute,
                    on,
                    s.hover_mute == Some(i),
                );
                paint_switch(
                    dib,
                    dc,
                    fonts,
                    scale,
                    lay.byp[i],
                    "BYP",
                    WARM,
                    mode == dsp::FeatureMode::Bypass,
                    on,
                    s.hover_byp == Some(i),
                );
            }

            // ---- GR（コンプの GR と同じく 0 から下へ振れる） ----
            paint_gr(dib, dc, fonts, scale, lay.gr, lay.gr_scale, lay.gr_num, s.reading.gr_db);

            // ---- 出力 3 バンド（GR の隣に細く） ----
            paint_bands(
                dib,
                dc,
                fonts,
                scale,
                lay.bands,
                lay.bands_label,
                &s.bands,
                &s.bands_peak,
            );

            // ---- ステータス ----
            paint_status(dib, dc, fonts, &s, scale);

            // ---- BYPASS ----
            paint_key(
                dib,
                dc,
                fonts,
                scale,
                lay.button,
                "BYPASS",
                if !s.enabled { LED_RED } else { 0 },
                s.hover_button,
                s.press_button,
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
// 描画（`Panel` から呼ぶフリー関数）
// --------------------------------------------------------------------------

/// 面板（黒アルマイト）の地。縦グラデ + 上端の照り + ざらつき + 外周の溝。
fn paint_panel(dib: &mut Dib, w: i32, h: i32, scale: f32) {
    let sc = |v: f32| (v * scale).round() as i32;
    let all = IR { l: 0, t: 0, r: w, b: h };
    dib.fill_vgrad(all, PANEL_TOP, PANEL_BOT);
    // ヘアライン（金属の目）。実機のアルミパネルと同じ方向で薄く入れる。
    dib.hairlines(all, 3, 0x77808c, 0.10);
    dib.top_sheen(all, 0xffffff, sc(2.0).max(1), 0.045);
    dib.grain(all, 2.0);

    // 外周は彫り込み（上左に影 / 下右に光）。
    dib.fill_rect(IR { t: 0, b: 1, ..all }, BG);
    dib.fill_rect(IR { l: 0, r: 1, ..all }, BG);
    dib.fill_rect(IR { t: h - 1, b: h, ..all }, lerp_rgb(PANEL_BOT, BEVEL_HI, 0.40));
    dib.fill_rect(IR { l: w - 1, r: w, ..all }, lerp_rgb(PANEL_BOT, BEVEL_HI, 0.40));
    // その内側に面取りの光（板が浮いて見える）。
    let hi = lerp_rgb(PANEL_TOP, BEVEL_HI, 0.30);
    dib.fill_rect(IR { t: 1, b: 2, l: 1, r: w - 1 }, hi);
    dib.fill_rect(IR { l: 1, r: 2, t: 1, b: h - 1 }, hi);
}

/// 四隅のネジ（金属のドーム + 十字の溝）。
fn paint_screw(dib: &mut Dib, cx: i32, cy: i32, r: i32) {
    let r = r.max(3);
    // 座面（座ぐりの影）
    dib.fill_ellipse(cx, cy + 1, r + 1, 0x0a0b0d);
    // ドーム
    dib.fill_ellipse_vgrad(cx, cy, r, 0xaeb4ba, 0x494f56);
    // 十字の溝（中央が太く、外周へ細くなる = フィリップス）。
    let t = (r / 2).max(1);
    for k in 0..3 {
        let w = (t - k).max(1);
        let d = k;
        dib.fill_rect(
            IR { l: cx - r + 1 + d, t: cy - w / 2, r: cx + r - 1 - d, b: cy - w / 2 + w },
            0x181a1c,
        );
        dib.fill_rect(
            IR { l: cx - w / 2, t: cy - r + 1 + d, r: cx - w / 2 + w, b: cy + r - 1 - d },
            0x181a1c,
        );
    }
    // 溝の上辺に光（面取りが光る）。
    dib.fill_rect(IR { l: cx - r + 2, t: cy - t / 2 - 1, r: cx + r - 2, b: cy - t / 2 }, 0x7d838a);
    dib.fill_rect(IR { l: cx - t / 2 - 1, t: cy - r + 2, r: cx - t / 2, b: cy + r - 2 }, 0x7d838a);
    // 頭の照り
    dib.fill_ellipse(cx - r / 3, cy - r / 3, (r / 3).max(1), 0xc9ced4);
}

/// 押しボタン（黒いキー + 彫り込みラベル + LED）。
///
/// `led` が 0 以外なら点灯（ラベルも明るくする）。`pressed` で面取りを反転。
fn paint_key(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    r: IR,
    label: &str,
    led: u32,
    hover: bool,
    pressed: bool,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(3.0);
    let on = led != 0;

    let (top, bot) = if pressed {
        (0x141619, 0x22262a)
    } else if hover {
        (0x2c3035, 0x1f2327)
    } else {
        (0x262a2e, 0x1a1d21)
    };
    dib.fill_round_rect_vgrad(r, rad, top, bot);
    dib.grain(r, 1.0);

    if pressed {
        dib.inner_shadow(r, rad, 0x000000, sc(3.0), 0.55);
    } else {
        dib.bevel(r, lerp_rgb(top, BEVEL_HI, 0.55), BEVEL_LO);
    }

    // LED（キーの上寄り中央）。
    let lr = sc(2.0).max(2);
    let (lx, ly) = (r.cx(), r.t + sc(5.0));
    if on {
        dib.fill_ellipse(lx, ly, lr + sc(1.0), lerp_rgb(bot, led, 0.35));
        dib.fill_ellipse(lx, ly, lr, led);
        dib.fill_ellipse(lx - 1, ly - 1, (lr / 2).max(1), lighten(led, 0.55));
    } else {
        dib.fill_ellipse(lx, ly, lr, 0x101214);
        dib.fill_ellipse(lx, ly + 1, lr - 1, 0x181b1e);
    }

    let text_r = IR { t: r.t + sc(6.0), ..r };
    unsafe {
        draw_engraved(
            dc,
            label,
            text_r,
            fonts.button,
            if on { VALUE } else { SILK_DIM },
            scale,
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
    }
}

/// フェーダーの MUTE / BYP キー（小さいトグル）。
///
/// 左端の細い窓が LED（点灯 = その状態が有効）、右に文字。`enabled` が false
/// のときは押せないので全体を沈める（REVERB が 1 出力モデルのとき）。
#[allow(clippy::too_many_arguments)]
unsafe fn paint_switch(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    r: IR,
    label: &str,
    led: u32,
    lit: bool,
    enabled: bool,
    hover: bool,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(2.0);

    let (top, bot) = if !enabled {
        (0x1b1e21, 0x141618)
    } else if lit {
        (0x2b3034, 0x1d2124)
    } else if hover {
        (0x30353a, 0x22262a)
    } else {
        (0x25292d, 0x1a1d20)
    };
    dib.fill_round_rect_vgrad(r, rad, top, bot);
    dib.bevel(r, lerp_rgb(top, BEVEL_HI, 0.5), BEVEL_LO);

    // LED（左端の縦長の窓）。消灯中は黒い穴のまま。
    let lamp = IR {
        l: r.l + sc(3.0),
        t: r.t + sc(3.0),
        r: r.l + sc(3.0) + sc(3.0).max(2),
        b: r.b - sc(3.0),
    };
    dib.fill_rect(lamp, 0x0b0d0f);
    if lit && enabled {
        dib.fill_rect(lamp.inset(1), led);
        dib.fill_rect(IR { t: lamp.t + 1, ..lamp.inset(1) }, lighten(led, 0.4));
    }

    draw_engraved(
        dc,
        label,
        IR { l: lamp.r, r: r.r - sc(2.0), ..r },
        fonts.silk_small,
        if !enabled {
            SILK_FAINT
        } else if lit {
            VALUE
        } else {
            SILK_DIM
        },
        scale,
        DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );
}

/// 小さいキー（メモリーのスロット）。
fn paint_slot_key(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    r: IR,
    label: &str,
    filled: bool,
    current: bool,
    hover: bool,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(2.0);
    let (top, bot) = if hover {
        (0x33383d, 0x22262a)
    } else {
        (0x24282c, 0x191c20)
    };
    dib.fill_round_rect_vgrad(r, rad, top, bot);
    dib.bevel(r, lerp_rgb(top, BEVEL_HI, 0.5), BEVEL_LO);

    // LED（上中央）。
    let lr = sc(1.8).max(2);
    let (lx, ly) = (r.cx(), r.t + sc(4.0));
    let led = if current {
        LED_GREEN
    } else if filled {
        WARM
    } else {
        0
    };
    if led != 0 {
        dib.fill_ellipse(lx, ly, lr, led);
        dib.fill_ellipse(lx - 1, ly - 1, (lr / 2).max(1), lighten(led, 0.6));
    } else {
        dib.fill_ellipse(lx, ly, lr, 0x101214);
    }

    let text_r = IR { t: r.t + sc(5.0), ..r };
    unsafe {
        draw_engraved(
            dc,
            label,
            text_r,
            fonts.digital_small,
            if filled { VALUE } else { SILK_FAINT },
            scale,
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
    }
}

/// メモリー行（見出し + スロット 8 + SAVE）。
fn paint_memory(dib: &mut Dib, dc: HDC, fonts: &Fonts, s: &Snapshot, scale: f32) {
    let lay = s.layout;
    unsafe {
        draw_text(
            dc,
            "MEMORY",
            lay.memory_label,
            fonts.silk_small,
            SILK_FAINT,
            DT_RIGHT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
    }
    for i in 0..SLOTS {
        paint_slot_key(
            dib,
            dc,
            fonts,
            scale,
            lay.slot[i],
            &(i + 1).to_string(),
            s.slots[i],
            s.current_slot == Some(i),
            s.hover_slot == Some(i),
        );
    }

    // SAVE: 待機中は赤く光らせる。空きスロットと区別するため幅を広めに取ってある。
    paint_key(
        dib,
        dc,
        fonts,
        scale,
        lay.save_btn,
        "SAVE",
        if s.save_armed { LED_RED } else { 0 },
        s.hover_save,
        false,
    );
}

/// アナログ VU 盤面（アイボリー + 針）。
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
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(3.0);

    // ---- 盤面（ランプで後ろから照らした暖色のアイボリー）----
    let lit = if on { 1.0 } else { 0.45 };
    let (top, bot) = if on {
        (VU_FACE_TOP, VU_FACE_BOT)
    } else {
        (dim(VU_FACE_TOP, 0.34), dim(VU_FACE_BOT, 0.34))
    };
    dib.fill_round_rect_vgrad(r, rad, top, bot);
    // 光源は上（少し右）にある。盤面の上側が暖かく光る。
    dib.glow(
        r,
        rad,
        r.cx() + r.w() / 6,
        r.t - r.h() / 4,
        (r.h() as f32 * 1.3).round() as i32,
        VU_HOT,
        0.55 * lit,
    );
    // ガラスの反射（右下へ抜ける斜めの帯）。
    dib.glass_streak(
        r,
        rad,
        r.l as f32 + r.w() as f32 * 0.16,
        1.25,
        sc(26.0),
        0xffffff,
        0.14 * lit,
    );
    // 枠の内側の奥行き（ガラス越しの影）と、ガラス上端の照り。
    dib.inner_shadow(r, rad, 0x2a2116, sc(6.0), 0.42);
    dib.top_sheen(r.inset(sc(2.0)), 0xffffff, sc(3.0), 0.26);

    // 支点と半径。針は盤面の下寄りを中心に振る。
    let px = r.cx() as f32;
    let py = (r.b - sc(VU_PIVOT_LIFT)) as f32;
    let arc_r = (r.h() as f32 - 20.0 * scale).max(18.0 * scale);

    let vu_ang = |vu: f32| -> f32 {
        let t = ((vu - VU_VU_AT_MIN) / (VU_VU_AT_MAX - VU_VU_AT_MIN)).clamp(0.0, 1.0);
        VU_ANGLE_MIN + t * (VU_ANGLE_MAX - VU_ANGLE_MIN)
    };
    let pt = |a: f32, rr: f32| -> (f32, f32) {
        let rad = a.to_radians();
        (px + rr * rad.sin(), py - rr * rad.cos())
    };

    let ink = if on { VU_INK } else { dim(VU_INK, 0.40) };
    let ink_dim = if on { VU_INK_DIM } else { dim(VU_INK_DIM, 0.40) };
    let red = if on { VU_RED } else { dim(VU_RED, 0.40) };

    // ---- 目盛りの弧（実機は太い黒の弧。0 VU から +3 は赤い帯）----
    stroke_arc(
        dc,
        px,
        py,
        arc_r,
        vu_ang(VU_VU_AT_MIN),
        vu_ang(VU_VU_AT_MAX),
        ink,
        (1.8 * scale).max(2.0).round() as i32,
    );
    stroke_arc(
        dc,
        px,
        py,
        arc_r + 1.0 * scale,
        vu_ang(0.0),
        vu_ang(VU_VU_AT_MAX),
        red,
        (2.2 * scale).max(2.0).round() as i32,
    );

    // ---- 目盛り（0 付近が細かい = 実機の VU と同じ）----
    const TICKS: [f32; 11] = [-20.0, -10.0, -7.0, -5.0, -3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0];
    const LABELS: [i32; 5] = [-20, -10, -5, 0, 3];
    for &v in TICKS.iter() {
        let a = vu_ang(v);
        let len = if v >= 0.0 || (v.fract() == 0.0) { 6.5 * scale } else { 4.0 * scale };
        let (x0, y0) = pt(a, arc_r - len);
        let (x1, y1) = pt(a, arc_r + 0.5 * scale);
        let c = if v >= 0.0 { red } else { ink };
        stroke_line(dc, x0.round() as i32, y0.round() as i32, x1.round() as i32, y1.round() as i32, c, (1.3 * scale).max(1.0) as i32);
    }
    for v in LABELS {
        let a = vu_ang(v as f32);
        let (x, y) = pt(a, arc_r - 16.0 * scale);
        let txt = if v > 0 { format!("+{v}") } else { v.to_string() };
        let rc = IR {
            l: (x - 12.0 * scale).round() as i32,
            t: (y - 5.0 * scale).round() as i32,
            r: (x + 12.0 * scale).round() as i32,
            b: (y + 5.0 * scale).round() as i32,
        };
        draw_text(
            dc,
            &txt,
            rc,
            fonts.vu,
            if v >= 0 { red } else { ink },
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
    }

    // ---- 2 本目の細い弧（0 VU = 100 % の校正目盛り。実機にある）----
    let cal_r = arc_r - 26.0 * scale;
    if cal_r > 12.0 * scale {
        stroke_arc(dc, px, py, cal_r, vu_ang(VU_VU_AT_MIN), vu_ang(0.0), ink_dim, (0.9 * scale).max(1.0) as i32);
        for (v, txt) in [(-10.0f32, "50"), (-1.0, "100 %")] {
            let a = vu_ang(v);
            let (x, y) = pt(a, cal_r - 7.0 * scale);
            let rc = IR {
                l: (x - 16.0 * scale).round() as i32,
                t: (y - 4.0 * scale).round() as i32,
                r: (x + 16.0 * scale).round() as i32,
                b: (y + 4.0 * scale).round() as i32,
            };
            draw_text(
                dc,
                txt,
                rc,
                fonts.vu_tiny,
                ink_dim,
                DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
            );
        }
    }

    // ---- 「VU」と「dB」の盤面印刷 ----
    draw_text(
        dc,
        "VU",
        IR {
            l: r.l + sc(8.0),
            t: r.t + sc(5.0),
            r: r.l + sc(38.0),
            b: r.t + sc(18.0),
        },
        fonts.vu,
        ink,
        DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );
    draw_text(
        dc,
        "dB",
        IR {
            l: r.cx() - sc(8.0),
            t: r.b - sc(52.0),
            r: r.cx() + sc(36.0),
            b: r.b - sc(34.0),
        },
        fonts.vu_big,
        ink,
        DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );

    // ---- 針（実機は細い黒針。付け根から先へ向けて細くする）----
    let a = VU_ANGLE_MIN + pos.clamp(0.0, 1.0) * (VU_ANGLE_MAX - VU_ANGLE_MIN);
    let (tx, ty) = pt(a, arc_r - 2.0 * scale);
    let (bx, by) = pt(a, -2.5 * scale); // 支点のすぐ向こう（カバーに隠れる）
    let needle = if on { NEEDLE } else { dim(NEEDLE, 0.40) };
    let (t0x, t0y) = pt(a, arc_r - 22.0 * scale);
    // 根元（太め）→ 先端（細い）の 2 本でテーパーにする。
    stroke_line(
        dc,
        bx.round() as i32,
        by.round() as i32,
        t0x.round() as i32,
        t0y.round() as i32,
        needle,
        (1.3 * scale).max(1.0).round() as i32,
    );
    stroke_line(
        dc,
        t0x.round() as i32,
        t0y.round() as i32,
        tx.round() as i32,
        ty.round() as i32,
        if on { NEEDLE_TIP } else { dim(NEEDLE_TIP, 0.40) },
        (1.2 * scale).max(1.0).round() as i32,
    );

    // 支点のカバー（実機の黒い丸い蓋）。
    let cap_w = sc(14.0).max(8);
    let cap_h = sc(7.0).max(5);
    let (cx, cy) = (px.round() as i32, py.round() as i32);
    dib.fill_round_rect(
        IR {
            l: cx - cap_w / 2,
            t: cy - cap_h / 2,
            r: cx + cap_w / 2,
            b: cy + cap_h / 2,
        },
        cap_h / 2,
        if on { 0x27241f } else { dim(0x27241f, 0.40) },
    );
    dib.top_sheen(
        IR {
            l: cx - cap_w / 2,
            t: cy - cap_h / 2,
            r: cx + cap_w / 2,
            b: cy,
        },
        if on { 0x8e887c } else { dim(0x8e887c, 0.40) },
        sc(1.0).max(1),
        0.55,
    );

    // ---- チャンネル名（右下）----
    draw_text(
        dc,
        name,
        IR {
            l: r.r - sc(28.0),
            t: r.b - sc(20.0),
            r: r.r - sc(8.0),
            b: r.b - sc(4.0),
        },
        fonts.silk,
        ink,
        DT_RIGHT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );

    // ---- ベゼル（黒いモールド枠 + 内側の金属縁 + ガラス端の光）----
    stroke_round_rect(dc, r, rad, 0x08090b, (2.0 * scale).max(2.0).round() as i32);
    stroke_round_rect(dc, r.inset(sc(2.0)), rad, 0x6a7076, 1);
    dib.bevel(r.inset(sc(2.0)), 0x9aa0a5, 0x14161a);
}

/// VU の数値窓（名前 + 現在値 dBFS + ピーク dBFS）。凹んだ表示窓。
fn paint_meter_num(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    r: IR,
    name: &str,
    db: f32,
    peak_db: f32,
    on: bool,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(3.0);

    dib.fill_round_rect_vgrad(r, rad, 0x0a0c0e, 0x111417);
    dib.inner_shadow(r, rad, 0x000000, sc(3.0), 0.6);
    dib.bevel(r, 0x2a2f35, 0x050607);

    let mid = r.t + r.h() / 2;
    let (c_now, c_peak) = if on {
        (VALUE, WARM_HI)
    } else {
        (dim(VALUE, 0.4), dim(WARM_HI, 0.4))
    };

    unsafe {
        // 名前（IN / OUT）。
        draw_text(
            dc,
            name,
            IR { l: r.l, t: r.t + sc(2.0), r: r.r, b: r.t + sc(13.0) },
            fonts.silk_small,
            if on { SILK_DIM } else { SILK_FAINT },
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
        draw_text(
            dc,
            &db_text(db),
            IR { l: r.l, t: r.t + sc(13.0), r: r.r, b: mid - sc(2.0) },
            fonts.digital,
            c_now,
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
        draw_text(
            dc,
            "dBFS",
            IR { t: mid - sc(4.0), b: mid + sc(7.0), ..r },
            fonts.silk_small,
            SILK_FAINT,
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
        // 区切りの溝。
        dib.fill_rect(IR { l: r.l + sc(6.0), t: mid + sc(10.0), r: r.r - sc(6.0), b: mid + sc(11.0) }, 0x050607);
        draw_text(
            dc,
            &db_text(peak_db),
            IR { l: r.l, t: mid + sc(12.0), r: r.r, b: r.b - sc(12.0) },
            fonts.digital_small,
            c_peak,
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
        draw_text(
            dc,
            "PEAK",
            IR { t: r.b - sc(13.0), b: r.b - sc(3.0), ..r },
            fonts.silk_small,
            SILK_FAINT,
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
    }
}

/// 縦フェーダー 1 本（目盛り + トラック + つまみ + dB / % の併記）。
///
/// `value_lit` は値表示を点けるか。ミュート / バイパス中はここだけ落とす
/// （つまみの位置そのものは変わらないので、柱とつまみは通常のまま）。
#[allow(clippy::too_many_arguments)]
unsafe fn paint_fader(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    track: IR,
    label_r: IR,
    db_r: IR,
    pc_r: IR,
    index: usize,
    value: f32,
    db_text_s: &str,
    pc_text_s: &str,
    on: bool,
    value_lit: bool,
    hover: bool,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let spec = &FADERS[index];

    // ---- 見出し（彫り込み）----
    draw_engraved(
        dc,
        spec.name,
        label_r,
        fonts.silk,
        if on { SILK } else { SILK_FAINT },
        scale,
        DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );

    // ---- 数値（上段 = dB / %、下段 = もう一方）----
    let t = ((value - spec.min) / (spec.max - spec.min)).clamp(0.0, 1.0);
    let (up, low) = if spec.percent_first {
        (pc_text_s, db_text_s)
    } else {
        (db_text_s, pc_text_s)
    };
    draw_text(
        dc,
        up,
        db_r,
        fonts.digital_small,
        if value_lit { VALUE } else { dim(VALUE, 0.45) },
        DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );
    draw_text(
        dc,
        low,
        pc_r,
        fonts.silk_small,
        if value_lit { SILK_DIM } else { SILK_FAINT },
        DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
    );

    // ---- トラック（掘り込んだスリット）----
    let hh = sc(K_FADER_HANDLE_H);
    let slot_w = sc(K_FADER_TRACK_W);
    let slot = IR {
        l: track.cx() - slot_w / 2,
        t: track.t,
        r: track.cx() + slot_w / 2,
        b: track.b,
    };
    dib.fill_round_rect(slot, slot_w / 2, 0x0a0c0e);
    dib.inner_shadow(slot, slot_w / 2, 0x000000, sc(2.0), 0.7);
    // 金属レール（実機のフェーダーはスリットの両側が金属）。
    let rail = sc(2.0).max(1);
    for side in [-1i32, 1] {
        let x = if side < 0 { slot.l - rail } else { slot.r };
        let rr = IR { l: x, t: track.t, r: x + rail, b: track.b };
        if on {
            dib.fill_vgrad(rr, 0x9aa0a6, 0x3d4349);
            // 内側の稜線に光を入れて、えぐれではなく浮きに見せる。
            let inner = if side < 0 { rr.r - 1 } else { rr.l };
            dib.fill_rect(IR { l: inner, t: rr.t, r: inner + 1, b: rr.b }, 0xc3c8ce);
        } else {
            dib.fill_vgrad(rr, dim(0x9aa0a6, 0.45), dim(0x3d4349, 0.45));
        }
    }

    // つまみの中心が動ける範囲。
    let y_top = track.t + hh / 2;
    let y_bot = track.b - hh / 2;
    let travel = (y_bot - y_top).max(1) as f32;
    let y_handle = y_top + ((1.0 - t) * travel).round() as i32;

    // ---- 目盛り（右側）----
    let ticks = 11;
    for k in 0..ticks {
        let tt = k as f32 / (ticks - 1) as f32;
        let y = y_top + ((1.0 - tt) * travel).round() as i32;
        let major = k % 5 == 0;
        let len = if major { sc(8.0) } else { sc(4.5) };
        let x0 = slot.r + sc(4.0);
        let c = if on { if major { SILK_DIM } else { SILK_FAINT } } else { dim(SILK_FAINT, 0.4) };
        dib.fill_rect(IR { l: x0, t: y, r: x0 + len, b: y + 1 }, c);
        dib.fill_rect(IR { l: x0, t: y + 1, r: x0 + len, b: y + 2 }, 0x0b0d0f);
    }
    // 0 dB / 中央の基準線（VOICE は中央、他は下端）。
    let y_ref = if index == 1 { track.cy() } else { y_bot };
    dib.fill_rect(
        IR { l: slot.l - sc(3.0), t: y_ref, r: slot.r + sc(3.0), b: y_ref + 1 },
        if on { WARM } else { dim(WARM, 0.35) },
    );

    // ---- 値のバー（基準線からつまみまで）----
    let (ya, yb) = if y_handle < y_ref { (y_handle, y_ref) } else { (y_ref, y_handle) };
    if yb - ya > 1 {
        let bar = IR { l: slot.l + sc(1.0), t: ya, r: slot.r - sc(1.0), b: yb };
        let (c0, c1) = if index == 1 {
            (ACCENT_HI, ACCENT)
        } else {
            (ACCENT, ACCENT_HI)
        };
        if on {
            dib.fill_vgrad(bar, if index == 1 { c0 } else { c0 }, if index == 1 { c1 } else { c1 });
        } else {
            dib.fill_vgrad(bar, dim(ACCENT, 0.35), dim(ACCENT_HI, 0.35));
        }
    }

    // ---- つまみ（金属のキャップ）----
    let cap_w = sc(K_FADER_HANDLE_W);
    let cap = IR {
        l: slot.cx() - cap_w / 2,
        t: y_handle - hh / 2,
        r: slot.cx() + cap_w / 2,
        b: y_handle + hh / 2,
    };
    // 落ち影。
    dib.fill_round_rect(IR { t: cap.t + sc(2.0), b: cap.b + sc(2.0), ..cap }, sc(2.0), 0x08090b);
    let rad = sc(2.0);
    let (ct, cb) = if hover {
        (0xb9c0c7, 0x565c63)
    } else {
        (0xa5acb3, 0x464c52)
    };
    dib.fill_round_rect_vgrad(cap, rad, if on { ct } else { dim(ct, 0.45) }, if on { cb } else { dim(cb, 0.45) });
    // 中央の溝（指をかけるところ）。
    let mid_y = cap.cy();
    dib.fill_rect(IR { l: cap.l + sc(1.0), t: mid_y - 1, r: cap.r - sc(1.0), b: mid_y + 1 }, 0x2b3036);
    dib.fill_rect(IR { l: cap.l + sc(1.0), t: mid_y + 1, r: cap.r - sc(1.0), b: mid_y + 2 }, 0xd7dce1);
    // 上下のミゾ（滑り止め）。
    for dy in [-sc(4.0), sc(4.0)] {
        dib.fill_rect(IR { l: cap.l + sc(3.0), t: mid_y + dy, r: cap.r - sc(3.0), b: mid_y + dy + 1 }, 0x353a40);
    }
    dib.bevel(cap, 0xd2d8de, 0x1b1e22);

    if hover && on {
        // つまみを触っている間は縁をアクセントで光らせる。
        stroke_round_rect(dc, cap.inset(sc(1.0)), rad, ACCENT_HI, 1);
    }
}

/// GR メーター（コンプの GR と同じく 0 から下へ振れる）。
fn paint_gr(dib: &mut Dib, dc: HDC, fonts: &Fonts, scale: f32, bar: IR, scale_r: IR, num_r: IR, gr_db: f32) {
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(2.0);

    // 見出し。
    unsafe {
        draw_engraved(
            dc,
            "GR",
            IR { t: bar.t - sc(14.0), b: bar.t - sc(2.0), ..bar },
            fonts.silk_small,
            SILK_FAINT,
            scale,
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
    }

    // 0 dB の基準線。
    dib.fill_rect(IR { l: bar.l - sc(2.0), t: bar.t - 1, r: bar.r + sc(2.0), b: bar.t + 1 }, WARM);

    // バー（掘り込み + 1 dB 刻みのセグメント LED）。
    dib.fill_round_rect(bar, rad, 0x0a0c0e);
    dib.inner_shadow(bar, rad, 0x000000, sc(2.0), 0.7);

    const SEGS: i32 = 30;
    let seg = bar.h() as f32 / SEGS as f32;
    let lit = (gr_db / GR_DB_RANGE * SEGS as f32).clamp(0.0, SEGS as f32);
    for k in 0..SEGS {
        let y0 = bar.t + (k as f32 * seg).round() as i32;
        let y1 = bar.t + ((k + 1) as f32 * seg).round() as i32 - sc(1.0);
        if y1 <= y0 {
            continue;
        }
        let t = (k as f32 + 1.0) / SEGS as f32;
        // 深い抑圧ほど暖色〜赤。
        let c = if t > 0.8 {
            lerp_rgb(WARM, RED, (t - 0.8) / 0.2)
        } else {
            lerp_rgb(ACCENT, WARM, (t / 0.8).powf(1.4))
        };
        let r = IR { t: y0, b: y1, ..bar };
        if (k as f32) < lit {
            let head = (lit - k as f32).min(1.0);
            dib.fill_rect(r, lerp_rgb(0x14171a, c, 0.35 + 0.65 * head));
        } else {
            dib.fill_rect(r, 0x15181b);
        }
    }

    // 目盛り（0 / -6 / -12 / -18 / -24 / -30）。
    unsafe {
        for k in 0..=5 {
            let db = -(k as f32 * 6.0);
            let y = bar.t + (db.abs() / GR_DB_RANGE * bar.h() as f32).round() as i32;
            let x0 = bar.r + sc(2.0);
            stroke_line(dc, x0, y, x0 + sc(3.0), y, SILK_FAINT, 1);
            draw_text(
                dc,
                &format!("{}", db as i32),
                IR { l: x0 + sc(4.0), t: y - sc(6.0), r: scale_r.r, b: y + sc(6.0) },
                fonts.silk_small,
                SILK_FAINT,
                DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
            );
        }
    }

    // 数値（現在の抑圧量）。
    unsafe {
        draw_text(
            dc,
            &if gr_db < 0.05 { "0.0".to_string() } else { format!("-{gr_db:.1}") },
            IR { l: num_r.l, t: num_r.t + num_r.h() / 3 - sc(8.0), r: num_r.r, b: num_r.t + num_r.h() / 3 + sc(8.0) },
            fonts.digital,
            if gr_db > 0.05 { WARM_HI } else { VALUE_DIM },
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
        draw_text(
            dc,
            "dB",
            IR { l: num_r.l, t: num_r.t + num_r.h() / 3 + sc(6.0), r: num_r.r, b: num_r.t + num_r.h() / 3 + sc(20.0) },
            fonts.silk_small,
            SILK_FAINT,
            DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
    }
}

/// 出力 3 バンド（低 / 中 / 高）の LED バー。
fn paint_bands(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    bars: IR,
    labels: IR,
    levels: &[f32; 3],
    peaks: &[f32; 3],
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let band_w = sc(K_BAND_W);
    let gap = sc(K_BAND_GAP);
    let rad = sc(2.0);
    const SEGS: i32 = 20; // 3 dB 刻み（-60 〜 0 dBFS）

    for b in 0..3usize {
        let x = bars.l + b as i32 * (band_w + gap);
        let r = IR { l: x, t: bars.t, r: x + band_w, b: bars.b };
        dib.fill_round_rect(r, rad, 0x0a0c0e);
        dib.inner_shadow(r, rad, 0x000000, sc(2.0), 0.7);

        let seg = bars.h() as f32 / SEGS as f32;
        let lit = (levels[b] * SEGS as f32).clamp(0.0, SEGS as f32);
        for k in 0..SEGS {
            // k = 0 が下端。
            let y1 = bars.b - (k as f32 * seg).round() as i32;
            let y0 = bars.b - ((k + 1) as f32 * seg).round() as i32 + sc(1.0);
            if y1 <= y0 {
                continue;
            }
            let db = -60.0 + 3.0 * (k as f32 + 1.0);
            let c = if db > -6.0 {
                RED
            } else if db > -18.0 {
                WARM
            } else {
                ACCENT
            };
            let seg_r = IR { t: y0, b: y1, ..r };
            if (k as f32) < lit {
                let head = (lit - k as f32).min(1.0);
                dib.fill_rect(seg_r, lerp_rgb(0x14171a, c, 0.35 + 0.65 * head));
            } else {
                dib.fill_rect(seg_r, 0x15181b);
            }
        }

        // ピークホールド（白い線）。
        let yp = bars.b - (peaks[b] * bars.h() as f32).round() as i32;
        dib.fill_rect(IR { t: yp, b: yp + sc(1.5).max(1), ..r }, 0xe9eef2);

        // ラベル。
        let names = ["L", "M", "H"];
        unsafe {
            draw_text(
                dc,
                names[b],
                IR { l: r.l, t: labels.t, r: r.r, b: labels.b },
                fonts.silk_small,
                SILK_FAINT,
                DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
            );
        }
    }
}

/// ステータス（LED + 状態テキスト + モデル概要）。
fn paint_status(dib: &mut Dib, dc: HDC, fonts: &Fonts, s: &Snapshot, scale: f32) {
    let sc = |v: f32| (v * scale).round() as i32;
    let r = s.layout.status;

    let led_r = sc(4.0).max(3);
    let cx = r.l + led_r + sc(2.0);
    let cy = r.cy();

    // 点灯色: OK は緑、それ以外（未構築 / モデル無しなど）はアンバー。
    let (base, tint) = if s.status == ST_OK {
        (LED_GREEN, 0x1d5f31)
    } else {
        (WARM, 0x6a4a15)
    };
    dib.fill_ellipse(cx, cy + 1, led_r + 1, 0x090a0c);
    dib.fill_ellipse(cx, cy, led_r, tint);
    dib.fill_ellipse_vgrad(cx, cy, led_r - 1, lighten(base, 0.45), base);
    dib.fill_ellipse(cx - 1, cy - 1, (led_r / 3).max(1), 0xf4fff6);

    let text_r = IR {
        l: cx + led_r + sc(8.0),
        r: r.r - sc(76.0),
        ..r
    };
    let (line1, c1) = match &s.flash {
        Some(f) => (f.clone(), WARM_HI),
        None => (s.status_text.clone(), SILK_DIM),
    };
    unsafe {
        draw_text(
            dc,
            &line1,
            IR { t: text_r.t, b: text_r.cy() + sc(1.0), ..text_r },
            fonts.status,
            c1,
            DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_END_ELLIPSIS.0,
        );
        draw_text(
            dc,
            &s.summary,
            IR { t: text_r.cy() + sc(1.0), b: text_r.b, ..text_r },
            fonts.silk_small,
            SILK_FAINT,
            DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_END_ELLIPSIS.0,
        );
        // 使っているモデルの刻印（フッタ右端）。
        draw_engraved(
            dc,
            &format!("MODEL {}", s.model.as_str().to_uppercase()),
            IR { l: r.r - sc(76.0), t: r.cy() - sc(8.0), r: r.r, b: r.cy() + sc(8.0) },
            fonts.silk_small,
            if s.model == ModelChoice::Auto { SILK_FAINT } else { SILK_DIM },
            scale,
            DT_RIGHT.0 | DT_VCENTER.0 | DT_SINGLELINE.0,
        );
    }
}

// --------------------------------------------------------------------------
// GDI ヘルパ
// --------------------------------------------------------------------------

unsafe fn make_font(face: &str, height: f32, weight: i32) -> HFONT {
    let name: Vec<u16> = face.encode_utf16().chain(std::iter::once(0)).collect();
    let h = -(height.round().max(1.0) as i32);
    CreateFontW(
        h,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_TT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        FONT_QUALITY(5), // CLEARTYPE_QUALITY
        // iPitchAndFamily: DEFAULT_PITCH (0) | FF_DONTCARE (0)。
        0,
        PCWSTR(name.as_ptr()),
    )
}

unsafe fn select_font(dc: HDC, font: HFONT) -> HGDIOBJ {
    SelectObject(dc, HGDIOBJ(font.0))
}

/// 彫り込み風の文字（下に 1 px の影 -> 本体）。静的ラベル用。
unsafe fn draw_engraved(
    dc: HDC,
    text: &str,
    r: IR,
    font: HFONT,
    color: u32,
    scale: f32,
    flags: u32,
) {
    let d = scale.round().max(1.0) as i32;
    draw_text(dc, text, IR { t: r.t + d, b: r.b + d, ..r }, font, ENGRAVE_HI, flags);
    draw_text(dc, text, r, font, color, flags);
}

unsafe fn draw_text(dc: HDC, text: &str, r: IR, font: HFONT, color: u32, flags: u32) {
    let old = select_font(dc, font);
    SetTextColor(dc, COLORREF(colorref(color)));
    let mut w: Vec<u16> = text.encode_utf16().collect();
    let mut rc = RECT { left: r.l, top: r.t, right: r.r, bottom: r.b };
    DrawTextW(dc, &mut w, &mut rc, DRAW_TEXT_FORMAT(flags));
    SelectObject(dc, old);
}

unsafe fn stroke_line(dc: HDC, x0: i32, y0: i32, x1: i32, y1: i32, color: u32, width: i32) {
    let pen = CreatePen(PS_SOLID, width.max(1), COLORREF(colorref(color)));
    if pen.0.is_null() {
        return;
    }
    let old = SelectObject(dc, HGDIOBJ(pen.0));
    let _ = MoveToEx(dc, x0, y0, None);
    let _ = LineTo(dc, x1, y1);
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
    let _ = RoundRect(dc, r.l, r.t, r.r, r.b, rad * 2, rad * 2);
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

/// 弧（角度は 12 時方向を 0 として時計回り、度）。
unsafe fn stroke_arc(dc: HDC, cx: f32, cy: f32, rad: f32, a0: f32, a1: f32, color: u32, width: i32) {
    let steps = (((a1 - a0).abs() / 4.0).ceil() as i32).max(2);
    let pen = CreatePen(PS_SOLID, width.max(1), COLORREF(colorref(color)));
    if pen.0.is_null() {
        return;
    }
    let old = SelectObject(dc, HGDIOBJ(pen.0));
    for i in 0..=steps {
        let a = (a0 + (a1 - a0) * i as f32 / steps as f32).to_radians();
        let x = (cx + rad * a.sin()).round() as i32;
        let y = (cy - rad * a.cos()).round() as i32;
        if i == 0 {
            let _ = MoveToEx(dc, x, y, None);
        } else {
            let _ = LineTo(dc, x, y);
        }
    }
    SelectObject(dc, old);
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

/// DIB の中身を 32 bit BMP として書き出す（テストと検証用の画像ダンプで共用）。
fn write_bmp(dib: &Dib, path: &std::path::Path) -> std::io::Result<()> {
    let (w, h) = (dib.w, dib.h);
    let pix = (w * h * 4) as u32;
    let mut out: Vec<u8> = Vec::with_capacity((54 + pix) as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + pix).to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&pix.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    // DIB は top-down、BMP は bottom-up。
    unsafe {
        for y in (0..h).rev() {
            let row = std::slice::from_raw_parts(dib.bits.add((y * w) as usize), w as usize);
            for &px in row {
                out.push((px & 0xFF) as u8);
                out.push(((px >> 8) & 0xFF) as u8);
                out.push(((px >> 16) & 0xFF) as u8);
                out.push(0xFF);
            }
        }
    }
    std::fs::write(path, out)
}

// --------------------------------------------------------------------------
// オフラインのプレビュー（テスト）
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::engine::Meters;
    use std::path::PathBuf;

    fn panel(scale: f32) -> Panel {
        let shared = Arc::new(Shared::new(Arc::new(Meters::new())));
        shared.alive.store(false, Ordering::SeqCst); // libobs には触らない
        Panel::new(shared, std::ptr::null_mut(), scale)
    }

    fn setup(scale: f32) -> Panel {
        let mut p = panel(scale);
        let w = p.sc(CLIENT_W);
        let h = p.sc(CLIENT_H);
        p.dib = Some(unsafe { Dib::new(w, h).expect("DIB") });
        p.fonts = Some(unsafe { Fonts::new(scale) });
        p.layout = p.compute_layout(w, h);
        p
    }

    fn lin(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    fn fill_demo(p: &mut Panel) {
        p.reading = Reading {
            vu_in_lin: lin(-20.5),
            vu_in_db: -20.5,
            vu_out_lin: lin(-32.5),
            vu_out_db: -32.5,
            peak_in_db: -17.8,
            peak_out_db: -29.4,
            gr_db: 11.6,
        };
        p.bands_display = [0.72, 0.45, 0.28];
        p.bands_peak = [0.78, 0.50, 0.31];
        p.shared.params.set_ambience(0.62);
        p.shared.params.set_voice(3.5);
        p.shared.params.set_voice_reverb(0.28);
        p.shared.params.set_model(ModelChoice::Quality);
        p.shared.channels.store(2, Ordering::Relaxed);
        p.shared.dereverb.store(true, Ordering::Relaxed);
        let mut slots: [Option<Preset>; SLOTS] = Default::default();
        slots[0] = Some(Preset {
            ambience: 0.62,
            voice_db: 3.5,
            reverb: 0.28,
            ambience_mode: dsp::FeatureMode::Mute,
            model: "dp8".into(),
            ..Default::default()
        });
        slots[2] = Some(Preset {
            ambience: 0.85,
            voice_db: 0.0,
            reverb: 0.0,
            model: "dp2".into(),
            ..Default::default()
        });
        slots[6] = Some(Preset {
            ambience: 0.30,
            voice_db: 6.0,
            reverb: 0.6,
            voice_mode: dsp::FeatureMode::Bypass,
            model: "auto".into(),
            ..Default::default()
        });
        p.slots = slots;
        p.current_slot = Some(2);
        p.shared.set_status_detail(
            ST_OK,
            "48 kHz / ch=2 / latency 2880 samples / dpdfnet8_48khz_hr_2out.onnx",
        );
        p.shared
            .set_summary("2 ch  ·  48 kHz  ·  遅延 2880 samples (60 ms)  ·  dereverb あり  ·  model dp8".into());
    }

    /// `DPDFNET_SNAPSHOT_DIR` があれば BMP を書き出す。
    fn render(p: &mut Panel, name: &str) {
        let Some(dir) = std::env::var_os("DPDFNET_SNAPSHOT_DIR") else {
            return;
        };
        p.paint();
        let dib = p.dib.as_ref().expect("DIB");
        let path = PathBuf::from(dir).join(format!("{name}.bmp"));
        if let Err(e) = write_bmp(dib, &path) {
            panic!("BMP を書けません {}: {e}", path.display());
        }
    }

    #[test]
    fn snapshot_panel_2ch() {
        let mut p = setup(1.0);
        fill_demo(&mut p);
        render(&mut p, "panel-2ch");
    }

    #[test]
    fn snapshot_panel_mono_save_armed() {
        let mut p = setup(1.0);
        fill_demo(&mut p);
        // モノラル + SAVE 待機 + モデル auto + 基準 -20 dBFS。
        p.shared.channels.store(1, Ordering::Relaxed);
        p.shared.params.set_model(ModelChoice::Auto);
        p.shared.params.set_vu_ref(dsp::VuRef::Db20);
        p.shared
            .set_summary("1 ch  ·  48 kHz  ·  遅延 2880 samples (60 ms)  ·  dereverb あり  ·  model auto".into());
        p.save_armed = true;
        p.hover_slot = Some(4);
        p.hover_button = true;
        render(&mut p, "panel-mono-save");
    }

    /// MUTE / BYP が灯った状態（AMBIENCE をミュート、VOICE をバイパス）。
    #[test]
    fn snapshot_panel_modes() {
        let mut p = setup(1.0);
        fill_demo(&mut p);
        p.shared.params.set_ambience_mode(dsp::FeatureMode::Mute);
        p.shared.params.set_voice_mode(dsp::FeatureMode::Bypass);
        p.hover_mute = Some(2);
        p.flash = Some(("VOICE: バイパス — presence を掛けません (0 dB)".into(), Instant::now()));
        render(&mut p, "panel-modes");
    }

    #[test]
    fn snapshot_panel_hover_drag() {
        let mut p = setup(1.0);
        fill_demo(&mut p);
        p.hover_fader = Some(1);
        p.drag = Some(Drag { fader: 1 });
        p.flash = Some(("MEMORY 3 を呼び出しました".into(), Instant::now()));
        // REVERB 無効（1 出力モデル）。
        p.shared.dereverb.store(false, Ordering::Relaxed);
        p.shared.params.set_voice_reverb(0.0);
        p.shared
            .set_summary("2 ch  ·  48 kHz  ·  遅延 2880 samples (60 ms)  ·  dereverb なし  ·  model dp8".into());
        render(&mut p, "panel-hover-flash");
    }

    /// 画面の高さを変えても破綻しないこと（レイアウトの算術チェック）。
    #[test]
    fn layout_stays_inside() {
        for (cw, chh) in [(700.0, 360.0), (780.0, 400.0), (1100.0, 620.0)] {
            let p = panel(1.0);
            let (w, h) = ((cw * p.scale) as i32, (chh * p.scale) as i32);
            let l = p.compute_layout(w, h);
            for r in [l.title, l.status, l.button, l.gr, l.bands, l.save_btn, l.memory_label]
                .into_iter()
                .chain(l.slot)
                .chain(l.section)
                .chain(l.vu)
                .chain(l.vu_num)
                .chain(l.fader)
                .chain(l.fader_track)
                .chain(l.label)
                .chain(l.value_db)
                .chain(l.value_pc)
                .chain(l.mute)
                .chain(l.byp)
                .chain([l.bands_label])
            {
                assert!(r.l >= 0 && r.t >= 0 && r.r <= w && r.b <= h, "はみ出し: {r:?} ({w}x{h})");
                assert!(r.r > r.l && r.b > r.t, "潰れた: {r:?}");
            }
        }
    }

    #[test]
    fn fader_text_matches_range() {
        // AMBIENCE 100 % = 6 dB までしか抑えない / 0 % = 48 dB。
        assert!(fader_text(0, 1.0).0.starts_with("-6.0"));
        assert!(fader_text(0, 0.0).0.starts_with("-48.0"));
        assert_eq!(fader_text(0, 0.5).1, "50 %");
        assert_eq!(fader_text(1, 0.0).0, "+0.0 dB");
        assert_eq!(fader_text(1, 12.0).1, "100 %");
        assert_eq!(fader_text(2, 1.0).0, "100 %");
        assert_eq!(fader_text(2, 0.0).1, "-∞ dB");
    }

    #[test]
    fn mode_messages_name_the_effect() {
        assert_eq!(
            mode_message(0, dsp::FeatureMode::Active),
            "AMBIENCE: 有効（つまみの値）"
        );
        // フェーダーごとに効きが違うので、押した直後の文章も違うこと。
        assert!(mode_message(0, dsp::FeatureMode::Mute).contains("暗騒音を残しません"));
        assert!(mode_message(0, dsp::FeatureMode::Bypass).contains("100 %"));
        assert!(mode_message(1, dsp::FeatureMode::Mute).contains("-12 dB"));
        assert!(mode_message(1, dsp::FeatureMode::Bypass).contains("0 dB"));
        assert!(mode_message(2, dsp::FeatureMode::Mute).contains("残響を残しません"));
        assert!(mode_message(2, dsp::FeatureMode::Bypass).contains("100 %"));
    }

    /// MUTE / BYP は排他で、同じキーをもう一度押すと有効に戻る。
    /// **つまみの値には触らない。**
    #[test]
    fn toggle_mode_is_exclusive_and_keeps_the_fader() {
        let mut p = setup(1.0);
        p.shared.params.set_ambience(0.62);

        p.toggle_mode(0, dsp::FeatureMode::Mute);
        assert_eq!(p.fader_mode(0), dsp::FeatureMode::Mute);
        assert!((p.fader_value(0) - 0.62).abs() < 1e-6, "つまみが動いた");

        // BYP を押すと MUTE は落ちる。
        p.toggle_mode(0, dsp::FeatureMode::Bypass);
        assert_eq!(p.fader_mode(0), dsp::FeatureMode::Bypass);

        // 同じキーをもう一度で有効へ。
        p.toggle_mode(0, dsp::FeatureMode::Bypass);
        assert_eq!(p.fader_mode(0), dsp::FeatureMode::Active);
        assert!((p.fader_value(0) - 0.62).abs() < 1e-6, "つまみが動いた");
    }

    /// REVERB のキーは 1 出力モデル（dereverb なし）では押せない。
    #[test]
    fn reverb_keys_are_disabled_without_dereverb() {
        let p = setup(1.0);
        assert!(!p.fader_enabled(2), "dereverb なしで REVERB が有効");
        assert!(p.fader_enabled(0) && p.fader_enabled(1));
    }

    #[test]
    fn vu_reference_shifts_the_scale() {
        // 0 VU = -18 dBFS なら -38 dBFS が左端。
        assert!(vu_to_pos(lin(-38.0), 18.0) < 0.01);
        assert!(vu_to_pos(lin(-15.0), 18.0) > 0.99);
        // 基準を -14 dBFS にすると同じ値でも針は下がる。
        let a = vu_to_pos(lin(-20.0), 18.0);
        let b = vu_to_pos(lin(-20.0), 14.0);
        assert!(b < a, "-20 dBFS の針位置: {a} -> {b}");
    }
}

// （ファイル終端）
