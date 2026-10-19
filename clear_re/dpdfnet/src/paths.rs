//! パス探索: 自分自身の DLL の位置、同梱 ONNX Runtime、モデルファイル。
//!
//! **ホスト非依存**（OBS 版と VST3 版で共有する）。探索順は
//!
//!   1. 環境変数 (`DPDFNET_ORT` / `DPDFNET_MODEL`)
//!   2. ホストが渡す `extra_dirs`（OBS: `data/`、VST3: bundle の中）
//!   3. 自分の DLL の隣と、そこから 3 階層上までの `models/`
//!      （`<root>/bin/64bit/...` → `<root>/data/models` のような配置を拾う）
//!   4. ユーザーのドキュメントフォルダ（旧 VST3 版の置き場）
//!
//! という並びで、最初に見つかったものを使う。移植元は `PluginProcessor.cpp` の
//! `getPluginDirectory()` / `resolveModelFile()` と `OrtRuntime.cpp` の探索順。

use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};

/// 自動 (`ModelChoice::Auto`) の探索順。
///
/// **先頭は中間 (dp4)**。品質 (dp8) は重く、軽量 (dp2) は音質が落ちるため、
/// 1 ホップ = 10 ms の予算に余裕を持って収まる中間を既定にしている。以降は
/// 品質 → 軽量の順に落ちる。2 出力版 (spec_ns 付き = Voice Reverb 可) を
/// 優先し、無ければ 1 出力版へ落ちる。
pub const MODEL_FILE_NAMES: [&str; 5] = [
    "dpdfnet4_48khz_hr_2out.onnx",
    "dpdfnet8_48khz_hr_2out.onnx",
    "dpdfnet2_48khz_hr_2out.onnx",
    "dpdfnet8_48khz_hr.onnx",
    "dpdfnet2_48khz_hr.onnx",
];

/// 高品質モデル (dpdfnet8) のみ。2 出力版を優先。
pub const MODEL_FILE_NAMES_DP8: [&str; 2] = [
    "dpdfnet8_48khz_hr_2out.onnx",
    "dpdfnet8_48khz_hr.onnx",
];

/// 中間モデル (dpdfnet4) のみ。1 出力版は同梱していない。
pub const MODEL_FILE_NAMES_DP4: [&str; 1] = ["dpdfnet4_48khz_hr_2out.onnx"];

/// 軽量モデル (dpdfnet2) のみ。2 出力版を優先。
pub const MODEL_FILE_NAMES_DP2: [&str; 2] = [
    "dpdfnet2_48khz_hr_2out.onnx",
    "dpdfnet2_48khz_hr.onnx",
];

/// 使用するモデルの選択（OBS のプロパティ「モデル」）。
///
/// 同梱しているのは `dpdfnet8` / `dpdfnet4` / `dpdfnet2` の 3 種なので、
/// 選択肢はこの 4 つ。保存される文字列 (`as_str`) と `code()` は**変えては
/// いけない**（シーンコレクションに保存済みの値・プリセットの `model` 文字列・
/// `AtomicU8` の code が読めなくなる）。`Middle` は末尾に足して code 3 を
/// 使うので、既存の 0 / 1 / 2 はそのまま。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ModelChoice {
    /// 自動: 中間 (dp4) を優先し、無ければ品質 (dp8) → 軽量 (dp2) と落ちる。
    #[default]
    Auto,
    /// 高品質 (`dpdfnet8_*`)。
    Quality,
    /// 軽量・低負荷 (`dpdfnet2_*`)。
    Fast,
    /// 中間 (`dpdfnet4_*`)。品質と速度の中間で、**自動の既定**。
    Middle,
}

