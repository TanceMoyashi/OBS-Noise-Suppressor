//! プリセット（番号スロット 1〜8）。
//!
//! 保存先は OBS のプラグイン設定ディレクトリ
//! (`%APPDATA%\obs-studio\plugin_config\obs_noise_suppressor\presets.json`)。
//! シーンコレクションとは別なので、フィルタをまたいで使い回せる。
//!
//! 依存を増やさないため JSON は自前で書く / 読む。読む方は壊れた行を
//! 飛ばすだけで、例外も panic もしない（読めなかったスロットは空になる）。
//!
//! スロットに入れるのは**音に関わる設定**（AMBIENCE / VOICE / REVERB と
//! モデル）。VU の基準値はメーターの表示設定なので含めない。

use crate::dsp::params;
use crate::paths;

/// スロット数（パネルの MEMORY 1〜8）。
pub const SLOTS: usize = 8;

/// 設定ファイル名（`obs_module_config_path` に渡す名前）。
pub const FILE_NAME: &str = "presets.json";

/// 1 スロットぶんの設定。
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub ambience: f32,
    pub voice_db: f32,
    pub reverb: f32,
    /// `paths::ModelChoice::as_str()` の値。
    pub model: String,
}

impl Default for Preset {
    fn default() -> Self {
        Self {
            ambience: params::AMBIENCE_DEFAULT as f32,
            voice_db: params::VOICE_DEFAULT as f32,
            reverb: params::VOICE_REVERB_DEFAULT as f32,
            model: paths::ModelChoice::default().as_str().to_string(),
        }
    }
}

impl Preset {
    pub fn model_choice(&self) -> paths::ModelChoice {
        paths::ModelChoice::from_setting(&self.model)
    }
}

// --------------------------------------------------------------------------
// JSON（この形だけ扱う最小実装）
// --------------------------------------------------------------------------

