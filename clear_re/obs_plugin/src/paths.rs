//! パス探索: 自モジュールの位置、同梱 ONNX Runtime、モデルファイル。
//!
//! 移植元: `PluginProcessor.cpp` の `getPluginDirectory()` / `resolveModelFile()` と
//! `OrtRuntime.cpp` の探索順。VST3 では bundle の中だったので 3 階層上まで
//! `models/` を探していたが、OBS では
//!
//! ```text
//! <plugin root>/bin/64bit/obs_noise_suppressor.dll   ← ここ
//! <plugin root>/data/models/*.onnx            ← obs_module_file() の既定
//! ```
//!
//! という配置なので、両方を候補に入れる。

use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};

/// 品質優先 → 速度優先。2 出力版 (spec_ns 付き = Voice Reverb 可) を優先する。
pub const MODEL_FILE_NAMES: [&str; 4] = [
    "dpdfnet8_48khz_hr_2out.onnx",
    "dpdfnet2_48khz_hr_2out.onnx",
    "dpdfnet8_48khz_hr.onnx",
    "dpdfnet2_48khz_hr.onnx",
];

/// 同梱 ONNX Runtime のファイル名。
///
/// **`onnxruntime.dll` という名前で System32 から読み込んではいけない**。
/// OBS のプロセスには他プラグイン (atkAudio 等) が System32 の ORT 1.17 を
/// 既にロードしていることがあり、名前だけで解決するとそちらを掴まされる。
/// それを避けるため、読み込みは必ず絶対パス + `LOAD_WITH_ALTERED_SEARCH_PATH`。
pub const ORT_DLL_NAMES: [&str; 2] = ["dpdfnet_onnxruntime.dll", "onnxruntime.dll"];

/// 自分自身 (`obs_noise_suppressor.dll`) が置かれているディレクトリ。
pub fn own_dll_dir() -> Option<PathBuf> {
    unsafe extern "system" fn marker() {}

    let mut h = HMODULE::default();
    // SAFETY: marker はこのモジュール内の関数で、FROM_ADDRESS で自モジュールの
    // HMODULE が得られる。参照カウントは UNCHANGED_REFCOUNT なので増えない。
    let r = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(marker as *const () as *const u16),
            &mut h,
        )
    };
    if r.is_err() {
        return None;
    }

    let mut buf = [0u16; 4096];
    // SAFETY: buf は十分な長さの書き込み可能な配列。
    let n = unsafe { GetModuleFileNameW(Some(h), &mut buf) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }

    let s = String::from_utf16_lossy(&buf[..n]);
    Path::new(&s).parent().map(|p| p.to_path_buf())
}

/// ユーザーのドキュメントフォルダ（VST3 版と同じ場所も見るため）。
///
/// `%USERPROFILE%\Documents` を決め打ちすると、Documents が別ドライブや OneDrive に
/// リダイレクトされている環境で外す（この開発機も `D:\Users\...\Documents`）。
/// まず既知フォルダ API に訊き、失敗したときだけ環境変数に落ちる。
fn documents_dir() -> Option<PathBuf> {
    if let Some(p) = known_documents() {
        return Some(p);
    }
    let home = std::env::var_os("USERPROFILE")?;
    Some(PathBuf::from(home).join("Documents"))
}

/// `FOLDERID_Documents` を解決する。失敗したら `None`。
fn known_documents() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Documents, KF_FLAG_DEFAULT};

    // SAFETY: 成功時の戻り値は CoTaskMemAlloc された文字列なので、String に
    // コピーしたあと CoTaskMemFree で解放する。
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
        let s = s?;
        if s.is_empty() {
            return None;
        }
        Some(PathBuf::from(s))
    }
}

/// ディレクトリ群の中から最初に見つかったモデルを返す。
pub fn find_model(extra_dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in model_dirs(extra_dirs) {
        for name in MODEL_FILE_NAMES {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// モデル探索ディレクトリ（優先順）。
pub fn model_dirs(extra_dirs: &[PathBuf]) -> Vec<PathBuf> {
    // 1) 環境変数 (ファイルでもディレクトリでもよい)
    if let Some(v) = std::env::var_os("DPDFNET_MODEL") {
        let f = PathBuf::from(v);
        if f.is_file() {
            // ファイル指定はその 1 つだけを返すため、特別扱いする。
            return vec![f.parent().map(|p| p.to_path_buf()).unwrap_or_default()];
        }
        if f.is_dir() {
            let mut dirs = vec![f];
            dirs.extend(extra_dirs.iter().cloned());
            return dirs;
        }
    }

    let mut dirs: Vec<PathBuf> = Vec::new();

    // 2) OBS のモジュールデータディレクトリ
    dirs.extend(extra_dirs.iter().cloned());

    // 3) 自分の DLL の隣と、その親 (VST3 版と同じ 3 階層上まで)
    if let Some(dll_dir) = own_dll_dir() {
        dirs.push(dll_dir.join("models"));
        // OBS 配置: <root>/bin/64bit → <root>/data
        dirs.push(dll_dir.join("..").join("..").join("data").join("models"));
        dirs.push(dll_dir.join("..").join("..").join("data"));

        let mut up = dll_dir;
        for _ in 0..3 {
            up = match up.parent() {
                Some(p) => p.to_path_buf(),
                None => break,
            };
            dirs.push(up.join("models"));
        }
    }

    // 4) ユーザー文書フォルダ
    if let Some(docs) = documents_dir() {
        dirs.push(docs.join("DPDFNetVoice").join("models"));
        // 既存の VST3 インストールをそのまま使えるようにする。
        dirs.push(docs.join("OBS").join("VST3").join("models"));
    }

    dirs
}

/// 環境変数 `DPDFNET_MODEL` がファイルを直接指しているならそれを優先する。
pub fn env_model_file() -> Option<PathBuf> {
    let v = std::env::var_os("DPDFNET_MODEL")?;
    let f = PathBuf::from(v);
    if f.is_file() {
        Some(f)
    } else {
        None
    }
}

/// ONNX Runtime DLL の候補（優先順、すべて絶対パス）。
pub fn ort_dll_candidates(extra_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    // 1) 環境変数 (ファイルでもディレクトリでもよい)
    if let Some(v) = std::env::var_os("DPDFNET_ORT") {
        let p = PathBuf::from(v);
        if p.is_file() {
            out.push(p);
        } else if p.is_dir() {
            for name in ORT_DLL_NAMES {
                out.push(p.join(name));
            }
        }
    }

    // 2) OBS のモジュールデータディレクトリ / 自分の DLL の隣
    let mut dirs: Vec<PathBuf> = extra_dirs.to_vec();
    if let Some(dll_dir) = own_dll_dir() {
        dirs.push(dll_dir.join("..").join("..").join("data"));
        dirs.push(dll_dir);
    }
    // 3) VST3 版が入っていれば、その同梱 ORT をそのまま使える
    //    (コピーし忘れても動くようにするための保険)。
    if let Some(docs) = documents_dir() {
        dirs.push(
            docs.join("OBS")
                .join("VST3")
                .join("DPDFNet Voice.vst3")
                .join("Contents")
                .join("x86_64-win"),
        );
    }
    for d in dirs {
        for name in ORT_DLL_NAMES {
            out.push(d.join(name));
        }
    }

    out.retain(|p| p.is_file());
    out
}
