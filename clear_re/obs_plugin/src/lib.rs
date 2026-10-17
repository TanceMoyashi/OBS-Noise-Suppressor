//! OBS Noise Suppressor — OBS Studio ネイティブ音声フィルタ（Rust 実装）。
//!
//! 旧 VST3 + atkAudio 構成を置き換えるもの。DSP は JUCE 非依存だった
//! `clear_re/plugin/src` からそのまま移植している（`src/dsp/` 以下）。
//!
//! OBS 側の要件（すべて 32.2.2 のソースで確認済み）:
//!   * PE エクスポートに `obs_module_load` があること
//!     (`util/platform-windows.c` の `has_obs_export` が名前を見るだけ)
//!   * `obs_module_load` / `obs_module_set_pointer` / `obs_module_ver` の 3 つ
//!   * `obs_module_ver()` は実行中の libobs と同じかそれ以下の API バージョン
//!
//! スレッドモデルとパニック処理は `filter.rs` を参照。

mod dsp;
mod dump;
mod ffi;
mod filter;
mod log;
mod meter;
mod paths;
mod preset;
mod ui;

use ffi::*;

use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

/// フィルタの識別子。
///
/// **改名後もこの ID は変えない。** OBS のシーンコレクションはフィルタを
/// この ID で保存しているため、変更すると既存シーンのフィルタが消える。
const FILTER_ID: &CStr = c"dpdfnet_voice_filter";

/// `enum obs_icon_type` の `OBS_ICON_TYPE_AUDIO_INPUT`。
const OBS_ICON_TYPE_AUDIO_INPUT: i32 = 4;

/// OBS が `obs_module_set_pointer` で渡してくるモジュールハンドル。
///
/// `obs_module_file()` マクロはこのポインタを `obs_find_module_file()` に
/// 渡すだけなので、プラグイン側で保持する必要がある（`obs.dll` は
/// `obs_module_set_pointer` をエクスポートしていない）。
pub(crate) static MODULE_POINTER: AtomicPtr<obs_module_t> = AtomicPtr::new(ptr::null_mut());

// --------------------------------------------------------------------------
// パニックガード
//
// OBS はオーディオスレッドで我々のコールバックを直接呼ぶ。Rust のパニックが
// FFI 境界を越えるとプロセスが即 abort するため、必ずここで捕まえて
// フォールバック値（多くは素通し）を返す。
// --------------------------------------------------------------------------

pub(crate) fn guard<T: UnwindSafeFallback>(f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            obs_log!(LOG_ERROR, "internal panic caught; falling back to passthrough");
            T::on_panic()
        }
    }
}

pub(crate) trait UnwindSafeFallback {
    fn on_panic() -> Self;
}

impl UnwindSafeFallback for () {
    fn on_panic() {}
}

impl UnwindSafeFallback for *mut obs_audio_data {
    fn on_panic() -> Self {
        ptr::null_mut()
    }
}

impl UnwindSafeFallback for *mut obs_properties_t {
    fn on_panic() -> Self {
        ptr::null_mut()
    }
}

impl UnwindSafeFallback for bool {
    fn on_panic() -> Self {
        false
    }
}

// --------------------------------------------------------------------------
// 登録
// --------------------------------------------------------------------------

static FILTER_INFO: obs_source_info = {
    let mut info = obs_source_info::zeroed();
    info.id = FILTER_ID.as_ptr();
    info.type_ = OBS_SOURCE_TYPE_FILTER;
    // OBS_SOURCE_VIDEO を含めないため、libobs 側が自動で ASYNC を付ける。
    info.output_flags = OBS_SOURCE_AUDIO;
    info.get_name = Some(filter::get_name);
    info.create = Some(filter::create);
    info.destroy = Some(filter::destroy);
    info.get_defaults = Some(filter::get_defaults);
    info.get_properties = Some(filter::get_properties);
    info.update = Some(filter::update);
    info.activate = Some(filter::activate);
    info.deactivate = Some(filter::deactivate);
    info.filter_audio = Some(filter::filter_audio);
    info.icon_type = OBS_ICON_TYPE_AUDIO_INPUT;
    info
};

// --------------------------------------------------------------------------
// モジュールエクスポート（OBS_DECLARE_MODULE 相当）
// --------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn obs_module_set_pointer(module: *mut obs_module_t) {
    MODULE_POINTER.store(module, Ordering::SeqCst);
}

/// OBS は `obs_module_ver()` が返す API バージョンを見て、自分より新しい
/// libobs 向けにビルドされたモジュールを拒否する。
#[no_mangle]
pub extern "C" fn obs_module_ver() -> u32 {
    LIBOBS_API_VER
}

#[no_mangle]
pub extern "C" fn obs_module_load() -> bool {
    match catch_unwind(AssertUnwindSafe(|| {
        unsafe {
            obs_register_source_s(&FILTER_INFO, std::mem::size_of::<obs_source_info>());
        }
        // 専用ログを最初に開く。これ以降、LOG_DEBUG 以外はファイルにも残る。
        let log_path = log::init();
        obs_log!(
            LOG_INFO,
            "OBS Noise Suppressor v{} を読み込みました",
            env!("CARGO_PKG_VERSION")
        );
        obs_log!(
            LOG_INFO,
            "module loaded (libobs api {:#010x}, obs_source_info {} bytes, ort api {})",
            LIBOBS_API_VER,
            std::mem::size_of::<obs_source_info>(),
            dsp::ort::api_version()
        );
        if let Some(dir) = paths::own_dll_dir() {
            obs_log!(LOG_INFO, "dll: {}", dir.display());
        }
        match &log_path {
            Some(p) => obs_log!(LOG_INFO, "log file: {}", p.display()),
            None => obs_log!(
                LOG_WARNING,
                "専用ログファイルを作れませんでした (OBS 標準ログのみになります)"
            ),
        }
        true
    })) {
        Ok(v) => v,
        Err(_) => {
            obs_log!(LOG_ERROR, "panic in obs_module_load");
            false
        }
    }
}

#[no_mangle]
pub extern "C" fn obs_module_description() -> *const c_char {
    c"OBS Noise Suppressor - speech enhancement (denoise / dereverb), OBS native".as_ptr()
}

/// モジュールのアンロード。
///
/// パネルは専用スレッドで動いており、そのスレッドは我々のコード上を走っている。
/// `FreeLibrary` でコードが消える前に必ず終わらせる。
#[no_mangle]
pub extern "C" fn obs_module_unload() {
    crate::guard(|| {
        ui::shutdown(2000);
        obs_log!(LOG_INFO, "module unloaded");
    })
}

#[no_mangle]
pub extern "C" fn obs_module_name() -> *const c_char {
    c"OBS Noise Suppressor".as_ptr()
}
