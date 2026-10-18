//! 有理比ポリフェーズ・リサンプラ（OBS のレート ↔ 48 kHz）。
//!
//! モデルは 48 kHz 前提なので、OBS が 44.1 / 96 / 192 kHz などで動いている
//! ときは「入力 → 48 kHz → モデル → 元のレート」と挟む。その変換器がこれ。
//!
//! 設計:
//!   * 比は `rate_out / rate_in = up / down`（既約）で厳密に表す。
//!     ポリフェーズ分解により位相の端数は出ない（補間誤差なし）。
//!   * 係数はカイザー窓つき sinc。阻止域の開始を `1/(2*max(up,down))`
//!     （= 低いほうのナイキスト）に合わせる。
//!   * 音声スレッドでアロケーションしない（履歴・作業領域は構築時に確保）。
//!   * 出力サンプル数は入力から一意に決まる（`floor((N-1)*up/down)+1`）。
//!     入出力を 1:1 で使う側は、端数を待ち行列で吸収する。

use std::sync::Arc;

/// 比率の上限。これを超えるレートは異常として断る（呼び出し側はパススルー）。
pub const MAX_RATIO: usize = 1024;
/// 1 位相あたりのタップ数の上限（メモリ保護）。
const MAX_TAPS: usize = 4096;
/// 阻止域の減衰量 [dB]。カイザー窓の設計目標。
const STOPBAND_DB: f64 = 90.0;
/// 通過域端を阻止域端の何倍にするか（遷移帯の広さ）。
const PASSBAND_RATIO: f64 = 0.90;

/// `rate` を 48 kHz と相互変換できるか（できないレートはパススルーする）。
pub fn supported(rate: u32) -> bool {
    let internal = super::INTERNAL_SAMPLE_RATE as u32;
    if rate == 0 {
        return false;
    }
    if rate == internal {
        return true;
    }
    Coeffs::for_rates(rate, internal).is_some() && Coeffs::for_rates(internal, rate).is_some()
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.max(1)
}

/// 0 次変形ベッセル関数（カイザー窓用）。
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0f64;
    let mut term = 1.0f64;
    let half = x / 2.0;
    for k in 1..64 {
        term *= (half / k as f64) * (half / k as f64);
        sum += term;
        if term < 1e-17 * sum {
            break;
        }
    }
    sum
}

/// レート変換の係数（全チャンネルで共有する）。
pub struct Coeffs {
    up: usize,
    down: usize,
    taps: usize,
    /// `h[phase * taps + j]` = 位相 `phase` で `j` 番目に古い入力に掛ける係数。
    /// 内積がそのまま書けるよう逆順で入れてある。
    h: Box<[f32]>,
    delay: f64,
}

impl Coeffs {
    /// `rate_in` → `rate_out` の変換係数。比が大きすぎるレートは `None`。
    pub fn for_rates(rate_in: u32, rate_out: u32) -> Option<Arc<Coeffs>> {
        if rate_in == 0 || rate_out == 0 || rate_in == rate_out {
            return None;
        }
        let g = gcd(u64::from(rate_in), u64::from(rate_out));
        let up = (u64::from(rate_out) / g) as usize;
        let down = (u64::from(rate_in) / g) as usize;
        if up == 0 || down == 0 || up > MAX_RATIO || down > MAX_RATIO {
            return None;
        }

        // 阻止域端 = 低いほうのナイキスト。
        let m = up.max(down) as f64;
        let fc = 1.0 / (2.0 * m);
        let fp = PASSBAND_RATIO * fc;
        let dw = std::f64::consts::TAU * (fc - fp);
        let want = ((STOPBAND_DB - 8.0) / (2.285 * dw)).ceil() as usize;
        let taps = want.div_ceil(up).max(2);
        if taps > MAX_TAPS {
            return None;
        }
        let len = taps * up;
        let beta = 0.1102 * (STOPBAND_DB - 8.7);
        let center = (len - 1) as f64 / 2.0;
        let gain = up as f64;

        let mut proto = vec![0.0f64; len];
        let mut norm = 0.0f64;
        for (k, p) in proto.iter_mut().enumerate() {
            let x = k as f64 - center;
            let sinc = if x.abs() < 1e-12 {
                2.0 * fc
            } else {
                (std::f64::consts::TAU * fc * x).sin() / (std::f64::consts::PI * x)
            };
            let u = 2.0 * k as f64 / (len - 1).max(1) as f64 - 1.0;
            let w = bessel_i0(beta * (1.0 - u * u).max(0.0).sqrt()) / bessel_i0(beta);
            *p = sinc * w;
            norm += *p;
        }
        // DC 利得をちょうど `up` に合わせる（振幅を保つ）。
        if norm.abs() > 1e-12 {
            for p in proto.iter_mut() {
                *p *= gain / norm;
            }
        }

        // ポリフェーズ分解（逆順）。
        let mut h = vec![0.0f32; len];
        for phase in 0..up {
            for j in 0..taps {
                h[phase * taps + j] = proto[phase + (taps - 1 - j) * up] as f32;
            }
        }

        Some(Arc::new(Coeffs {
            up,
            down,
            taps,
            h: h.into_boxed_slice(),
            delay: (len - 1) as f64 / (2.0 * up as f64),
        }))
    }