impl ModelChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Quality => "dp8",
            Self::Fast => "dp2",
            Self::Middle => "dp4",
        }
    }

    /// 設定文字列から。未知の値（空文字など）は `Auto`。
    pub fn from_setting(s: &str) -> Self {
        match s {
            "dp8" | "quality" => Self::Quality,
            "dp2" | "fast" => Self::Fast,
            // 保存側は "dp4" だが、"middle" も受け付ける。
            "dp4" | "middle" => Self::Middle,
            _ => Self::Auto,
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Quality => 1,
            Self::Fast => 2,
            // 既存 0 / 1 / 2 は変えられないので、空いている 3 を使う。
            Self::Middle => 3,
        }
    }

    pub fn from_code(c: u8) -> Self {
        match c {
            1 => Self::Quality,
            2 => Self::Fast,
            3 => Self::Middle,
            _ => Self::Auto,
        }
    }

    /// 探索するファイル名（優先順）。
    pub fn names(self) -> &'static [&'static str] {
        match self {
            Self::Auto => &MODEL_FILE_NAMES,
            Self::Quality => &MODEL_FILE_NAMES_DP8,
            Self::Fast => &MODEL_FILE_NAMES_DP2,
            Self::Middle => &MODEL_FILE_NAMES_DP4,
        }
    }
}

/// 同梱 ONNX Runtime のファイル名。
///
/// **`onnxruntime.dll` という名前で System32 から読み込んではいけない**。
/// OBS のプロセスには他プラグイン (atkAudio 等) が System32 の ORT 1.17 を
/// 既にロードしていることがあり、名前だけで解決するとそちらを掴まされる。
/// それを避けるため、読み込みは必ず絶対パス + `LOAD_WITH_ALTERED_SEARCH_PATH`。
pub const ORT_DLL_NAMES: [&str; 2] = ["dpdfnet_onnxruntime.dll", "onnxruntime.dll"];

/// GPU (DirectML) 用 ONNX Runtime のファイル名。
///
/// CPU 版とは**別の DLL**（DirectML EP 入りのビルド）を同梱している。同じ
/// `onnxruntime.dll` という名前で 2 つ置けないため、名前を分けている。
/// CPU 版にフォールバックはさせない（DML EP が無いビルドに DML を足すと
/// セッション作成に失敗するだけで、原因が分かりにくくなる）。
pub const ORT_DLL_NAMES_GPU: [&str; 1] = ["dpdfnet_onnxruntime_dml.dll"];

/// ORT DLL の隣に置く、GPU 用の追加 DLL（見つかれば先に読み込む）。
///
/// `DirectML.dll` は ORT が**実行時に** `LoadLibrary` する。これは静的な
/// import ではないので `LOAD_WITH_ALTERED_SEARCH_PATH` の恩恵を受けず、
/// 同梱ディレクトリからは見つからない。あらかじめこちらから読んでおく。
pub const GPU_SIDE_DLL_NAMES: [&str; 2] = ["DirectML.dll", "onnxruntime_providers_shared.dll"];

/// 推論に使うデバイス（OBS のプロパティ「推論デバイス」）。
///
/// 既定は `Cpu`。**GPU は実測で CPU より遅い**（per-hop の dispatch / sync が
/// 支配的なため。`OBS_VERIFY.md` の「GPU の実測」を参照）が、CPU 側の余裕が
/// まったく無い環境向けに選択肢として残してある。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DeviceChoice {
    /// CPU（既定）。`ort_dll_candidates` の 1 番目を使う。
    #[default]
    Cpu,
    /// GPU (DirectML)。同梱の `dpdfnet_onnxruntime_dml.dll` を使う。
    Gpu,
}

impl DeviceChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
        }
    }

    /// 設定文字列から。未知の値（空文字など）は `Cpu`。
    pub fn from_setting(s: &str) -> Self {
        match s {
            "gpu" | "dml" | "directml" => Self::Gpu,
            _ => Self::Cpu,
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Cpu => 0,
            Self::Gpu => 1,
        }
    }

    pub fn from_code(c: u8) -> Self {
        match c {
            1 => Self::Gpu,
            _ => Self::Cpu,
        }
    }

    /// パネル / ログに出す短い名前。
    pub fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Gpu => "GPU",
        }
    }

    /// 探索する ORT DLL 名（優先順）。
    pub fn ort_dll_names(self) -> &'static [&'static str] {
        match self {
            Self::Cpu => &ORT_DLL_NAMES,
            Self::Gpu => &ORT_DLL_NAMES_GPU,
        }
    }
}

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

