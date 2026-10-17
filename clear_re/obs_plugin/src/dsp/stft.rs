//! DPDFNet 用のストリーミング STFT / iSTFT。
//!
//! 移植元: `clear_re/plugin/src/VorbisStft.cpp`（kissfft 版）。
//! 仕様（公式エクスポートの metadata と `onnx_model/infer_dpdfnet_onnx.py` に一致）:
//!
//!   * n_fft == win_len == 960, hop == win_len / 2 == 480
//!   * 窓は Vorbis: `w[i] = sin(pi/2 * sin^2(pi*(i+0.5)/(win/2)))`
//!   * center == true (reflect) 相当。フレーム t は入力 `[(t-1)*hop, (t+1)*hop)` を使う
//!   * 生の複素スペクトルをそのまま ONNX に渡す（wnorm は外から掛けない）
//!   * iSTFT は窓包絡線で正規化する WOLA
//!
//! kissfft の代わりに `realfft`（rustfft 系）を使う。n_fft = 960 = 2^6*3*5 の
//! 混合基数を扱える点と、非正規化の実 FFT という規約が kissfft と同じなので
//! 出力は移植元と一致する（forward はスケーリング無し、inverse は N 倍）。

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use std::sync::Arc;

const PI: f64 = std::f64::consts::PI;

/// Vorbis 窓。`half = win_len / 2`。
///
/// 50 % オーバーラップで `w[i]^2 + w[i+hop]^2 == 1`（厳密に Princen-Bradley）になる。
fn fill_vorbis_window(w: &mut [f32], win_len: usize) {
    let half = win_len as f64 / 2.0;
    for (i, v) in w.iter_mut().enumerate() {
        let s = (0.5 * PI * (i as f64 + 0.5) / half).sin();
        *v = (0.5 * PI * s * s).sin() as f32;
    }
}

pub struct VorbisStft {
    n_fft: usize,
    win_len: usize,
    hop: usize,
    freq_bins: usize,

    window: Vec<f32>,
    /// WOLA の包絡線 `env[m] = w[m]^2 + w[m+hop]^2`（長さ hop）。
    env: Vec<f32>,

    in_buf: Vec<f32>,
    wbuf: Vec<f32>,
    acc: Vec<f32>,
    tbuf: Vec<f32>,

    spec: Vec<Complex32>,
    fwd_scratch: Vec<Complex32>,
    inv_scratch: Vec<Complex32>,

    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,

    first_synthesis: bool,
    valid: bool,
}

impl VorbisStft {
    /// 何も準備していない（`valid == false`）状態で作る。
    pub fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        // ダミーの 2 点プラン。prepare() で差し替える。
        let fwd = planner.plan_fft_forward(2);
        let inv = planner.plan_fft_inverse(2);
        Self {
            n_fft: 0,
            win_len: 0,
            hop: 0,
            freq_bins: 0,
            window: Vec::new(),
            env: Vec::new(),
            in_buf: Vec::new(),
            wbuf: Vec::new(),
            acc: Vec::new(),
            tbuf: Vec::new(),
            spec: Vec::new(),
            fwd_scratch: Vec::new(),
            inv_scratch: Vec::new(),
            fwd,
            inv,
            first_synthesis: true,
            valid: false,
        }
    }

    /// DPDFNet の 48 kHz HR モデルは `n_fft == win_len == 960, hop == win_len/2` を
    /// 要求する。異なる値なら失敗させ、中途半端に動かさない。
    pub fn prepare(&mut self, n_fft: usize, win_len: usize, hop: usize) -> bool {
        self.valid = false;

        if win_len != n_fft || n_fft == 0 || hop != win_len / 2 || n_fft % 2 != 0 {
            return false;
        }

        self.n_fft = n_fft;
        self.win_len = win_len;
        self.hop = hop;
        self.freq_bins = n_fft / 2 + 1;

        let mut planner = RealFftPlanner::<f32>::new();
        self.fwd = planner.plan_fft_forward(n_fft);
        self.inv = planner.plan_fft_inverse(n_fft);

        self.window = vec![0.0; win_len];
        fill_vorbis_window(&mut self.window, win_len);

        // Vorbis 窓は hop = win_len/2 で厳密に env == 1 になるが、一般性と
        // 数値検証のために実際の和で割る（移植元と同じ）。
        self.env = vec![0.0; hop];
        for m in 0..hop {
            let a = self.window[m];
            let b = self.window[m + hop];
            self.env[m] = a * a + b * b;
        }

        self.in_buf = vec![0.0; win_len];
        self.wbuf = vec![0.0; win_len];
        self.acc = vec![0.0; win_len + hop];
        self.tbuf = vec![0.0; n_fft];
        self.spec = vec![Complex32::new(0.0, 0.0); self.freq_bins];
        self.fwd_scratch = vec![Complex32::new(0.0, 0.0); self.fwd.get_scratch_len()];
        self.inv_scratch = vec![Complex32::new(0.0, 0.0); self.inv.get_scratch_len()];

        self.reset();
        self.valid = true;
        true
    }

    /// テスト（完全再構成の確認）用。
    #[cfg(test)]
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    #[cfg(test)]
    pub fn freq_bins(&self) -> usize {
        self.freq_bins
    }

    pub fn reset(&mut self) {
        self.in_buf.iter_mut().for_each(|v| *v = 0.0);
        self.acc.iter_mut().for_each(|v| *v = 0.0);
        self.first_synthesis = true;
    }

    /// hop 個の入力 -> 1 フレームの複素スペクトル（re, im の交互、長さ 2*freq_bins）。
    pub fn analyse(&mut self, hop_in: &[f32], frame_interleaved: &mut [f32]) {
        if !self.valid {
            frame_interleaved.iter_mut().for_each(|v| *v = 0.0);
            return;
        }

        // 先頭 hop を捨てて新しい hop を末尾に足す（center=true 相当の 1 フレーム）。
        self.in_buf.copy_within(self.hop..self.win_len, 0);
        self.in_buf[self.win_len - self.hop..].copy_from_slice(&hop_in[..self.hop]);

        for i in 0..self.win_len {
            self.wbuf[i] = self.in_buf[i] * self.window[i];
        }

        // 非正規化の実 FFT。freq_bins 個の (re, im)。
        if self
            .fwd
            .process_with_scratch(&mut self.wbuf, &mut self.spec, &mut self.fwd_scratch)
            .is_err()
        {
            frame_interleaved.iter_mut().for_each(|v| *v = 0.0);
            return;
        }

        for k in 0..self.freq_bins {
            frame_interleaved[2 * k] = self.spec[k].re;
            frame_interleaved[2 * k + 1] = self.spec[k].im;
        }
    }

    /// 1 フレームの複素スペクトル -> hop 個の出力。
    ///
    /// 最初の 1 回はオーバーラップ加算バッファを捨てるだけで出力しない
    /// （移植元と同じ。戻り値 `false`）。
    pub fn synthesise(&mut self, frame_interleaved: &[f32], hop_out: &mut [f32]) -> bool {
        if !self.valid {
            hop_out.iter_mut().for_each(|v| *v = 0.0);
            return false;
        }

        for k in 0..self.freq_bins {
            self.spec[k] = Complex32::new(frame_interleaved[2 * k], frame_interleaved[2 * k + 1]);
        }

        // irfft と同じく DC とナイキストの虚部は無視される。
        self.spec[0].im = 0.0;
        self.spec[self.freq_bins - 1].im = 0.0;

        if self
            .inv
            .process_with_scratch(&mut self.spec, &mut self.tbuf, &mut self.inv_scratch)
            .is_err()
        {
            hop_out.iter_mut().for_each(|v| *v = 0.0);
            return false;
        }

        let inv = 1.0 / self.n_fft as f32;
        for i in 0..self.win_len {
            self.tbuf[i] *= inv * self.window[i];
        }

        let emit_now = !self.first_synthesis;

        if self.first_synthesis {
            self.acc.iter_mut().for_each(|v| *v = 0.0);
            self.first_synthesis = false;
        } else {
            // hop だけ進める。acc[0..hop) は前回放出済みなので捨ててよい。
            self.acc.copy_within(self.hop..self.win_len + self.hop, 0);
            self.acc[self.win_len..].iter_mut().for_each(|v| *v = 0.0);
        }

        for i in 0..self.win_len {
            self.acc[i] += self.tbuf[i];
        }

        if !emit_now {
            hop_out.iter_mut().for_each(|v| *v = 0.0);
            return false;
        }

        for m in 0..self.hop {
            let e = self.env[m];
            hop_out[m] = if e > 1.0e-8 { self.acc[m] / e } else { 0.0 };
        }

        true
    }
}

