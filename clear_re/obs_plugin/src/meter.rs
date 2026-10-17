//! メーターの「針の動き」（弾道特性）。
//!
//! パネルは 40 ms ごと (`ui::TIMER_MS`) にエンジンのレベルを読み、ここへ
//! 通して表示用の値にする。Win32 に依存しないのでテストできる。
//!
//! ## VU の弾道
//!
//! 実機の VU メーター (ANSI C16.5-1942 / IEC 60268-17) は
//! 「一定振幅の正弦波を入れると 300 ms で 99 % の振れに達し、行き過ぎは
//! 1.5 % 以下」という特性を持つ。1 次遅れ 2 段（= 臨界制動の 2 次系）は
//! ちょうど行き過ぎの無い応答で、
//!
//! ```text
//! 99 % 到達時間 = 6.64 τ
//! ```
//!
//! なので `τ = 300 / 6.64 ≈ 45.2 ms` とすれば規格どおりの速さになる。
//! 実機の戻りも同じ機械的制動で決まるため、戻りも同じ時定数（対称）とする。
//!
//! **弾道はリニア値（振れそのもの）に掛ける。** 実機の可動部は電流に比例して
//! 振れるので、dB へ変換するのは弾道を通したあと（目盛り側）でよい。

/// パネルのタイマー間隔 [ms]（`ui::TIMER_MS` と一致させること）。
pub const TICK_MS: f32 = 40.0;

/// VU の時定数 [ms]。300 ms で 99 % に達する値（`6.64 τ = 300 ms`）。
pub const VU_TAU_MS: f32 = 45.2;

/// 表示の下限 [dBFS]。これ以下は「無音」として扱う。
pub const DB_FLOOR: f32 = -60.0;

/// ピークホールドの保持時間 [ms]。
pub const PEAK_HOLD_MS: f32 = 1500.0;
/// ピークホールドの減衰 [dB/s]（保持時間が過ぎてから）。
pub const PEAK_DECAY_DB_PER_S: f32 = 20.0;

/// GR の掛かりの時定数 [ms]（コンプの GR と同じく速い）。
const GR_ATTACK_TAU_MS: f32 = 20.0;
/// GR の戻りの時定数 [ms]。
const GR_RELEASE_TAU_MS: f32 = 500.0;

/// リニア値 -> dB（下限 `DB_FLOOR`）。
#[inline]
pub fn lin_to_db(x: f32) -> f32 {
    let v = if x > 0.0 { x } else { 1.0e-6 };
    (20.0 * v.log10()).max(DB_FLOOR)
}