    /// `n` サンプル入れたときに出せる出力の最大数（作業領域の見積もり用）。
    pub fn max_out(&self, n_in: usize) -> usize {
        n_in / self.down.max(1) * self.up + self.up + 2
    }

    /// 群遅延（入力サンプル数）。
    pub fn delay_in_samples(&self) -> f64 {
        self.delay
    }
}

/// 1 チャンネルぶんの変換器の状態。
///
/// `push` と `pull` を交互に呼ぶ（1 ブロック = push → pull）。履歴は
/// 「フィルタ長 + 直前の pull 以降に積んだ入力数」だけ保持すれば足りる。
pub struct Stream {
    c: Arc<Coeffs>,
    buf: Box<[f32]>,
    /// `buf[0]` が表す絶対サンプル番号。
    base: i64,
    /// `buf[0..len]` が有効。
    len: usize,
    /// 次に出す出力の位置（`m / up` = 入力位置、`m % up` = 位相）。
    m: u64,
    /// 直前の `pull` 以降に積んだ入力数（保持量の判断に使う）。
    pending_in: usize,
}

/// 1 回の `push` で想定する最大サンプル数（これを超えても壊れないが、
/// 履歴が足りない分はゼロとして扱う）。レート変換を使う側の
/// 1 チャンク長の上限でもある。
pub const MAX_BLOCK: usize = 4096;

impl Stream {
    pub fn new(c: Arc<Coeffs>) -> Self {
        let taps = c.taps;
        let cap = taps + MAX_BLOCK + 8;
        Self {
            c,
            buf: vec![0.0f32; cap].into_boxed_slice(),
            // 先頭は入力前のゼロ（フィルタの初期過渡）として扱う。
            base: -(taps as i64),
            len: taps,
            m: 0,
            pending_in: 0,
        }
    }

    pub fn reset(&mut self) {
        let taps = self.c.taps;
        self.buf.iter_mut().for_each(|v| *v = 0.0);
        self.base = -(taps as i64);
        self.len = taps;
        self.m = 0;
        self.pending_in = 0;
    }

    /// 入力を積む。呼び出し側は「1 ブロック push → それに対応する出力を pull」を
    /// 繰り返すこと（`MAX_BLOCK` を超えて積み続けると履歴が足りなくなる）。
    pub fn push(&mut self, x: &[f32]) {
        let taps = self.c.taps;
        let cap = self.buf.len();
        // いま必要なのは「直前の pull 以降に積んだ入力」+ フィルタ長。
        // それを超える古いサンプルは捨ててよい（次の出力には使われない）。
        let keep = (taps + self.pending_in).min(cap);
        if self.len + x.len() > cap {
            let drop = self.len.saturating_sub(keep);
            if drop > 0 {
                self.buf.copy_within(drop..self.len, 0);
                self.base += drop as i64;
                self.len -= drop;
            }
        }
        for &v in x {
            if self.len == cap {
                // 想定より大きなブロック。古いほうから捨てる（ゼロ扱いになる）。
                self.buf.copy_within(1..cap, 0);
                self.base += 1;
                self.len -= 1;
            }
            self.buf[self.len] = v;
            self.len += 1;
        }
        self.pending_in += x.len();
    }

