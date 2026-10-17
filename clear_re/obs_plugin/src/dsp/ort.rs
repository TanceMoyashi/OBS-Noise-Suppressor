//! 同梱 ONNX Runtime の動的ロード。
//!
//! 移植元: `clear_re/plugin/src/OrtRuntime.cpp`。
//!
//! **重要**: OBS プロセスには他プラグイン（atkAudio 等）が System32 の
//! `onnxruntime.dll`（1.17）を既にロードしていることがある。名前だけで
//! 解決するとそちらを掴み、API 不一致で即死する。したがって
//!
//!   * 名前は `dpdfnet_onnxruntime.dll`（同梱物）を最優先
//!   * 必ず絶対パス + `LOAD_WITH_ALTERED_SEARCH_PATH` で読む
//!     （同梱物の隣にある VC ランタイム等もそのディレクトリから解決させる）
//!
//! という 2 点を守る。リンクは一切しない（`ort-sys` の `disable-linking`）。

use std::ffi::{c_char, CStr};
use std::path::PathBuf;
use std::sync::OnceLock;

use ort_sys::{OrtApi, OrtApiBase, OrtLoggingLevel, OrtStatusPtr};

use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};

use crate::paths;

/// `OrtGetApiBase` の型。
type GetApiBaseFn = unsafe extern "system" fn() -> *const OrtApiBase;

struct Runtime {
    api: &'static OrtApi,
    dll_path: PathBuf,
    version: String,
    /// HMODULE は解放しない（プロセス終了まで保持）。生ポインタは `Send`/`Sync`
    /// でないためアドレスとして持つ。
    _module_addr: usize,
}

// SAFETY: Runtime が持つのはロード済み DLL 内の静的データへの参照だけで、
// 書き換えは行わない。HMODULE は解放しないのでアドレスは有効なまま。
unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}

static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

/// `dpdfnet_onnxruntime.dll` を読み込み、`OrtApi` (ORT_API_VERSION = 17) を返す。
///
/// 初回だけ実際にロードし、以降はその結果（失敗も含む）を返す。
pub fn ensure_loaded(extra_dirs: &[PathBuf]) -> Result<&'static OrtApi, String> {
    match RUNTIME.get_or_init(|| {
        let r = load(extra_dirs);
        // どの DLL を掴んだかは不具合切り分けの要（System32 の ORT と取り違えやすい）。
        match &r {
            Ok(rt) => crate::obs_log!(
                crate::ffi::LOG_INFO,
                "onnxruntime {} ({}) を読み込みました",
                rt.version,
                rt.dll_path.display()
            ),
            Err(e) => crate::obs_log!(
                crate::ffi::LOG_WARNING,
                "onnxruntime を読み込めません: {e}"
            ),
        }
        r
    }) {
        Ok(rt) => Ok(rt.api),
        Err(e) => Err(e.clone()),
    }
}

/// ロード結果の説明（テストから状態を覗くためだけに残している）。
///
/// 実運用では `ensure_loaded` が同じ内容を OBS のログへ出す。
#[cfg(test)]
pub fn description() -> String {
    match RUNTIME.get() {
        Some(Ok(rt)) => format!("onnxruntime {} ({})", rt.version, rt.dll_path.display()),
        Some(Err(e)) => e.clone(),
        None => "onnxruntime not loaded yet".to_string(),
    }
}