/// ディレクトリ群の中から最初に見つかったモデルを返す（モデル種別を指定）。
pub fn find_model_choice(choice: ModelChoice, extra_dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in model_dirs(extra_dirs) {
        for name in choice.names() {
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

/// ONNX Runtime DLL の候補（優先順、すべて絶対パス）。`device` ごとに別の DLL。
pub fn ort_dll_candidates(device: DeviceChoice, extra_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let names = device.ort_dll_names();
    let mut out: Vec<PathBuf> = Vec::new();

    // 1) 環境変数 (ファイルでもディレクトリでもよい)
    if let Some(v) = std::env::var_os("DPDFNET_ORT") {
        let p = PathBuf::from(v);
        if p.is_file() {
            out.push(p);
        } else if p.is_dir() {
            for name in names {
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
        for name in names {
            out.push(d.join(name));
        }
    }

    out.retain(|p| p.is_file());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_choice_round_trips() {
        for c in [
            ModelChoice::Auto,
            ModelChoice::Quality,
            ModelChoice::Fast,
            ModelChoice::Middle,
        ] {
            assert_eq!(ModelChoice::from_setting(c.as_str()), c);
            assert_eq!(ModelChoice::from_code(c.code()), c);
        }
    }

    #[test]
    fn unknown_model_setting_falls_back_to_auto() {
        assert_eq!(ModelChoice::from_setting(""), ModelChoice::Auto);
        assert_eq!(ModelChoice::from_setting("dpdfnet8"), ModelChoice::Auto);
        assert_eq!(ModelChoice::from_code(255), ModelChoice::Auto);
        assert_eq!(ModelChoice::default(), ModelChoice::Auto);
    }

    #[test]
    fn model_choice_accepts_aliases() {
        assert_eq!(ModelChoice::from_setting("quality"), ModelChoice::Quality);
        assert_eq!(ModelChoice::from_setting("fast"), ModelChoice::Fast);
        // 新しい中間モデルは保存側 "dp4" のほか "middle" も受ける。
        assert_eq!(ModelChoice::from_setting("middle"), ModelChoice::Middle);
        assert_eq!(ModelChoice::from_setting("dp4"), ModelChoice::Middle);
    }

    /// `code()` は既存の値（0 / 1 / 2）を変えてはいけない（保存済み設定の互換）。
    #[test]
    fn model_codes_keep_their_saved_values() {
        assert_eq!(ModelChoice::Auto.code(), 0);
        assert_eq!(ModelChoice::Quality.code(), 1);
        assert_eq!(ModelChoice::Fast.code(), 2);
        assert_eq!(ModelChoice::Middle.code(), 3);
    }

    #[test]
    fn model_names_are_ordered_by_preference() {
        // 2 出力版 (Voice Reverb 付き) を先に見る。
        assert_eq!(ModelChoice::Middle.names()[0], "dpdfnet4_48khz_hr_2out.onnx");
        assert_eq!(ModelChoice::Quality.names()[0], "dpdfnet8_48khz_hr_2out.onnx");
        assert_eq!(ModelChoice::Fast.names()[0], "dpdfnet2_48khz_hr_2out.onnx");
        // Auto は 5 ファイル全部を中間優先で見る。
        assert_eq!(ModelChoice::Auto.names().len(), MODEL_FILE_NAMES.len());
        assert_eq!(ModelChoice::Auto.names()[0], "dpdfnet4_48khz_hr_2out.onnx");
        assert_eq!(ModelChoice::Auto.names()[1], "dpdfnet8_48khz_hr_2out.onnx");
        assert_eq!(ModelChoice::Auto.names()[2], "dpdfnet2_48khz_hr_2out.onnx");
        assert_eq!(ModelChoice::Auto.names()[4], "dpdfnet2_48khz_hr.onnx");
    }
}