/// スロット配列を JSON にする。空きスロットは `null`。
pub fn render(slots: &[Option<Preset>; SLOTS]) -> String {
    let num = |v: f32| -> String {
        // f32 に十分な桁（表示は短く保つ）。
        let s = format!("{v:.6}");
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        if s.is_empty() || s == "-" {
            "0".to_string()
        } else {
            s
        }
    };

    let mut out = String::from("{\n  \"version\": 1,\n  \"slots\": [\n");
    for (i, slot) in slots.iter().enumerate() {
        out.push_str("    ");
        match slot {
            Some(p) => {
                out.push_str(&format!(
                    "{{\"ambience\": {}, \"voice_db\": {}, \"reverb\": {}, \"model\": \"{}\"}}",
                    num(p.ambience),
                    num(p.voice_db),
                    num(p.reverb),
                    p.model
                ));
            }
            None => out.push_str("null"),
        }
        if i + 1 < SLOTS {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

/// JSON を読む。読めない部分は無視して空きスロットにする。
pub fn parse(text: &str) -> [Option<Preset>; SLOTS] {
    let mut out: [Option<Preset>; SLOTS] = Default::default();
    let Some(inner) = array_body(text, "slots") else {
        return out;
    };

    for (i, elem) in split_top_level(inner).into_iter().enumerate() {
        if i >= SLOTS {
            break;
        }
        out[i] = parse_slot(elem);
    }
    out
}

/// `"key"` の後ろにある配列の中身（`[` と対応する `]` の間）を返す。
fn array_body<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let kpos = find_key(text, key)?;
    let bytes = text.as_bytes();
    let mut i = kpos;
    while i < bytes.len() && bytes[i] != b'[' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let start = i + 1;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'[' | b'{' => depth += 1,
                b']' | b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return text.get(start..i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// `"key"` の直後の `:` の位置（`"slots"` の終わりの次）を返す。
fn find_key(text: &str, key: &str) -> Option<usize> {
    let pat = format!("\"{key}\"");
    let pos = text.find(&pat)?;
    let mut i = pos + pat.len();
    let bytes = text.as_bytes();
    while i < bytes.len() && bytes[i] != b':' {
        i += 1;
    }
    Some(i)
}

/// 配列の中身をトップレベルの `,` で分ける（`{}` の中と文字列の中は無視）。
fn split_top_level(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;

    for (i, &c) in bytes.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// 1 要素を読む。`{...}` でなければ（`null` など）空きスロット。
fn parse_slot(elem: &str) -> Option<Preset> {
    if !elem.contains('{') {
        return None;
    }
    let mut p = Preset::default();
    let mut any = false;

    if let Some(v) = get_number(elem, "ambience") {
        p.ambience = v as f32;
        any = true;
    }
    if let Some(v) = get_number(elem, "voice_db").or_else(|| get_number(elem, "voice")) {
        p.voice_db = v as f32;
        any = true;
    }
    if let Some(v) = get_number(elem, "reverb").or_else(|| get_number(elem, "voice_reverb")) {
        p.reverb = v as f32;
        any = true;
    }
    if let Some(v) = get_string(elem, "model") {
        p.model = v.to_string();
    }

    if any {
        Some(p)
    } else {
        None
    }
}

/// `"key"` の後ろの数値を読む。
fn get_number(text: &str, key: &str) -> Option<f64> {
    let v = get_value(text, key)?;
    v.parse::<f64>().ok()
}

/// `"key"` の後ろの文字列（`"` で囲まれた中身）を読む。
fn get_string<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let v = get_value(text, key)?;
    let v = v.strip_prefix('"')?;
    let end = v.find('"')?;
    Some(&v[..end])
}

/// `"key"` に続く値のトークン（`,` `}` の手前まで、空白を詰める）。
fn get_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\"");
    let pos = text.find(&pat)?;
    let rest = &text[pos + pat.len()..];
    let colon = rest.find(':')?;
    let rest = rest[colon + 1..].trim_start();
    // 文字列は閉じ引用符まで、それ以外は区切り文字まで。
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        return rest.get(..end + 2);
    }
    let end = rest
        .find(|c: char| c == ',' || c == '}' || c == '\n' || c == ']')
        .unwrap_or(rest.len());
    Some(rest[..end].trim())
}

// --------------------------------------------------------------------------
// ファイル入出力
// --------------------------------------------------------------------------

/// 保存先のパス（`%APPDATA%\obs-studio\plugin_config\obs_noise_suppressor\presets.json`）。
pub fn path() -> Option<std::path::PathBuf> {
    paths::config_file(FILE_NAME)
}

/// 読み込む。ファイルが無い / 壊れている場合は空のスロットを返す。
pub fn load() -> [Option<Preset>; SLOTS] {
    let Some(p) = path() else {
        return Default::default();
    };
    match std::fs::read_to_string(&p) {
        Ok(s) => parse(&s),
        Err(_) => Default::default(),
    }
}

/// 書き込む。壊れたファイルを残さないよう、一時ファイル経由で置き換える。
pub fn store(slots: &[Option<Preset>; SLOTS]) -> Result<(), String> {
    let p = path().ok_or_else(|| "設定ディレクトリを解決できません".to_string())?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, render(slots)).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Preset {
        Preset {
            ambience: 0.62,
            voice_db: 2.5,
            reverb: 0.28,
            model: "dp8".to_string(),
        }
    }

    #[test]
    fn round_trip() {
        let mut slots: [Option<Preset>; SLOTS] = Default::default();
        slots[0] = Some(sample());
        slots[4] = Some(Preset {
            ambience: 0.0,
            voice_db: -12.0,
            reverb: 1.0,
            model: "dp2".to_string(),
        });

        let text = render(&slots);
        let back = parse(&text);

        let a = back[0].clone().expect("slot 1");
        assert!((a.ambience - 0.62).abs() < 1e-5);
        assert!((a.voice_db - 2.5).abs() < 1e-5);
        assert!((a.reverb - 0.28).abs() < 1e-5);
        assert_eq!(a.model, "dp8");

        let b = back[4].clone().expect("slot 5");
        assert_eq!(b.voice_db, -12.0);
        assert_eq!(b.model, "dp2");

        assert!(back[1].is_none());
        assert!(back[7].is_none());
    }

    #[test]
    fn parse_tolerates_garbage() {
        assert!(parse("").iter().all(|s| s.is_none()));
        assert!(parse("{").iter().all(|s| s.is_none()));
        assert!(parse("{\"slots\": [").iter().all(|s| s.is_none()));
        assert!(parse("{\"slots\": [null, null]}")[0].is_none());
        assert!(parse("not json at all").iter().all(|s| s.is_none()));
    }

    #[test]
    fn parse_partial_slot_uses_defaults() {
        let slots = parse("{\"slots\": [{\"ambience\": 0.25}, null]}");
        let p = slots[0].clone().expect("slot 1");
        assert!((p.ambience - 0.25).abs() < 1e-6);
        assert_eq!(p.voice_db, params::VOICE_DEFAULT as f32);
        assert_eq!(p.model, "auto");
    }

    #[test]
    fn parse_ignores_extra_slots_and_keys() {
        let slots = parse(
            "{\"slots\": [{\"ambience\": 1, \"unknown\": [1,2,3], \"model\": \"dp2\"}], \"x\": 1}",
        );
        let p = slots[0].clone().expect("slot 1");
        assert_eq!(p.model, "dp2");
        assert!((p.ambience - 1.0).abs() < 1e-6);
    }

    #[test]
    fn render_is_stable() {
        let mut slots: [Option<Preset>; SLOTS] = Default::default();
        slots[0] = Some(sample());
        let text = render(&slots);
        // 1 行目は version、2 行目からスロット。
        assert!(text.contains("\"version\": 1"));
        assert_eq!(text.matches("null").count(), SLOTS - 1);
        assert_eq!(parse(&text), parse(&render(&parse(&text))));
    }
}