fn load(extra_dirs: &[PathBuf]) -> Result<Runtime, String> {
    let candidates = paths::ort_dll_candidates(extra_dirs);

    if candidates.is_empty() {
        return Err(format!(
            "dpdfnet_onnxruntime.dll が見つかりません。探索先: {} \
             （DPDFNET_ORT で明示指定できます）",
            paths::ort_dll_candidates(&[])
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let mut tried = Vec::new();
    for path in candidates {
        match open(&path) {
            Ok(rt) => return Ok(rt),
            Err(e) => tried.push(format!("{}: {}", path.display(), e)),
        }
    }

    Err(format!(
        "同梱 onnxruntime を読み込めませんでした ({})",
        tried.join(" / ")
    ))
}

fn open(path: &PathBuf) -> Result<Runtime, String> {
    let wide = to_wide_path(path);

    // SAFETY: wide は NUL 終端済み。絶対パスなので ALTERED_SEARCH_PATH が有効。
    let h: HMODULE = unsafe { LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }
        .map_err(|e| format!("LoadLibraryExW: {e}"))?;

    // SAFETY: h は直前にロードした有効なモジュール。
    let proc = unsafe { GetProcAddress(h, PCSTR(b"OrtGetApiBase\0".as_ptr())) };

    let Some(proc) = proc else {
        return Err("OrtGetApiBase がエクスポートされていない".to_string());
    };

    // SAFETY: proc は OrtGetApiBase の実体。
    let get_api_base: GetApiBaseFn = unsafe { std::mem::transmute(proc) };
    let base_ptr = unsafe { get_api_base() };

    if base_ptr.is_null() {
        return Err("OrtGetApiBase が NULL を返した".to_string());
    }

    // SAFETY: ORT が返した有効なポインタ。ロード中は有効。
    let base = unsafe { &*base_ptr };
    let version = {
        // SAFETY: GetVersionString は静的 UTF-8 を返す。
        let p = unsafe { (base.GetVersionString)() };
        if p.is_null() {
            "unknown".to_string()
        } else {
            // SAFETY: NULL 終端済み UTF-8。
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    };

    let api_ptr = unsafe { (base.GetApi)(ort_sys::ORT_API_VERSION) };
    if api_ptr.is_null() {
        return Err(format!(
            "この onnxruntime は API version {} を持たない (ORT {version})",
            ort_sys::ORT_API_VERSION
        ));
    }

    // SAFETY: ORT が返した OrtApi はプロセス終了まで有効な静的データ。
    let api: &'static OrtApi = unsafe { &*api_ptr };

    Ok(Runtime {
        api,
        dll_path: path.clone(),
        version,
        _module_addr: h.0 as usize,
    })
}

/// `OrtStatus*` を検査して、エラーならメッセージを返す（status は消費する）。
pub fn check(api: &OrtApi, st: OrtStatusPtr) -> Result<(), String> {
    if st.0.is_null() {
        return Ok(());
    }

    // SAFETY: NULL でない OrtStatus* は有効。
    let msg_ptr = unsafe { (api.GetErrorMessage)(st.0) };
    let msg = if msg_ptr.is_null() {
        "unknown onnxruntime error".to_string()
    } else {
        unsafe { CStr::from_ptr(msg_ptr) }.to_string_lossy().into_owned()
    };

    // SAFETY: check は status の所有権を引き取り、必ず解放する。
    unsafe { (api.ReleaseStatus)(st.0) };

    Err(msg)
}

/// `OrtApi` のバージョン（ログ用）。
pub fn api_version() -> u32 {
    ort_sys::ORT_API_VERSION
}

/// ログレベル。モデルロード失敗だけ見たいので警告以上。
pub fn logging_level() -> OrtLoggingLevel {
    OrtLoggingLevel::ORT_LOGGING_LEVEL_WARNING
}

fn to_wide_path(p: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

// --------------------------------------------------------------------------
// プロセスで 1 つの OrtEnv
// --------------------------------------------------------------------------

static ENV: OnceLock<Result<usize, String>> = OnceLock::new();

/// `OrtEnv` をプロセスで 1 つだけ作る（OrtRuntime.cpp と同じ方針）。
///
/// 戻り値は `*mut OrtEnv`。解放しない（プロセス終了まで保持）。
pub fn global_env(api: &'static OrtApi) -> Result<usize, String> {
    match ENV.get_or_init(|| {
        let mut env: *mut ort_sys::OrtEnv = std::ptr::null_mut();
        // SAFETY: api は有効な OrtApi、env は有効な書き込み先。
        let st = unsafe {
            (api.CreateEnv)(
                logging_level(),
                c"obs-noise-suppressor".as_ptr() as *const c_char,
                &mut env,
            )
        };
        check(api, st)?;
        if env.is_null() {
            return Err("CreateEnv が NULL を返した".to_string());
        }
        Ok(env as usize)
    }) {
        Ok(v) => Ok(*v),
        Err(e) => Err(e.clone()),
    }
}
