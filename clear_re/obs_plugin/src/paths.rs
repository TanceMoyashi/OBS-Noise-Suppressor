//! パス探索のうち **OBS 固有のもの**。
//!
//! ホスト非依存の部分（モデル / ONNX Runtime の探索、`ModelChoice` /
//! `DeviceChoice`）は共通クレート `dpdfnet::paths` にある（VST3 版と共有）。
//! ここはそれを再輸出しつつ、libobs の設定ファイル API だけを足す。

use std::path::PathBuf;

/// 共通クレートの探索関数を、OBS 版でも今までどおり `paths::～` で使えるようにする。
pub use dpdfnet::paths::{
    env_model_file, find_model_choice, model_dirs, ort_dll_candidates, own_dll_dir, DeviceChoice,
    ModelChoice,
};

/// ユーザー設定のファイルパス（`obs_module_config_path(name)` 相当）。
///
/// `%APPDATA%\obs-studio\plugin_config\obs_noise_suppressor\<name>`。
/// ディレクトリは libobs 側が作る。モジュールポインタ未設定なら `None`。
pub fn config_file(name: &str) -> Option<PathBuf> {
    let module = crate::MODULE_POINTER.load(std::sync::atomic::Ordering::SeqCst);
    if module.is_null() {
        return None;
    }

    let c = std::ffi::CString::new(name).ok()?;
    let p = unsafe { crate::ffi::obs_module_get_config_path(module, c.as_ptr()) };
    if p.is_null() {
        return None;
    }

    let s = unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned();
    unsafe { crate::ffi::bfree(p as *mut std::ffi::c_void) };

    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}
