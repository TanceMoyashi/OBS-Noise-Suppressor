//! ログの受け口。ホスト（OBS / VST3 / テスト）が差し込む。
//!
//! このクレートは libobs も nih-plug も知らないので、自分のログはここへ流す。
//! 出力先が設定されていなければ**黙って捨てる**（単体テストはこれで静かに走る）。
//! どのレベルを残すかはホスト側の判断（OBS 版は専用ファイルにも書く）。

use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Info,
    Warning,
    Error,
}

type Sink = fn(Level, &str);

static SINK: OnceLock<Sink> = OnceLock::new();

/// 出力先を設定する。**最初の 1 回だけ**効く（2 回目以降は無視する）。
pub fn set_sink(f: Sink) {
    let _ = SINK.set(f);
}

/// 出力先があれば渡す。渡したら `true`。
///
/// 使わない戻り値で `must_use` 警告を出さないよう `dsp_log!` は戻り値を捨てる。
#[must_use]
pub fn emit(level: Level, msg: &str) -> bool {
    match SINK.get() {
        Some(sink) => {
            sink(level, msg);
            true
        }
        None => false,
    }
}

/// `dsp_log!(Level::Warning, "読めない: {}", e)` の形で使う。
#[macro_export]
macro_rules! dsp_log {
    ($level:expr, $($arg:tt)*) => {{
        let _ = $crate::log::emit($level, &::std::format!($($arg)*));
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static MSGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn collecting_sink(level: Level, msg: &str) {
        MSGS.lock()
            .unwrap()
            .push(format!("{level:?}:{msg}"));
    }

    /// シンクはプロセス全体で 1 回だけ設定できるので、テストは 1 本にまとめる。
    /// 設定後は他のテストのログも届くので、件数ではなく「入っていること」を見る。
    #[test]
    fn a_sink_receives_what_the_core_logs() {
        // 設定する前は捨てられる（シンクを設定するのはこのテストだけ）。
        assert!(!emit(Level::Info, "シンクを設定する前"));

        set_sink(collecting_sink);
        assert!(emit(Level::Warning, "設定したあと"));
        dsp_log!(Level::Error, "{} {}", "マクロ", 1);

        let msgs = MSGS.lock().unwrap();
        assert!(msgs.iter().any(|m| m == "Warning:設定したあと"), "{msgs:?}");
        assert!(msgs.iter().any(|m| m == "Error:マクロ 1"), "{msgs:?}");
    }
}
