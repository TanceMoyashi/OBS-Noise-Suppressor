//! Win32 / GDI の生 FFI 宣言。
//!
//! **外部クレートを増やさないため**に、`windows` クレートを使わず必要な分だけを
//! ここに書く（`extern "system"`）。使うのは user32 / gdi32 / kernel32 の
//! ごく一部で、宣言はパネルが必要とするものだけに絞ってある。
//!
//! 構造体は `#[repr(C)]` で Windows SDK の定義と同じ並びにしてある。サイズは
//! テストで確かめている（ずれると `CreateWindowExW` などが壊れるため）。

#![allow(non_snake_case)]

use core::ffi::c_void;
use core::ptr;

// --------------------------------------------------------------------------
// 基本型
// --------------------------------------------------------------------------

/// ウィンドウ / GDI オブジェクトのハンドル。
///
/// `HWND` / `HDC` / `HBITMAP` … はすべて「ポインタ 1 本」なので、1 つの型で
/// 表して別名を付ける（取り違えを型で防ぐほどの価値より、宣言の見通しを優先）。
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Handle(pub *mut c_void);

impl Default for Handle {
    #[inline]
    fn default() -> Self {
        Self(ptr::null_mut())
    }
}

impl Handle {
    #[inline]
    #[allow(dead_code)]
    pub fn is_null(self) -> bool {
        self.0.is_null()
    }
}

pub type HWND = Handle;
pub type HDC = Handle;
pub type HBITMAP = Handle;
pub type HFONT = Handle;
pub type HPEN = Handle;
pub type HBRUSH = Handle;
pub type HGDIOBJ = Handle;
pub type HCURSOR = Handle;
pub type HINSTANCE = Handle;
pub type HMODULE = Handle;

/// `WNDPROC`。`Option` にすると null 許容の関数ポインタになる（`WNDCLASSW` 用）。
pub type WNDPROC = unsafe extern "system" fn(HWND, u32, usize, isize) -> isize;
/// `TIMERPROC`（使わないので null を渡す）。
pub type TIMERPROC = unsafe extern "system" fn(HWND, u32, usize, u32);

// --------------------------------------------------------------------------
// 構造体
// --------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MSG {
    pub hwnd: HWND,
    pub message: u32,
    pub wParam: usize,
    pub lParam: isize,
    pub time: u32,
    pub pt: POINT,
}

