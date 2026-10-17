//! 専用ログファイル（エラー / 警告 / 起動情報）。
//!
//! OBS 標準ログ (`blog()`) は OBS 本体や他プラグインの出力と混ざるので、
//! 自分のプラグインで何が起きたかを後から追いにくい。ここでは
//!
//! ```text
//! %APPDATA%\obs-studio\plugin_config\obs_noise_suppressor\obs_noise_suppressor.log
//! ```
//!
//! に時刻付きで書き出す。OBS 標準ログにも従来どおり出るので二重記録になる。
//!
//! * 記録するレベルは `LOG_ERROR` / `LOG_WARNING` / `LOG_INFO`。
//!   `LOG_DEBUG` は OBS 標準ログだけに出す（ファイルは「後から不具合を追う」
//!   用途に絞るため）。
//! * `MAX_BYTES` を超えていたら、開くときに `.1` へ退避する（世代は最新 2 つ）。
//! * 常時 ON。設定は持たない。
//! * 書けなくても音声処理は止めない（黙って諦める）。
//!
//! 音声スレッドからも呼ばれるが、呼ぶ側が「1 回だけ」に絞っている
//! (`passthrough_logged` / panic 捕捉) のでロック待ちは実質起きない。念のため、
//! まったく同じメッセージが連続した場合は間引く（毎フレーム同じ警告が出ても
//! ファイルが溢れないようにする）。

use std::ffi::c_int;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// ログファイル名（`paths::config_file()` に渡す名前）。
pub const FILE_NAME: &str = "obs_noise_suppressor.log";

/// これを超えていたら開くときに `.1` へ退避する。
pub const MAX_BYTES: u64 = 1 << 20; // 1 MiB

/// 1 セッションで書く上限。間引きをすり抜ける暴走への保険。
pub const SESSION_MAX_BYTES: u64 = 4 * MAX_BYTES;

/// 退避ファイルの接尾辞。
const BACKUP_SUFFIX: &str = ".1";

/// 複数行メッセージの 2 行目以降に付ける字下げ。
const INDENT: &str = "    ";

/// ファイルに残すレベル。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Error,
    Warning,
    Info,
}

impl Level {
    /// ファイルに書く文字列。
    fn tag(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warning => "WARNING",
            Self::Info => "INFO",
        }
    }

    /// OBS の `LOG_*` 値から。ファイルに残さないレベルは `None`。
    pub fn from_obs(level: c_int) -> Option<Self> {
        match level {
            crate::ffi::LOG_ERROR => Some(Self::Error),
            crate::ffi::LOG_WARNING => Some(Self::Warning),
            crate::ffi::LOG_INFO => Some(Self::Info),
            _ => None,
        }
    }
}

/// ローカル時刻を `YYYY-MM-DD HH:MM:SS.mmm` で。
fn timestamp() -> String {
    // SAFETY: 引数も戻り値も値だけの Win32 API（ポインタを保持しない）。
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

/// 複数行のメッセージを 1 レコードにまとめる（2 行目以降は字下げ）。
fn indent_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            out.push_str(INDENT);
        }
        out.push_str(line.strip_suffix('\r').unwrap_or(line));
    }
    out.push('\n');
    out
}

/// 実際に書き込むファイル。
pub struct Sink {
    file: File,
    path: PathBuf,
    /// 現在のファイルサイズ（だいたい。書き込みバイト数の累計）。
    written: u64,
    /// このセッションで書く上限。
    session_bytes: u64,
    /// 直前に書いたメッセージ（連続重複の間引き用）。
    last: Option<(Level, String)>,
    /// `last` と同じメッセージを続けて受け取った回数。
    repeat: u64,
    /// 上限に達して以降書かない。
    full: bool,
}

impl Sink {
    /// 既定の上限で開く。
    pub fn open(path: &Path) -> std::io::Result<Self> {
        Self::open_with_limits(path, MAX_BYTES, SESSION_MAX_BYTES)
    }

    /// 上限を指定して開く（テスト用）。
    pub fn open_with_limits(
        path: &Path,
        rotate_bytes: u64,
        session_bytes: u64,
    ) -> std::io::Result<Self> {
        // libobs 側が作るはずだが、無い環境でも書けるようにしておく。
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }

