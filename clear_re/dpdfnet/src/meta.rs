//! DPDFNet の ONNX metadata から読み出した、STFT と状態管理に必要なパラメータ。
//!
//! 移植元: `clear_re/plugin/src/ModelMeta.cpp`。
//!
//! metadata のキー（公式エクスポートが埋め込む）:
//! `n_fft, hop_length, window_length, window_type, normalized, center, pad_mode,`
//! `freq_bins, erb_bins, spec_bins, state_size,`
//! `erb_norm_state_size, spec_norm_state_size, erb_norm_init, spec_norm_init,`
//! `sample_rate, model_type, version, profile`

/// metadata を引くためのコールバック。見つからなければ `None`。
pub type MetaLookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

#[derive(Debug, Clone)]
pub struct ModelMeta {
    pub valid: bool,
    pub model_type: String,
    pub profile: String,
    pub sample_rate: i32,
    pub n_fft: i32,
    pub hop: i32,
    pub win_len: i32,
    pub freq_bins: i32,
    pub erb_bins: i32,
    pub spec_bins: i32,
    pub state_size: i32,
    pub erb_norm_state_size: i32,
    pub spec_norm_state_size: i32,
    pub centre: bool,
    pub window_type: String,
    pub erb_norm_init: Vec<f32>,
    pub spec_norm_init: Vec<f32>,
}

impl Default for ModelMeta {
    fn default() -> Self {
        Self {
            valid: false,
            model_type: String::new(),
            profile: String::new(),
            sample_rate: 48000,
            n_fft: 960,
            hop: 480,
            win_len: 960,
            freq_bins: 481,
            erb_bins: 32,
            spec_bins: 96,
            state_size: 0,
            erb_norm_state_size: 0,
            spec_norm_state_size: 0,
            centre: true,
            window_type: String::new(),
            erb_norm_init: Vec::new(),
            spec_norm_init: Vec::new(),
        }
    }
}

impl ModelMeta {
    pub fn from_lookup(get: &MetaLookup<'_>) -> Self {
        let mut r = Self::default();

        // DPDFNet のストリーミング・エクスポートでなければ即座に無効とする。
        let Some(model_type) = get("model_type") else {
            return r;
        };
        r.model_type = model_type;
        r.profile = get("profile").unwrap_or_default();

        let get_int = |key: &str, fallback: i32| -> i32 {
            match get(key) {
                Some(s) if !s.is_empty() => s.trim().parse::<i32>().unwrap_or(fallback),
                _ => fallback,
            }
        };

        r.sample_rate = get_int("sample_rate", 48000);
        r.n_fft = get_int("n_fft", 960);
        r.hop = get_int("hop_length", 480);
        r.win_len = get_int("window_length", r.n_fft);
        r.freq_bins = get_int("freq_bins", r.n_fft / 2 + 1);
        r.erb_bins = get_int("erb_bins", 32);
        r.spec_bins = get_int("spec_bins", 96);
        r.state_size = get_int("state_size", 0);
        r.erb_norm_state_size = get_int("erb_norm_state_size", 0);
        r.spec_norm_state_size = get_int("spec_norm_state_size", 0);
        r.centre = get_int("center", 1) != 0;
        r.window_type = get("window_type").unwrap_or_default();

        r.erb_norm_init = get("erb_norm_init").map(|s| parse_float_list(&s)).unwrap_or_default();
        r.spec_norm_init = get("spec_norm_init").map(|s| parse_float_list(&s)).unwrap_or_default();

        // 公式スクリプトと同じ健全性チェック。
        let ok = r.window_type == "vorbis"
            && r.win_len == r.n_fft
            && r.hop == r.win_len / 2
            && r.freq_bins == r.n_fft / 2 + 1
            && r.state_size > 0
            && r.erb_norm_init.len() as i32 == r.erb_norm_state_size
            && r.spec_norm_init.len() as i32 == r.spec_norm_state_size
            && r.erb_norm_state_size + r.spec_norm_state_size <= r.state_size;

        if !ok {
            return r;
        }

        r.valid = true;
        r
    }

    /// 初期状態ベクトル。先頭に ERB 正規化状態、続けてスペクトル正規化状態を置く。
    pub fn make_initial_state(&self) -> Vec<f32> {
        let mut s = vec![0.0f32; self.state_size.max(0) as usize];
        if !self.valid {
            return s;
        }

        for (i, v) in self.erb_norm_init.iter().enumerate() {
            if i < s.len() {
                s[i] = *v;
            }
        }

        let off = self.erb_norm_state_size.max(0) as usize;
        for (i, v) in self.spec_norm_init.iter().enumerate() {
            if off + i < s.len() {
                s[off + i] = *v;
            }
        }

        s
    }
}

/// `"0.1,0.2,..."` をパースする（移植元の `getFloatList` と同じ）。
fn parse_float_list(s: &str) -> Vec<f32> {
    s.split(',')
        .filter(|t| !t.is_empty())
        .map(|t| t.trim().parse::<f32>().unwrap_or(0.0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_with(pairs: &[(&str, &str)]) -> ModelMeta {
        let owned: Vec<(String, String)> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        ModelMeta::from_lookup(&|key: &str| {
            owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        })
    }

    #[test]
    fn rejects_non_dpdfnet_metadata() {
        let m = meta_with(&[]);
        assert!(!m.valid);
    }

    #[test]
    fn accepts_dpdfnet_metadata_and_builds_initial_state() {
        let m = meta_with(&[
            ("model_type", "dpdfnet"),
            ("n_fft", "960"),
            ("hop_length", "480"),
            ("window_length", "960"),
            ("window_type", "vorbis"),
            ("freq_bins", "481"),
            ("state_size", "4"),
            ("erb_norm_state_size", "2"),
            ("spec_norm_state_size", "2"),
            ("erb_norm_init", "0.25,0.5"),
            ("spec_norm_init", "0.75,1.0"),
        ]);
        assert!(m.valid);
        assert_eq!(m.make_initial_state(), vec![0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn rejects_wrong_window_type() {
        let m = meta_with(&[
            ("model_type", "dpdfnet"),
            ("n_fft", "960"),
            ("hop_length", "480"),
            ("window_length", "960"),
            ("window_type", "hann"),
            ("freq_bins", "481"),
            ("state_size", "4"),
            ("erb_norm_state_size", "2"),
            ("spec_norm_state_size", "2"),
            ("erb_norm_init", "0.25,0.5"),
            ("spec_norm_init", "0.75,1.0"),
        ]);
        assert!(!m.valid);
    }
}