    /// 出せるだけ出す。戻り値は書いたサンプル数。
    pub fn pull(&mut self, out: &mut [f32]) -> usize {
        let taps = self.c.taps;
        let up = self.c.up as u64;
        let down = self.c.down as u64;
        let produced = self.base + self.len as i64;
        let mut n = 0usize;
        while n < out.len() {
            let i = (self.m / up) as i64;
            if i >= produced {
                break; // 入力待ち
            }
            let phase = (self.m % up) as usize;
            let row = &self.c.h[phase * taps..phase * taps + taps];
            let lo = i - (taps as i64 - 1); // 窓の先頭の絶対番号
            let acc = if lo >= self.base && i < produced {
                // 通常経路: 履歴がそろっている = 連続領域として読める。
                let s = (lo - self.base) as usize;
                let win = &self.buf[s..s + taps];
                let mut a = 0.0f32;
                for (c, v) in row.iter().zip(win) {
                    a += c * v;
                }
                a
            } else {
                // 履歴が足りない（巨大ブロックで古い履歴を捨てたとき）。
                // 無い分はゼロとして扱う（音は乱れるが落ちない）。
                // `row[j]` は `x[i - (taps-1-j)]` に掛かる（通常経路と同じ）。
                let mut a = 0.0f32;
                for (j, c) in row.iter().enumerate() {
                    let idx = (i - (taps - 1 - j) as i64) - self.base;
                    if idx >= 0 && idx < self.len as i64 {
                        a += c * self.buf[idx as usize];
                    }
                }
                a
            };
            out[n] = acc;
            n += 1;
            self.m += down;
        }
        self.pending_in = 0;
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate_pairs() -> Vec<(u32, u32)> {
        vec![
            (96_000, 48_000),
            (48_000, 96_000),
            (44_100, 48_000),
            (48_000, 44_100),
            (192_000, 48_000),
            (48_000, 192_000),
            (88_200, 48_000),
        ]
    }

    #[test]
    fn ratio_is_reduced_and_delay_is_sane() {
        let c = Coeffs::for_rates(96_000, 48_000).unwrap();
        assert_eq!((c.up, c.down), (1, 2));
        assert!((10.0..400.0).contains(&(c.taps as f64)));
        let c = Coeffs::for_rates(44_100, 48_000).unwrap();
        assert_eq!((c.up, c.down), (160, 147));
        assert!(c.delay_in_samples() < 64.0, "{}", c.delay_in_samples());
    }

    /// ブロック単位で流す（実使用と同じ）。
    fn convert(c: &Arc<Coeffs>, input: &[f32], block: usize) -> Vec<f32> {
        let mut s = Stream::new(Arc::clone(c));
        let mut out = vec![0.0f32; input.len() * c.up / c.down + 8];
        let mut n = 0usize;
        let mut tmp = vec![0.0f32; c.max_out(block)];
        for chunk in input.chunks(block) {
            s.push(chunk);
            let k = s.pull(&mut tmp);
            out[n..n + k].copy_from_slice(&tmp[..k]);
            n += k;
        }
        out.truncate(n);
        out
    }

    #[test]
    fn dc_is_preserved() {
        for (rin, rout) in rate_pairs() {
            let c = Coeffs::for_rates(rin, rout).unwrap();
            let input = vec![0.5f32; rin as usize / 10];
            let out = convert(&c, &input, 4096);
            let want = input.len() as f64 * rout as f64 / rin as f64;
            assert!(
                out.len() as f64 > want * 0.98,
                "{rin}->{rout}: out={} want≈{want:.0}",
                out.len()
            );
            // 後半（定常）は 0.5 のまま。
            for v in &out[out.len() / 2..] {
                assert!((v - 0.5).abs() < 1e-3, "{rin}->{rout}: {v}");
            }
        }
    }

    #[test]
    fn count_matches_the_ratio() {
        for (rin, rout) in rate_pairs() {
            let c = Coeffs::for_rates(rin, rout).unwrap();
            let mut s = Stream::new(c);
            let total = 4800usize;
            let mut pushed = 0usize;
            let mut pulled = 0usize;
            let mut out = vec![0.0f32; 8192];
            // ブロック長をばらけさせて端数の蓄積を見る。
            for (k, blk) in [1usize, 7, 480, 1024, 333, 512].iter().cycle().enumerate() {
                if pushed >= total {
                    break;
                }
                let n = (*blk).min(total - pushed);
                s.push(&vec![0.1f32; n]);
                pushed += n;
                assert!(n <= 8192);
                pulled += s.pull(&mut out);
                let _ = k;
            }
            let expect = (total as f64 * rout as f64 / rin as f64).round() as usize;
            let diff = (pulled as i64 - expect as i64).abs();
            assert!(diff <= 2, "{rin}->{rout}: pulled={pulled} expect≈{expect}");
        }
    }

    #[test]
    fn impulse_delay_matches_theory() {
        for (rin, rout) in rate_pairs() {
            let c = Coeffs::for_rates(rin, rout).unwrap();
            let delay = c.delay_in_samples();
            let mut s = Stream::new(c);
            let mut input = vec![0.0f32; 2000];
            input[600] = 1.0;
            s.push(&input);
            let mut out = vec![0.0f32; 4096];
            let n = s.pull(&mut out);
            let peak = out[..n]
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
                .unwrap();
            let got = peak.0 as f64 * rin as f64 / rout as f64;
            assert!(
                (got - (600.0 + delay)).abs() < 2.0,
                "{rin}->{rout}: peak at {:.1}, want {:.1}",
                got,
                600.0 + delay
            );
        }
    }

    /// 阻止域（エイリアスが折り返す帯域）が十分減衰していること。
    #[test]
    fn prototype_stopband_is_deep() {
        for (rin, rout) in rate_pairs() {
            let c = Coeffs::for_rates(rin, rout).unwrap();
            // プロトタイプ（アップサンプル後）の周波数応答を直接評価する。
            // `h[phase*taps + j] = proto[phase + (taps-1-j)*up]` なので逆に戻す。
            let len = c.taps * c.up;
            let proto: Vec<f64> = (0..len)
                .map(|k| {
                    let phase = k % c.up;
                    let j = c.taps - 1 - k / c.up;
                    c.h[phase * c.taps + j] as f64
                })
                .collect();
            let m = c.up.max(c.down) as f64;
            let fc = 1.0 / (2.0 * m);
            let up = c.up as f64;
            let mag = |f: f64| {
                let mut re = 0.0;
                let mut im = 0.0;
                for (k, v) in proto.iter().enumerate() {
                    let a = std::f64::consts::TAU * f * k as f64;
                    re += v * a.cos();
                    im -= v * a.sin();
                }
                (re * re + im * im).sqrt()
            };
            // 通過域（0.8 fc）はほぼ 1、阻止域（1.05 fc 以上）は 1e-3 未満。
            let p = mag(0.8 * fc) / up;
            assert!((p - 1.0).abs() < 0.02, "{rin}->{rout}: passband {p} dc={}", mag(0.0) / up);
            for f in [1.05 * fc, 0.3, 0.45] {
                if f < 0.5 {
                    let s = mag(f) / up;
                    assert!(s < 1e-3, "{rin}->{rout}: stopband {f:.4} -> {s:.2e}");
                }
            }
        }
    }

    /// 2 倍のラウンドトリップで波形が戻ること。
    #[test]
    fn roundtrip_96k_is_transparent() {
        let down = Coeffs::for_rates(96_000, 48_000).unwrap();
        let up = Coeffs::for_rates(48_000, 96_000).unwrap();
        let delay = down.delay_in_samples() + up.delay_in_samples() * 2.0;
        let n = 9600usize;
        let x: Vec<f32> = (0..n)
            .map(|i| (std::f32::consts::TAU * 997.0 * i as f32 / 96_000.0).sin() * 0.5)
            .collect();

        let mid = convert(&down, &x, 4096);
        let out = convert(&up, &mid, 4096);
        let m = out.len();

        // 群遅延ぶんずらして比べる（端は過渡なので除く）。
        let d = delay;
        let mut err = 0.0f64;
        let mut cnt = 0usize;
        for i in 600..(m.min(n) - 600) {
            let r = i as f64 - d;
            if r < 0.0 || (r as usize) + 1 >= n {
                continue;
            }
            let j = r as usize;
            let frac = r - j as f64;
            let want = x[j] as f64 * (1.0 - frac) + x[j + 1] as f64 * frac;
            let e = out[i] as f64 - want;
            err += e * e;
            cnt += 1;
        }
        let rms = (err / cnt.max(1) as f64).sqrt();
        assert!(rms < 2e-4, "roundtrip rms error {rms:.2e}");
    }
}