        let rotate = rotate_if_needed(path, rotate_bytes);
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);

        let mut sink = Self {
            file,
            path: path.to_path_buf(),
            written,
            session_bytes,
            last: None,
            repeat: 0,
            full: false,
        };
        if rotate {
            sink.write(Level::Info, "ログが大きくなったので、前回ぶんを .1 に退避しました");
        }
        Ok(sink)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 1 件書く。まったく同じメッセージが続いた場合は間引く。
    pub fn write(&mut self, level: Level, message: &str) {
        let message = message.trim_end_matches(['\r', '\n']);
        if message.is_empty() {
            return;
        }

        if let Some((last_level, last_text)) = &self.last {
            if *last_level == level && last_text == message {
                self.repeat += 1;
                // 2, 4, 8, ... 回目だけ記録する（毎フレーム同じ警告が出る場合の保険）。
                if self.repeat >= 2 && self.repeat.is_power_of_two() {
                    let n = self.repeat;
                    self.emit(Level::Info, &format!("（直前と同じメッセージを追加で {n} 回受け取りました）"));
                }
                return;
            }
        }

        self.last = Some((level, message.to_string()));
        self.repeat = 0;
        self.emit(level, message);
    }

    /// `時刻 [レベル] 本文` を書く。
    fn emit(&mut self, level: Level, body: &str) {
        if self.full {
            return;
        }

        let text = format!("{} [{}] {}", timestamp(), level.tag(), body);
        let buf = indent_lines(&text);

        if self.written + buf.len() as u64 > self.session_bytes {
            self.full = true;
            let stop = format!(
                "{} [INFO] ログが上限 ({} バイト) に達したので、このセッションの記録はここで終わりにします\n",
                timestamp(),
                self.session_bytes
            );
            let _ = self.file.write_all(stop.as_bytes());
            let _ = self.file.flush();
            return;
        }

        if self.file.write_all(buf.as_bytes()).is_err() {
            return;
        }
        let _ = self.file.flush();
        self.written += buf.len() as u64;
    }
}

/// 既存のログが `limit` 以上なら `<name>.1` へ退避する（古い `.1` は消す）。
///
/// Windows では開いているファイルを rename できないので、必ず「まだ開いて
/// いない」状態で呼ぶこと。
fn rotate_if_needed(path: &Path, limit: u64) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() < limit {
        return false;
    }

    let backup = backup_path(path);
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(path, &backup).is_ok()
}

fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(BACKUP_SUFFIX);
    PathBuf::from(s)
}

// --------------------------------------------------------------------------
// プロセス全体で 1 つのログファイル
// --------------------------------------------------------------------------

enum State {
    /// まだ開いていない（`obs_module_set_pointer` 前を含む）。
    Uninit,
    /// 開けた。
    Ready(Box<Sink>),
    /// 開けなかったので諦めた。
    Disabled,
}

/// 開こうとした結果。
enum TryOpen {
    Ready(Box<Sink>),
    /// パスがまだ取れない（モジュールポインタ未設定）。
    NoPath,
    /// パスは取れたが開けなかった。
    Failed,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::Uninit))
}

fn try_open() -> TryOpen {
    let Some(path) = crate::paths::config_file(FILE_NAME) else {
        return TryOpen::NoPath;
    };
    match Sink::open(&path) {
        Ok(s) => TryOpen::Ready(Box::new(s)),
        Err(_) => TryOpen::Failed,
    }
}

/// 未初期化なら開く。`NoPath` のときだけ状態を変えずに残し、次の機会に再挑戦する
/// （`log::init()` が `obs_module_set_pointer` より先に走った場合の保険）。
fn ensure<F: FnOnce() -> TryOpen>(state: &mut State, open: F) -> bool {
    if matches!(state, State::Uninit) {
        match open() {
            TryOpen::Ready(s) => *state = State::Ready(s),
            TryOpen::NoPath => {}
            TryOpen::Failed => *state = State::Disabled,
        }
    }
    match state {
        State::Ready(_) => true,
        State::Uninit | State::Disabled => false,
    }
}

/// モジュール読み込み時に 1 回呼ぶ。開けたら実際のパスを返す。
pub fn init() -> Option<PathBuf> {
    let Ok(mut g) = state().lock() else {
        return None;
    };
    if !ensure(&mut g, try_open) {
        return None;
    }
    match &*g {
        State::Ready(s) => Some(s.path().to_path_buf()),
        State::Uninit | State::Disabled => None,
    }
}