impl Default for MSG {
    fn default() -> Self {
        // SAFETY: すべて 0 が有効な初期値（ハンドル = null、数値 = 0）。
        unsafe { core::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WNDCLASSW {
    pub style: u32,
    pub lpfnWndProc: Option<WNDPROC>,
    pub cbClsExtra: i32,
    pub cbWndExtra: i32,
    pub hInstance: HINSTANCE,
    pub hIcon: Handle,
    pub hCursor: HCURSOR,
    pub hbrBackground: HBRUSH,
    pub lpszMenuName: *const u16,
    pub lpszClassName: *const u16,
}

impl Default for WNDCLASSW {
    fn default() -> Self {
        // SAFETY: すべて 0 が有効な初期値。
        unsafe { core::mem::zeroed() }
    }
}

/// `WM_CREATE` / `WM_NCCREATE` の `lParam`。先頭の `lpCreateParams` だけ使う。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CREATESTRUCTW {
    pub lpCreateParams: *mut c_void,
    pub hInstance: HINSTANCE,
    pub hMenu: Handle,
    pub hwndParent: HWND,
    pub cy: i32,
    pub cx: i32,
    pub y: i32,
    pub x: i32,
    pub style: i32,
    pub lpszName: *const u16,
    pub lpszClass: *const u16,
    pub dwExStyle: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MINMAXINFO {
    pub ptReserved: POINT,
    pub ptMaxSize: POINT,
    pub ptMaxPosition: POINT,
    pub ptMinTrackSize: POINT,
    pub ptMaxTrackSize: POINT,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PAINTSTRUCT {
    pub hdc: HDC,
    pub fErase: i32,
    pub rcPaint: RECT,
    pub fRestore: i32,
    pub fIncUpdate: i32,
    pub rgbReserved: [u8; 32],
}

impl Default for PAINTSTRUCT {
    fn default() -> Self {
        // SAFETY: すべて 0 が有効な初期値。
        unsafe { core::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BITMAPINFOHEADER {
    pub biSize: u32,
    pub biWidth: i32,
    pub biHeight: i32,
    pub biPlanes: u16,
    pub biBitCount: u16,
    pub biCompression: u32,
    pub biSizeImage: u32,
    pub biXPelsPerMeter: i32,
    pub biYPelsPerMeter: i32,
    pub biClrUsed: u32,
    pub biClrImportant: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BITMAPINFO {
    pub bmiHeader: BITMAPINFOHEADER,
    pub bmiColors: [u32; 1],
}

// --------------------------------------------------------------------------
// 定数
// --------------------------------------------------------------------------

// ウィンドウクラス
pub const CS_VREDRAW: u32 = 0x0001;
pub const CS_HREDRAW: u32 = 0x0002;
pub const CS_DBLCLKS: u32 = 0x0008;

// ウィンドウスタイル
pub const WS_OVERLAPPED: u32 = 0x0000_0000;
pub const WS_CHILD: u32 = 0x4000_0000;
pub const WS_VISIBLE: u32 = 0x1000_0000;
pub const WS_CAPTION: u32 = 0x00C0_0000;
pub const WS_SYSMENU: u32 = 0x0008_0000;
pub const WS_MINIMIZEBOX: u32 = 0x0002_0000;
pub const WS_MAXIMIZEBOX: u32 = 0x0001_0000;
pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;

// SetWindowPos
pub const SWP_NOMOVE: u32 = 0x0002;
pub const SWP_NOZORDER: u32 = 0x0004;
pub const SWP_NOACTIVATE: u32 = 0x0010;

pub const SW_SHOW: i32 = 5;

/// `GWLP_USERDATA`
pub const GWLP_USERDATA: i32 = -21;

/// `IDC_ARROW` = `MAKEINTRESOURCE(32512)`。
pub const IDC_ARROW: usize = 32512;

// GetSystemMetrics
pub const SM_CXSCREEN: i32 = 0;
pub const SM_CYSCREEN: i32 = 1;

// メッセージ
pub const WM_CREATE: u32 = 0x0001;
pub const WM_DESTROY: u32 = 0x0002;
pub const WM_SIZE: u32 = 0x0005;
pub const WM_PAINT: u32 = 0x000F;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_ERASEBKGND: u32 = 0x0014;
pub const WM_GETMINMAXINFO: u32 = 0x0024;
pub const WM_NCCREATE: u32 = 0x0081;
pub const WM_NCDESTROY: u32 = 0x0082;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_TIMER: u32 = 0x0113;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_LBUTTONDBLCLK: u32 = 0x0203;
pub const WM_MOUSEWHEEL: u32 = 0x020A;
pub const WM_DPICHANGED: u32 = 0x02E0;

/// マウスメッセージの `wParam` に載る `MK_CONTROL`。
pub const MK_CONTROL: usize = 0x0008;

// DrawText
pub const DT_LEFT: u32 = 0x0000;
pub const DT_CENTER: u32 = 0x0001;
pub const DT_RIGHT: u32 = 0x0002;
pub const DT_VCENTER: u32 = 0x0004;
pub const DT_SINGLELINE: u32 = 0x0020;
pub const DT_END_ELLIPSIS: u32 = 0x8000;

// GDI
pub const TRANSPARENT: i32 = 1;
pub const PS_SOLID: i32 = 0;
pub const NULL_BRUSH: i32 = 5;
pub const SRCCOPY: u32 = 0x00CC_0020;
pub const BI_RGB: u32 = 0;
pub const DIB_RGB_COLORS: u32 = 0;
pub const DEFAULT_CHARSET: u32 = 1;
pub const OUT_TT_PRECIS: u32 = 4;
pub const CLIP_DEFAULT_PRECIS: u32 = 0;
/// `CLEARTYPE_QUALITY`
pub const CLEARTYPE_QUALITY: u32 = 5;

// --------------------------------------------------------------------------
// user32
// --------------------------------------------------------------------------

#[link(name = "user32")]
extern "system" {
    pub fn GetModuleHandleW(lpModuleName: *const u16) -> HMODULE;
    pub fn RegisterClassW(lpWndClass: *const WNDCLASSW) -> u16;
    pub fn CreateWindowExW(
        dwExStyle: u32,
        lpClassName: *const u16,
        lpWindowName: *const u16,
        dwStyle: u32,
        X: i32,
        Y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HWND,
        hMenu: Handle,
        hInstance: HINSTANCE,
        lpParam: *const c_void,
    ) -> HWND;
    pub fn DefWindowProcW(hwnd: HWND, msg: u32, wParam: usize, lParam: isize) -> isize;
    pub fn DestroyWindow(hwnd: HWND) -> i32;
    pub fn PostQuitMessage(nExitCode: i32);
    pub fn PostMessageW(hwnd: HWND, msg: u32, wParam: usize, lParam: isize) -> i32;
    pub fn GetMessageW(lpMsg: *mut MSG, hWnd: HWND, min: u32, max: u32) -> i32;
    pub fn TranslateMessage(lpMsg: *const MSG) -> i32;
    pub fn DispatchMessageW(lpMsg: *const MSG) -> isize;
    pub fn GetWindowLongPtrW(hwnd: HWND, index: i32) -> isize;
    pub fn SetWindowLongPtrW(hwnd: HWND, index: i32, value: isize) -> isize;
    pub fn GetClientRect(hwnd: HWND, lpRect: *mut RECT) -> i32;
    pub fn GetWindowRect(hwnd: HWND, lpRect: *mut RECT) -> i32;
    pub fn SetWindowPos(
        hwnd: HWND,
        after: HWND,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
    pub fn ShowWindow(hwnd: HWND, cmd: i32) -> i32;
    pub fn SetForegroundWindow(hwnd: HWND) -> i32;
    pub fn SetTimer(hwnd: HWND, id: usize, elapse: u32, proc: Option<TIMERPROC>) -> usize;
    pub fn KillTimer(hwnd: HWND, id: usize) -> i32;
    pub fn InvalidateRect(hwnd: HWND, rect: *const RECT, erase: i32) -> i32;
    pub fn LoadCursorW(hinstance: HINSTANCE, name: *const u16) -> HCURSOR;
    pub fn GetSystemMetrics(index: i32) -> i32;
    pub fn GetCursorPos(lpPoint: *mut POINT) -> i32;
    pub fn ScreenToClient(hwnd: HWND, lpPoint: *mut POINT) -> i32;
    pub fn SetCapture(hwnd: HWND) -> HWND;
    pub fn ReleaseCapture() -> i32;
    pub fn GetActiveWindow() -> HWND;
    pub fn GetDpiForWindow(hwnd: HWND) -> u32;
}

// --------------------------------------------------------------------------
// gdi32
// --------------------------------------------------------------------------

#[link(name = "gdi32")]
extern "system" {
    pub fn BeginPaint(hwnd: HWND, lpPaint: *mut PAINTSTRUCT) -> HDC;
    pub fn EndPaint(hwnd: HWND, lpPaint: *const PAINTSTRUCT) -> i32;
    pub fn CreateCompatibleDC(hdc: HDC) -> HDC;
    pub fn CreateDIBSection(
        hdc: HDC,
        pbmi: *const BITMAPINFO,
        usage: u32,
        ppvBits: *mut *mut c_void,
        hSection: Handle,
        offset: u32,
    ) -> HBITMAP;
    pub fn SelectObject(hdc: HDC, obj: HGDIOBJ) -> HGDIOBJ;
    pub fn DeleteDC(hdc: HDC) -> i32;
    pub fn DeleteObject(obj: HGDIOBJ) -> i32;
    pub fn CreateFontW(
        cHeight: i32,
        cWidth: i32,
        cEscapement: i32,
        cOrientation: i32,
        cWeight: i32,
        bItalic: u32,
        bUnderline: u32,
        bStrikeOut: u32,
        iCharSet: u32,
        iOutPrecision: u32,
        iClipPrecision: u32,
        iQuality: u32,
        iPitchAndFamily: u32,
        pszFaceName: *const u16,
    ) -> HFONT;
    pub fn CreatePen(style: i32, width: i32, color: u32) -> HPEN;
    pub fn SetBkMode(hdc: HDC, mode: i32) -> i32;
    pub fn SetTextColor(hdc: HDC, color: u32) -> u32;
    pub fn DrawTextW(hdc: HDC, text: *const u16, count: i32, rect: *mut RECT, format: u32) -> i32;
    pub fn BitBlt(
        dest: HDC,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        src: HDC,
        sx: i32,
        sy: i32,
        rop: u32,
    ) -> i32;
    pub fn MoveToEx(hdc: HDC, x: i32, y: i32, prev: *mut POINT) -> i32;
    pub fn LineTo(hdc: HDC, x: i32, y: i32) -> i32;
    pub fn RoundRect(hdc: HDC, l: i32, t: i32, r: i32, b: i32, w: i32, h: i32) -> i32;
    pub fn GetStockObject(index: i32) -> HGDIOBJ;
}

// --------------------------------------------------------------------------
// kernel32
// --------------------------------------------------------------------------

extern "system" {
    pub fn GetTickCount64() -> u64;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 構造体の大きさ・並びが Windows SDK の定義と一致していること。
    /// （ずれていると `CreateWindowExW` / `GetMessageW` が壊れる。）
    #[test]
    fn struct_sizes_match_the_win32_abi_x64() {
        assert_eq!(core::mem::size_of::<POINT>(), 8);
        assert_eq!(core::mem::size_of::<RECT>(), 16);
        assert_eq!(core::mem::size_of::<MSG>(), 48);
        assert_eq!(core::mem::size_of::<WNDCLASSW>(), 72);
        assert_eq!(core::mem::size_of::<MINMAXINFO>(), 40);
        assert_eq!(core::mem::size_of::<PAINTSTRUCT>(), 72);
        assert_eq!(core::mem::size_of::<BITMAPINFOHEADER>(), 40);
        assert_eq!(core::mem::size_of::<BITMAPINFO>(), 44);
        // `Option<WNDPROC>` は null 許容の関数ポインタなので 1 ポインタ。
        assert_eq!(core::mem::size_of::<Option<WNDPROC>>(), 8);
    }

    /// `CREATESTRUCTW` の先頭が `lpCreateParams`（`WM_NCCREATE` で読む位置）。
    #[test]
    fn create_struct_starts_with_the_create_params() {
        assert_eq!(core::mem::offset_of!(CREATESTRUCTW, lpCreateParams), 0);
    }

    /// `MINMAXINFO` の `ptMinTrackSize` の位置（`WM_GETMINMAXINFO` で書く）。
    #[test]
    fn minmax_info_offsets() {
        assert_eq!(core::mem::offset_of!(MINMAXINFO, ptReserved), 0);
        assert_eq!(core::mem::offset_of!(MINMAXINFO, ptMinTrackSize), 24);
        assert_eq!(core::mem::offset_of!(MINMAXINFO, ptMaxTrackSize), 32);
    }

    /// 既定のハンドルは null。
    #[test]
    fn handle_default_is_null() {
        assert!(Handle::default().is_null());
        assert!(HWND::default().is_null());
    }
}