/// リニア値 -> 0..1（`DB_FLOOR` .. 0 dBFS）。バー表示用。
#[inline]
pub fn lin_to_norm(x: f32) -> f32 {
    ((lin_to_db(x) - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0)
}

/// 1 次遅れの係数 `1 - exp(-dt/τ)`。`dt` と `τ` は同じ単位。
#[inline]
fn lag_k(dt_ms: f32, tau_ms: f32) -> f32 {
    if tau_ms <= 0.0 {
        1.0
    } else {
        1.0 - (-dt_ms / tau_ms).exp()
    }
}

/// VU の弾道（1 次遅れ 2 段 = 臨界制動 2 次系）。
#[derive(Debug, Clone)]
pub struct VuBallistic {
    k: f32,
    /// [段][チャンネル]。`stage[0]` が入力側。
    stage: [[f32; 2]; 2],
}

impl VuBallistic {
    pub fn new() -> Self {
        Self {
            k: lag_k(TICK_MS, VU_TAU_MS),
            stage: [[0.0; 2]; 2],
        }
    }

    pub fn reset(&mut self) {
        self.stage = [[0.0; 2]; 2];
    }

    /// リニア値を 1 ティック進める。
    pub fn step(&mut self, ch: usize, x: f32) -> f32 {
        let ch = ch.min(1);
        let k = self.k;
        let a0 = self.stage[0][ch] + k * (x - self.stage[0][ch]);
        self.stage[0][ch] = a0;
        let a1 = self.stage[1][ch] + k * (a0 - self.stage[1][ch]);
        self.stage[1][ch] = a1;
        a1
    }
}

impl Default for VuBallistic {
    fn default() -> Self {
        Self::new()
    }
}

/// 入力ピークホールド（dB 表示用）。
#[derive(Debug, Clone)]
pub struct PeakHold {
    db: [f32; 2],
    hold_ms: [f32; 2],
}

impl PeakHold {
    pub fn new() -> Self {
        Self {
            db: [DB_FLOOR; 2],
            hold_ms: [0.0; 2],
        }
    }

    pub fn reset(&mut self) {
        self.db = [DB_FLOOR; 2];
        self.hold_ms = [0.0; 2];
    }

    /// リニアなピーク値を 1 ティック進め、保持中のピーク [dBFS] を返す。
    pub fn step(&mut self, ch: usize, peak_lin: f32) -> f32 {
        let ch = ch.min(1);
        let now = lin_to_db(peak_lin);
        if now >= self.db[ch] {
            self.db[ch] = now;
            self.hold_ms[ch] = PEAK_HOLD_MS;
        } else if self.hold_ms[ch] > 0.0 {
            self.hold_ms[ch] -= TICK_MS;
        } else {
            let drop = PEAK_DECAY_DB_PER_S * TICK_MS / 1000.0;
            self.db[ch] = (self.db[ch] - drop).max(now).max(DB_FLOOR);
        }
        self.db[ch]
    }
}

impl Default for PeakHold {
    fn default() -> Self {
        Self::new()
    }
}

/// GR（抑圧量）の弾道。掛かりは速く、戻りは遅い（コンプの GR と同じ）。
#[derive(Debug, Clone)]
pub struct GrBallistic {
    k_attack: f32,
    k_release: f32,
    db: f32,
}

impl GrBallistic {
    pub fn new() -> Self {
        Self {
            k_attack: lag_k(TICK_MS, GR_ATTACK_TAU_MS),
            k_release: lag_k(TICK_MS, GR_RELEASE_TAU_MS),
            db: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.db = 0.0;
    }

    /// 入力 / 出力のリニア RMS を入れて、抑圧量 [dB] (>= 0) を返す。
    ///
    /// VOICE で持ち上げて出力の方が大きい場合は 0（ゲインアップは GR ではない）。
    pub fn step(&mut self, in_lin: f32, out_lin: f32) -> f32 {
        let target = if in_lin > 1.0e-5 && out_lin > 0.0 {
            (lin_to_db(in_lin) - lin_to_db(out_lin)).max(0.0)
        } else {
            0.0
        };
        let k = if target > self.db {
            self.k_attack
        } else {
            self.k_release
        };
        self.db += k * (target - self.db);
        if self.db < 0.0 {
            self.db = 0.0;
        }
        self.db
    }
}

impl Default for GrBallistic {
    fn default() -> Self {
        Self::new()
    }
}

/// パネルが 1 ティックぶん進めた結果。
#[derive(Clone, Copy, Debug, Default)]
pub struct Reading {
    /// VU の平滑後のリニア値 [ch]（針の位置はこれを dB にしてから写す）。
    pub vu_lin: [f32; 2],
    /// VU の値 [dBFS][ch]（針と同じ値から算出。数値表示用）。
    pub vu_db: [f32; 2],
    /// 入力ピークホールド [dBFS][ch]。
    pub peak_db: [f32; 2],
    /// 抑圧量 [dB]（0 = 抑圧なし）。
    pub gr_db: f32,
}

/// 3 つの弾道をまとめて持つ（パネルが 1 つ持てばよい）。
#[derive(Debug, Clone)]
pub struct MeterState {
    vu: VuBallistic,
    peak: PeakHold,
    gr: GrBallistic,
    /// 出力 RMS の平滑値（GR 用に軽く均す）。
    out_sm: [f32; 2],
    in_sm: [f32; 2],
}

impl MeterState {
    pub fn new() -> Self {
        Self {
            vu: VuBallistic::new(),
            peak: PeakHold::new(),
            gr: GrBallistic::new(),
            out_sm: [0.0; 2],
            in_sm: [0.0; 2],
        }
    }

    pub fn reset(&mut self) {
        self.vu.reset();
        self.peak.reset();
        self.gr.reset();
        self.out_sm = [0.0; 2];
        self.in_sm = [0.0; 2];
    }

    /// 1 ティック進める。
    ///
    /// `live` が false（前回のティックから新しい音声ブロックが来ていない）なら
    /// 無音として扱い、針は静止位置へ戻る。
    pub fn tick(
        &mut self,
        live: bool,
        in_rms: [f32; 2],
        in_peak: [f32; 2],
        out_rms: [f32; 2],
        n_ch: usize,
    ) -> Reading {
        let mut out = Reading::default();

        for ch in 0..2 {
            let active = live && ch < n_ch.max(1);
            let rin = if active { in_rms[ch] } else { 0.0 };
            let rpk = if active { in_peak[ch] } else { 0.0 };
            let rout = if active { out_rms[ch] } else { 0.0 };

            out.vu_lin[ch] = self.vu.step(ch, rin);
            out.vu_db[ch] = lin_to_db(out.vu_lin[ch]);
            out.peak_db[ch] = self.peak.step(ch, rpk);

            // GR の材料も軽く均す（ブロックごとの揺れでバーが暴れないように）。
            let k = lag_k(TICK_MS, 30.0);
            self.in_sm[ch] += k * (rin - self.in_sm[ch]);
            self.out_sm[ch] += k * (rout - self.out_sm[ch]);
        }

        // GR は 1 本のメーターなので、チャンネルをまとめる（大きい方の抑圧を出す）。
        let gr = if n_ch.max(1) > 1 {
            self.gr
                .step(self.in_sm[0].max(self.in_sm[1]), self.out_sm[0].max(self.out_sm[1]))
        } else {
            self.gr.step(self.in_sm[0], self.out_sm[0])
        };
        out.gr_db = gr;

        out
    }
}

impl Default for MeterState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vu_reaches_99_percent_within_300ms() {
        let mut v = VuBallistic::new();
        // 7 ティック = 280 ms ではまだ 99 % 未満。
        let mut at280 = 0.0;
        for _ in 0..7 {
            at280 = v.step(0, 1.0);
        }
        assert!(at280 < 0.99, "280 ms で 99 % に達してはいけない: {at280}");

        // 8 ティック = 320 ms で 99 % 以上。
        let at320 = v.step(0, 1.0);
        assert!(at320 >= 0.99, "320 ms で 99 % に達していない: {at320}");
    }

    #[test]
    fn vu_has_almost_no_overshoot() {
        let mut v = VuBallistic::new();
        let mut max = 0.0f32;
        let mut last = 0.0f32;
        for _ in 0..100 {
            last = v.step(0, 1.0);
            max = max.max(last);
        }
        // 実機 VU の許容は 1.5 % 以下。臨界制動なので行き過ぎは無い。
        assert!(max <= 1.02, "行き過ぎが大きい: {max}");
        assert!((last - 1.0).abs() < 1e-3);
    }

    #[test]
    fn vu_returns_symmetrically() {
        let mut v = VuBallistic::new();
        let mut up = 0.0;
        for _ in 0..50 {
            up = v.step(0, 1.0);
        }
        assert!(up > 0.999);
        let mut down = 0.0;
        for _ in 0..8 {
            down = v.step(0, 0.0);
        }
        // 立ち上がりと同じ時定数なので、8 ティックで残りは 1 % 以下。
        assert!(down <= 0.01, "戻りが遅すぎる: {down}");
    }

    #[test]
    fn peak_holds_then_decays() {
        let mut p = PeakHold::new();
        assert_eq!(p.step(0, 1.0), 0.0);

        // 1.5 s (38 ティック) は保持される。
        let mut held = 0.0;
        for _ in 0..37 {
            held = p.step(0, 0.0);
        }
        assert!(held > -0.01, "保持中に落ちた: {held}");

        // そこから 20 dB/s で落ちる。
        let mut after = 0.0;
        for _ in 0..10 {
            after = p.step(0, 0.0);
        }
        assert!(after < -5.0 && after > -30.0, "減衰量が異常: {after}");
    }

    #[test]
    fn gr_moves_fast_and_returns_slowly() {
        let mut g = GrBallistic::new();
        // 20 dB の抑圧。
        let mut deep = 0.0;
        for _ in 0..40 {
            deep = g.step(1.0, 0.1);
        }
        assert!((deep - 20.0).abs() < 0.5, "GR が合わない: {deep}");

        // 掛かりは速い。
        let mut g2 = GrBallistic::new();
        let fast = g2.step(1.0, 0.1);
        assert!(fast > 15.0, "掛かりが遅い: {fast}");

        // 戻りは遅い（500 ms の時定数なので 1 ティックでは 3 dB も戻らない）。
        let back = g.step(1.0, 1.0);
        assert!(back > deep - 3.0, "戻りが速すぎる: {back}");
    }

    #[test]
    fn gr_is_zero_when_louder_out() {
        let mut g = GrBallistic::new();
        let mut v = 0.0;
        for _ in 0..20 {
            v = g.step(0.1, 0.5);
        }
        assert_eq!(v, 0.0);
    }

    #[test]
    fn meter_state_returns_to_rest_when_not_live() {
        let mut m = MeterState::new();
        // VU の針は 300 ms かけて振れるので、10 ティック (= 400 ms) 入れる。
        let mut r = Reading::default();
        for _ in 0..10 {
            r = m.tick(true, [0.2, 0.2], [0.3, 0.3], [0.1, 0.1], 2);
        }
        assert!(r.vu_db[0] > -20.0, "針が振れていない: {}", r.vu_db[0]);
        assert!(r.peak_db[0] > -20.0, "ピークが出ていない: {}", r.peak_db[0]);
        assert!(r.gr_db > 0.0, "GR が出ていない: {}", r.gr_db);

        // 音声が来なくなったら (live = false) 針もピークも下がる。
        let mut last = r;
        for _ in 0..40 {
            last = m.tick(false, [0.2, 0.2], [0.3, 0.3], [0.1, 0.1], 2);
        }
        assert!(last.vu_db[0] <= DB_FLOOR + 0.01, "針が戻らない: {}", last.vu_db[0]);
        // GR の戻りは 500 ms の時定数なので 1.6 s で 1 dB 以下まで下がる。
        assert!(last.gr_db < 1.0, "GR が戻らない: {}", last.gr_db);
    }

    #[test]
    fn mono_second_channel_rests() {
        let mut m = MeterState::new();
        let mut r = Reading::default();
        for _ in 0..10 {
            r = m.tick(true, [0.2, 0.2], [0.3, 0.3], [0.1, 0.1], 1);
        }
        assert!(r.vu_db[0] > -20.0, "針が振れていない: {}", r.vu_db[0]);
        assert!(r.vu_db[1] <= DB_FLOOR + 0.01, "R が動いた: {}", r.vu_db[1]);
    }
}