/// レベル付きで 1 件書く（まだ開いていなければここで開く）。
pub fn write(level: Level, message: &str) {
    let Ok(mut g) = state().lock() else {
        return;
    };
    if !ensure(&mut g, try_open) {
        return;
    }
    if let State::Ready(s) = &mut *g {
        s.write(level, message);
    }
}

/// OBS のログレベル付きメッセージをファイルにも書く（`LOG_DEBUG` は書かない）。
pub fn mirror_from_obs(level: c_int, message: &str) {
    if let Some(l) = Level::from_obs(level) {
        write(l, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の使い捨てディレクトリ。
    fn temp_dir(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("obs_ns_log_{tag}_{n}"));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("read log")
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `YYYY-MM-DD HH:MM:SS.mmm` の形になっているか。
    fn assert_timestamp_prefix(s: &str) {
        let c: Vec<char> = s.chars().collect();
        assert!(c.len() >= 23, "短すぎます: {s:?}");
        for (i, ch) in c[..23].iter().enumerate() {
            let expected = match i {
                4 | 7 => Some('-'),
                10 => Some(' '),
                13 | 16 => Some(':'),
                19 => Some('.'),
                _ => None,
            };
            match expected {
                Some(want) => assert_eq!(*ch, want, "{i} 文字目: {s:?}"),
                None => assert!(ch.is_ascii_digit(), "{i} 文字目: {s:?}"),
            }
        }
    }

    #[test]
    fn timestamp_looks_like_local_time() {
        assert_timestamp_prefix(&timestamp());
    }

    #[test]
    fn only_error_warning_info_are_recorded() {
        assert_eq!(Level::from_obs(crate::ffi::LOG_ERROR), Some(Level::Error));
        assert_eq!(Level::from_obs(crate::ffi::LOG_WARNING), Some(Level::Warning));
        assert_eq!(Level::from_obs(crate::ffi::LOG_INFO), Some(Level::Info));
        // DEBUG はファイルに残さない。
        assert_eq!(Level::from_obs(crate::ffi::LOG_DEBUG), None);
        assert_eq!(Level::from_obs(0), None);
    }

    #[test]
    fn level_tags_are_stable() {
        assert_eq!(Level::Error.tag(), "ERROR");
        assert_eq!(Level::Warning.tag(), "WARNING");
        assert_eq!(Level::Info.tag(), "INFO");
    }

    #[test]
    fn writes_timestamp_level_and_body() {
        let dir = temp_dir("basic");
        let p = dir.join("a.log");

        let mut s = Sink::open(&p).expect("open");
        s.write(Level::Error, "テスト失敗");
        s.write(Level::Warning, "警告です");
        s.write(Level::Info, "情報です");
        drop(s);

        let text = read(&p);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text:?}");
        for l in &lines {
            assert_timestamp_prefix(l);
        }
        assert!(lines[0].ends_with(" [ERROR] テスト失敗"), "{:?}", lines[0]);
        assert!(lines[1].ends_with(" [WARNING] 警告です"), "{:?}", lines[1]);
        assert!(lines[2].ends_with(" [INFO] 情報です"), "{:?}", lines[2]);
        cleanup(&dir);
    }

    #[test]
    fn multiline_message_becomes_one_record() {
        let dir = temp_dir("multiline");
        let p = dir.join("a.log");

        let mut s = Sink::open(&p).expect("open");
        s.write(Level::Error, "1 行目\r\n2 行目\n3 行目");
        drop(s);

        let text = read(&p);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text:?}");
        assert_timestamp_prefix(lines[0]);
        assert!(lines[0].ends_with("[ERROR] 1 行目"), "{:?}", lines[0]);
        assert_eq!(lines[1], "    2 行目");
        assert_eq!(lines[2], "    3 行目");
        // 拡張子側に \r が残らないこと。
        assert!(!text.contains('\r'), "{text:?}");
        cleanup(&dir);
    }

    #[test]
    fn empty_message_is_ignored() {
        let dir = temp_dir("empty");
        let p = dir.join("a.log");

        let mut s = Sink::open(&p).expect("open");
        s.write(Level::Error, "");
        s.write(Level::Error, "\r\n");
        s.write(Level::Info, "残る 1 行");
        drop(s);

        let text = read(&p);
        assert_eq!(text.lines().count(), 1, "{text:?}");
        assert!(text.ends_with("[INFO] 残る 1 行\n"), "{text:?}");
        cleanup(&dir);
    }

    #[test]
    fn repeats_are_thinned_out() {
        let dir = temp_dir("repeat");
        let p = dir.join("a.log");

        let mut s = Sink::open(&p).expect("open");
        // 1 回 + 同じメッセージを 8 回。
        for _ in 0..9 {
            s.write(Level::Warning, "dump に書けません");
        }
        drop(s);

        let text = read(&p);
        let lines: Vec<&str> = text.lines().collect();
        // 1 行 + 追加 2 / 4 / 8 回の注記 3 行。
        assert_eq!(lines.len(), 4, "{text:?}");
        assert!(lines[0].ends_with("[WARNING] dump に書けません"), "{:?}", lines[0]);
        for (i, want) in [2, 4, 8].iter().enumerate() {
            assert!(lines[i + 1].contains(&format!("追加で {want} 回")), "{:?}", lines[i + 1]);
            assert!(lines[i + 1].contains("[INFO]"), "{:?}", lines[i + 1]);
        }
        cleanup(&dir);
    }

    #[test]
    fn different_message_breaks_the_repeat_chain() {
        let dir = temp_dir("repeat2");
        let p = dir.join("a.log");

        let mut s = Sink::open(&p).expect("open");
        // A x3 → 1 行 + 「追加で 2 回」の注記。
        s.write(Level::Error, "A");
        s.write(Level::Error, "A");
        s.write(Level::Error, "A");
        // 別のメッセージを挟むと連続が切れる（B はそのまま 1 行）。
        s.write(Level::Error, "B");
        // そのあとの A は新しいメッセージ扱いで、もう 1 行になる。
        s.write(Level::Error, "A");
        s.write(Level::Error, "A");
        drop(s);

        let text = read(&p);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text:?}");
        assert!(lines[0].ends_with("[ERROR] A"), "{:?}", lines[0]);
        assert!(lines[1].contains("追加で 2 回"), "{:?}", lines[1]);
        assert!(lines[2].ends_with("[ERROR] B"), "{:?}", lines[2]);
        assert!(lines[3].ends_with("[ERROR] A"), "{:?}", lines[3]);
        cleanup(&dir);
    }

    #[test]
    fn appends_to_the_existing_file() {
        let dir = temp_dir("append");
        let p = dir.join("a.log");

        let mut s = Sink::open(&p).expect("open");
        s.write(Level::Info, "前のセッション");
        drop(s);

        let mut s = Sink::open(&p).expect("open");
        s.write(Level::Info, "次のセッション");
        drop(s);

        let text = read(&p);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text:?}");
        assert!(lines[0].ends_with("[INFO] 前のセッション"), "{:?}", lines[0]);
        assert!(lines[1].ends_with("[INFO] 次のセッション"), "{:?}", lines[1]);
        cleanup(&dir);
    }

    #[test]
    fn oversized_file_is_rotated_to_dot_one() {
        let dir = temp_dir("rotate");
        let p = dir.join("a.log");
        let old = "x".repeat(64);
        std::fs::write(&p, &old).expect("seed");

        // 64 バイト >= 32 バイト なので退避される。
        let mut s = Sink::open_with_limits(&p, 32, SESSION_MAX_BYTES).expect("open");
        s.write(Level::Info, "新しいセッション");
        drop(s);

        assert_eq!(read(&dir.join("a.log.1")), old);
        let text = read(&p);
        assert!(text.contains(".1 に退避しました"), "{text:?}");
        assert!(text.contains("新しいセッション"), "{text:?}");
        assert!(!text.contains('x'), "{text:?}");
        cleanup(&dir);
    }

    #[test]
    fn small_file_is_not_rotated() {
        let dir = temp_dir("norotate");
        let p = dir.join("a.log");
        std::fs::write(&p, "前回のログ\n").expect("seed");

        let mut s = Sink::open_with_limits(&p, 1 << 20, SESSION_MAX_BYTES).expect("open");
        s.write(Level::Info, "続き");
        drop(s);

        assert!(!dir.join("a.log.1").exists());
        let text = read(&p);
        assert!(text.starts_with("前回のログ\n"), "{text:?}");
        assert!(text.contains("続き"), "{text:?}");
        cleanup(&dir);
    }

    #[test]
    fn session_cap_stops_writing() {
        let dir = temp_dir("cap");
        let p = dir.join("a.log");

        // 1 行も書けないほど小さい上限。
        let mut s = Sink::open_with_limits(&p, MAX_BYTES, 10).expect("open");
        s.write(Level::Error, "これは書かれない");
        s.write(Level::Error, "これも書かれない");
        drop(s);

        let text = read(&p);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text:?}");
        assert!(lines[0].contains("上限 (10 バイト)"), "{text:?}");
        assert!(!text.contains("これは書かれない"), "{text:?}");
        cleanup(&dir);
    }

    #[test]
    fn open_fails_when_the_parent_is_a_file() {
        let dir = temp_dir("badpath");
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, "not a directory").expect("seed");

        assert!(Sink::open(&blocker.join("a.log")).is_err());
        cleanup(&dir);
    }

    #[test]
    fn ensure_retries_only_while_the_path_is_missing() {
        let mut st = State::Uninit;

        assert!(!ensure(&mut st, || TryOpen::NoPath));
        assert!(matches!(st, State::Uninit), "パスが取れないだけなら再挑戦する");

        assert!(!ensure(&mut st, || TryOpen::Failed));
        assert!(matches!(st, State::Disabled), "開けなかったら諦める");

        // 諦めた後は open を呼ばない（呼べば Failed が返るはずの closure が走る）。
        assert!(!ensure(&mut st, || panic!("呼んではいけない")));
    }

    #[test]
    fn ensure_opens_once_and_keeps_the_file() {
        let dir = temp_dir("ensure");
        let p = dir.join("a.log");

        let mut st = State::Uninit;
        let target = p.clone();
        assert!(ensure(&mut st, move || {
            TryOpen::Ready(Box::new(Sink::open(&target).expect("open")))
        }));

        if let State::Ready(s) = &mut st {
            s.write(Level::Error, "1 件目");
        }
        // 2 回目は開き直さない（開き直すと Failed 側に落ちて 2 件目が消える）。
        assert!(ensure(&mut st, || TryOpen::Failed));
        if let State::Ready(s) = &mut st {
            s.write(Level::Error, "2 件目");
            assert_eq!(s.path(), p.as_path());
        } else {
            panic!("Ready のはず");
        }
        drop(st);

        let text = read(&p);
        assert!(text.contains("1 件目"), "{text:?}");
        assert!(text.contains("2 件目"), "{text:?}");
        cleanup(&dir);
    }

    /// テスト用: グローバルの書き込み先を差し替える。
    fn install_for_test(sink: Box<Sink>) {
        if let Ok(mut g) = state().lock() {
            *g = State::Ready(sink);
        }
    }

    /// テスト用: 差し替えを戻す（以降はファイルに書かない）。
    fn reset_for_test() {
        if let Ok(mut g) = state().lock() {
            *g = State::Uninit;
        }
    }

    /// `obs_log!` → OBS 標準ログ + 専用ファイル、の経路を丸ごと確認する。
    #[test]
    fn obs_log_macro_writes_the_file_and_skips_debug() {
        let dir = temp_dir("mirror");
        let p = dir.join("a.log");
        install_for_test(Box::new(Sink::open(&p).expect("open")));

        crate::obs_log!(crate::ffi::LOG_ERROR, "テスト用の失敗 {}", 7);
        crate::obs_log!(crate::ffi::LOG_WARNING, "テスト用の警告");
        crate::obs_log!(crate::ffi::LOG_INFO, "テスト用の情報");
        crate::obs_log!(crate::ffi::LOG_DEBUG, "デバッグはファイルに出ない");

        reset_for_test();

        // 他のテストも同時にログを出すので、行数ではなく内容で見る。
        let text = read(&p);
        assert!(text.contains("[ERROR] テスト用の失敗 7"), "{text:?}");
        assert!(text.contains("[WARNING] テスト用の警告"), "{text:?}");
        assert!(text.contains("[INFO] テスト用の情報"), "{text:?}");
        assert!(!text.contains("デバッグはファイルに出ない"), "{text:?}");
        for l in text.lines() {
            assert_timestamp_prefix(l);
        }
        cleanup(&dir);
    }
}
