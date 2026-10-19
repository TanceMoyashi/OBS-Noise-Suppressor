//! メーターの「針の動き」（弾道特性）。
//!
//! パネルは 40 ms ごと (`crate::TIMER_MS`) にエンジンのレベルを読み、ここへ
//! 通して表示用の値にする。Win32 に依存しないのでテストできる。
//!
//! メーターは 2 本（IN = 加工前 / OUT = 加工後）。ステレオは L/R の**平均**を
//! 1 本の針にまとめる（モノラルはそのまま）。
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
//!
//! このモジュールは OBS 版 / VST3 版で共有する（`panel_ui`）。

/// パネルのタイマー間隔 [ms]（`crate::TIMER_MS` と一致させること）。
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

/// VU の弾道（1 次遅れ 2 段 = 臨界制動 2 次系）。1 本の針ぶん。
#[derive(Debug, Clone)]
pub struct VuBallistic {
    k: f32,
    /// [段]。`stage[0]` が入力側。
    stage: [f32; 2],
}

impl VuBallistic {
    pub fn new() -> Self {
        Self {
            k: lag_k(TICK_MS, VU_TAU_MS),
            stage: [0.0; 2],
        }
    }

    pub fn reset(&mut self) {
        self.stage = [0.0; 2];
    }

    /// リニア値を 1 ティック進める。
    pub fn step(&mut self, x: f32) -> f32 {
        let k = self.k;
        let a0 = self.stage[0] + k * (x - self.stage[0]);
        self.stage[0] = a0;
        let a1 = self.stage[1] + k * (a0 - self.stage[1]);
        self.stage[1] = a1;
        a1
    }
}

impl Default for VuBallistic {
    fn default() -> Self {
        Self::new()
    }
}

/// ピークホールド（dB 表示用）。1 本のメーターぶん。
#[derive(Debug, Clone)]
pub struct PeakHold {
    db: f32,
    hold_ms: f32,
}