impl Default for VorbisStft {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsupported_geometry() {
        let mut s = VorbisStft::new();
        // hop は win_len/2 でなければならない
        assert!(!s.prepare(960, 960, 256));
        // 奇数長は扱わない
        assert!(!s.prepare(961, 961, 480));
        // 偶数長 / hop = win/2 は任意に受け付ける（DPDFNet は 960 を使う）
        assert!(s.prepare(1024, 1024, 512));
        assert!(s.is_valid());
        assert!(s.prepare(960, 960, 480));
        assert!(s.is_valid());
    }

    #[test]
    fn window_is_princen_bradley() {
        let mut s = VorbisStft::new();
        assert!(s.prepare(960, 960, 480));
        for m in 0..480 {
            let e = s.window[m] * s.window[m] + s.window[m + 480] * s.window[m + 480];
            assert!((e - 1.0).abs() < 1e-5, "env[{m}] = {e}");
        }
        // env 配列も同じ値
        for m in 0..480 {
            assert!((s.env[m] - 1.0).abs() < 1e-5);
        }
    }

    /// 分析 -> そのまま合成 で、2 フレーム目以降は入力に戻る（完全再構成）。
    #[test]
    fn perfect_reconstruction_after_overlap() {
        let n = 960;
        let hop = 480;
        let mut s = VorbisStft::new();
        assert!(s.prepare(n, n, hop));

        let mut frame = vec![0.0f32; 2 * s.freq_bins()];
        let mut out = vec![0.0f32; hop];

        let mut produced: Vec<f32> = Vec::new();
        let mut fed: Vec<f32> = Vec::new();

        // 5 ホップ分のホワイトノイズ的入力（決定的な擬似乱数）
        let mut seed: u32 = 0x1234_5678;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / 8_388_608.0 - 1.0
        };

        for _ in 0..5 {
            let hop_in: Vec<f32> = (0..hop).map(|_| next()).collect();
            fed.extend_from_slice(&hop_in);

            s.analyse(&hop_in, &mut frame);
            if s.synthesise(&frame, &mut out) {
                produced.extend_from_slice(&out);
            }
        }

        // 最初の合成は捨てられ、以降は元の入力と一致するはず。
        // 放出 hop s は入力 hop (s-1) に対応するので、produced[0] は fed[0..hop)。
        assert!(produced.len() >= 2 * hop);
        let mut max_err = 0.0f32;
        for i in 0..produced.len() {
            let e = (produced[i] - fed[i]).abs();
            if e > max_err {
                max_err = e;
            }
        }
        assert!(max_err < 1e-4, "max_err = {max_err}");
    }
}