impl PeakHold {
    pub fn new() -> Self {
        Self {
            db: DB_FLOOR,
            hold_ms: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.db = DB_FLOOR;
        self.hold_ms = 0.0;
    }

    /// リニアなピーク値を 1 ティック進め、保持中のピーク [dBFS] を返す。
    pub fn step(&mut self, peak_lin: f32) -> f32 {
        let now = lin_to_db(peak_lin);
        if now >= self.db {
            self.db = now;
            self.hold_ms = PEAK_HOLD_MS;
        } else if self.hold_ms > 0.0 {
            self.hold_ms -= TICK_MS;
        } else {
            let drop = PEAK_DECAY_DB_PER_S * TICK_MS / 1000.0;
            self.db = (self.db - drop).max(now).max(DB_FLOOR);
        }
        self.db
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
///
/// メーターは 2 本（IN / OUT）。ステレオは L/R の**平均**を 1 本にまとめる
/// （モノラルはその 1 本をそのまま）。
///
/// `PartialEq` はパネルの「変化があったときだけ再描画」の判定に使う
/// （静的な画面で毎フレーム描き直さないため）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    /// IN の針: 平滑後のリニア値（針の位置はこれを dB にしてから写す）。
    pub vu_in_lin: f32,
    /// IN の値 [dBFS]（針と同じ値から算出。数値表示用）。
    pub vu_in_db: f32,
    /// OUT の針。
    pub vu_out_lin: f32,
    /// OUT の値 [dBFS]。
    pub vu_out_db: f32,
    /// ピークホールド [dBFS]。
    pub peak_in_db: f32,
    pub peak_out_db: f32,
    /// 抑圧量 [dB]（0 = 抑圧なし）。
    pub gr_db: f32,
}

impl Default for Reading {
    /// 音声がまだ来ていないときの表示（針は左端、数値は下限）。
    fn default() -> Self {
        Self {
            vu_in_lin: 0.0,
            vu_in_db: DB_FLOOR,
            vu_out_lin: 0.0,
            vu_out_db: DB_FLOOR,
            peak_in_db: DB_FLOOR,
            peak_out_db: DB_FLOOR,
            gr_db: 0.0,
        }
    }
}

/// 弾道をまとめて持つ（パネルが 1 つ持てばよい）。
#[derive(Debug, Clone)]
pub struct MeterState {
    vu_in: VuBallistic,
    vu_out: VuBallistic,
    peak_in: PeakHold,
    peak_out: PeakHold,
    gr: GrBallistic,
    /// 入力 / 出力 RMS の平滑値（GR 用に軽く均す）。
    in_sm: f32,
    out_sm: f32,
}

impl MeterState {
    pub fn new() -> Self {
        Self {
            vu_in: VuBallistic::new(),
            vu_out: VuBallistic::new(),
            peak_in: PeakHold::new(),
            peak_out: PeakHold::new(),
            gr: GrBallistic::new(),
            in_sm: 0.0,
            out_sm: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.vu_in.reset();
        self.vu_out.reset();
        self.peak_in.reset();
        self.peak_out.reset();
        self.gr.reset();
        self.in_sm = 0.0;
        self.out_sm = 0.0;
    }

    /// 1 ティック進める。
    ///
    /// `live` が false（前回のティックから新しい音声ブロックが来ていない）なら
    /// 無音として扱い、針は静止位置へ戻る。
    ///
    /// `n_ch` が 1 のときは ch0 だけを見る。ステレオで片側が死んでいても
    /// 半分に落ちないよう、割る相手をチャンネル数で変えている。
    pub fn tick(
        &mut self,
        live: bool,
        in_rms: [f32; 2],
        in_peak: [f32; 2],
        out_rms: [f32; 2],
        out_peak: [f32; 2],
        n_ch: usize,
    ) -> Reading {
        let mut out = Reading::default();

        // ステレオは L/R の平均、モノラルは ch0 そのまま。
        let average =
            |v: [f32; 2]| if n_ch.max(1) > 1 { 0.5 * (v[0] + v[1]) } else { v[0] };
        let (rin, rpkin, rout, rpkout) = if live {
            (
                average(in_rms),
                average(in_peak),
                average(out_rms),
                average(out_peak),
            )
        } else {
            (0.0, 0.0, 0.0, 0.0)
        };

        out.vu_in_lin = self.vu_in.step(rin);
        out.vu_in_db = lin_to_db(out.vu_in_lin);
        out.vu_out_lin = self.vu_out.step(rout);
        out.vu_out_db = lin_to_db(out.vu_out_lin);
        out.peak_in_db = self.peak_in.step(rpkin);
        out.peak_out_db = self.peak_out.step(rpkout);

        // GR の材料も軽く均す（ブロックごとの揺れでバーが暴れないように）。
        let k = lag_k(TICK_MS, 30.0);
        self.in_sm += k * (rin - self.in_sm);
        self.out_sm += k * (rout - self.out_sm);
        out.gr_db = self.gr.step(self.in_sm, self.out_sm);

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
            at280 = v.step(1.0);
        }
        assert!(at280 < 0.99, "280 ms で 99 % に達してはいけない: {at280}");

        // 8 ティック = 320 ms で 99 % 以上。
        let at320 = v.step(1.0);
        assert!(at320 >= 0.99, "320 ms で 99 % に達していない: {at320}");
    }

    #[test]
    fn vu_has_almost_no_overshoot() {
        let mut v = VuBallistic::new();
        let mut max = 0.0f32;
        let mut last = 0.0f32;
        for _ in 0..100 {
            last = v.step(1.0);
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
            up = v.step(1.0);
        }
        assert!(up > 0.999);
        let mut down = 0.0;
        for _ in 0..8 {
            down = v.step(0.0);
        }
        // 立ち上がりと同じ時定数なので、8 ティックで残りは 1 % 以下。
        assert!(down <= 0.01, "戻りが遅すぎる: {down}");
    }

    #[test]
    fn peak_holds_then_decays() {
        let mut p = PeakHold::new();
        assert_eq!(p.step(1.0), 0.0);

        // 1.5 s (38 ティック) は保持される。
        let mut held = 0.0;
        for _ in 0..37 {
            held = p.step(0.0);
        }
        assert!(held > -0.01, "保持中に落ちた: {held}");

        // そこから 20 dB/s で落ちる。
        let mut after = 0.0;
        for _ in 0..10 {
            after = p.step(0.0);
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
    fn meter_state_shows_in_and_out_and_returns_to_rest() {
        let mut m = MeterState::new();
        // VU の針は 300 ms かけて振れるので、10 ティック (= 400 ms) 入れる。
        let mut r = Reading::default();
        for _ in 0..10 {
            r = m.tick(true, [0.2, 0.2], [0.3, 0.3], [0.1, 0.1], [0.15, 0.15], 2);
        }
        assert!(r.vu_in_db > -20.0, "IN の針が振れていない: {}", r.vu_in_db);
        assert!(r.vu_out_db > -30.0, "OUT の針が振れていない: {}", r.vu_out_db);
        assert!(
            r.vu_in_db > r.vu_out_db + 3.0,
            "抑圧量が見えない: {} / {}",
            r.vu_in_db,
            r.vu_out_db
        );
        assert!(r.peak_in_db > -20.0, "IN のピークが出ていない: {}", r.peak_in_db);
        assert!(r.peak_out_db > -25.0, "OUT のピークが出ていない: {}", r.peak_out_db);
        assert!(r.gr_db > 0.0, "GR が出ていない: {}", r.gr_db);

        // 音声が来なくなったら (live = false) 針は戻る。ピークは 1.5 s 保持。
        let mut last = r;
        for _ in 0..30 {
            last = m.tick(false, [0.2, 0.2], [0.3, 0.3], [0.1, 0.1], [0.15, 0.15], 2);
        }
        assert!(
            last.vu_in_db <= DB_FLOOR + 0.01,
            "IN の針が戻らない: {}",
            last.vu_in_db
        );
        assert!(
            last.vu_out_db <= DB_FLOOR + 0.01,
            "OUT の針が戻らない: {}",
            last.vu_out_db
        );
        assert!(
            last.peak_in_db > -11.0,
            "ピークが保持されていない: {}",
            last.peak_in_db
        );
        // GR の戻りは 500 ms の時定数なので 1.2 s で 1 dB 以下まで下がる。
        assert!(last.gr_db < 1.0, "GR が戻らない: {}", last.gr_db);

        // 保持が切れたら下限まで落ちる。
        for _ in 0..150 {
            last = m.tick(false, [0.2, 0.2], [0.3, 0.3], [0.1, 0.1], [0.15, 0.15], 2);
        }
        assert!(
            last.peak_in_db <= DB_FLOOR + 0.01,
            "ピークが落ちきらない: {}",
            last.peak_in_db
        );
        assert!(
            last.peak_out_db <= DB_FLOOR + 0.01,
            "OUT のピークが落ちきらない: {}",
            last.peak_out_db
        );
    }

    #[test]
    fn stereo_averages_channels_and_mono_uses_first() {
        // ステレオ (L = R = 0.2) は 0.2 のまま。
        let mut st = MeterState::new();
        let mut stereo = Reading::default();
        for _ in 0..10 {
            stereo = st.tick(true, [0.2, 0.2], [0.2, 0.2], [0.2, 0.2], [0.2, 0.2], 2);
        }

        // モノラル (ch0 = 0.2 / ch1 は無い) でも同じ値になること。死んだ
        // チャンネルで半分に落ちない。
        let mut mo = MeterState::new();
        let mut mono = Reading::default();
        for _ in 0..10 {
            mono = mo.tick(true, [0.2, 0.0], [0.2, 0.0], [0.2, 0.0], [0.2, 0.0], 1);
        }
        assert!(
            (stereo.vu_in_db - mono.vu_in_db).abs() < 0.05,
            "モノラルとステレオで値が違う: {} / {}",
            stereo.vu_in_db,
            mono.vu_in_db
        );

        // ステレオで片側だけ鳴っているときは平均なので 6 dB 下がる。
        let mut half = MeterState::new();
        let mut l_only = Reading::default();
        for _ in 0..10 {
            l_only = half.tick(true, [0.2, 0.0], [0.2, 0.0], [0.2, 0.0], [0.2, 0.0], 2);
        }
        assert!(
            (stereo.vu_in_db - l_only.vu_in_db - 6.02).abs() < 0.1,
            "L/R の平均になっていない: {} / {}",
            stereo.vu_in_db,
            l_only.vu_in_db
        );
    }
}
