//! 「音声処理がリアルタイムに間に合っているか」の判定。
//!
//! 計測は `dsp::engine::Meters`（オーディオスレッドが 1 ホップごとに足し込む）
//! が持ち、判定はここで行う。ここは**純関数だけ**にしてあるので、スレッドを
//! 立てずにテストできる。
//!
//! 背景: DPDFNet の推論は 1 ホップ = 480 サンプル (10 ms) ごとに走る。ノート
//! PC などでこの 10 ms を超えると、OBS の音声スレッドが詰まって音が途切れ、
//! 見かけ上「音が遅れる」状態になる。実測では高品質モデル (`dpdfnet8`) は
//! 速い CPU でも 1 コアの 7 割を使うため、余裕の有無をログに残す価値が高い。

use crate::dsp::engine::DspStats;

/// 判定に使う最小のホップ数。少なすぎるサンプルで誤判定しないための下限。
pub const MIN_HOPS: u64 = 50;

/// 「間に合っていない」と見なす平均処理時間のしきい値（予算に対する比）。
///
/// ログに注意を出すための基準。速い CPU でも高品質モデルは 80 % 前後まで使う
/// ので、ここは「ほぼ使い切っている」水準にしてある。
pub const SLOW_AVG_RATIO: f64 = 0.95;

/// 「間に合っていない」と見なす超過ホップの割合 [%]。
///
/// 単発で 10 ms を少し超えるホップは、OBS が 21 ms ぶん（1024 サンプル）を
/// まとめて処理するので吸収される（実測でも速い CPU で 5 % 前後ある）。
/// ここは「半分近くが溢れている」水準にして、正常な機械で警告を出さない。
pub const SLOW_OVER_PCT: f64 = 25.0;

/// 自動フォールバックを検討する「間に合っていない」の基準（平均が予算そのもの
/// を超えている）。
///
/// `SLOW_AVG_RATIO` より厳しくしてあるのは、予算ぎりぎりで動いている機械
/// （実測で速い CPU でも 80 % 程度になる）で、一時的な負荷変動だけで品質を
/// 落とさないため。
pub const CRITICAL_AVG_RATIO: f64 = 1.0;

/// 自動フォールバックを検討する超過ホップの割合 [%]。
pub const CRITICAL_OVER_PCT: f64 = 50.0;

/// これだけ続けて重症だったら、自動で軽量モデルへ切り替える（監視の回数）。
pub const FALLBACK_STREAK: u32 = 3;

/// 直近の負荷（累計 2 点の差分から求めた値）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Load {
    /// この区間で計測したホップ数。
    pub hops: u64,
    /// 1 ホップあたりの平均処理時間 [ms]。
    pub avg_ms: f64,
    /// 1 ホップの予算（= ホップぶんの実時間）[ms]。
    pub budget_ms: f64,
    /// 予算を超えたホップの割合 [%]。
    pub over_pct: f64,
    /// 起動からの最大処理時間 [ms]（累計値なので区間内とは限らない）。
    pub max_ms: f64,
    /// リアルタイムに間に合っていない疑いがあるか（ログに注意を出す基準）。
    pub slow: bool,
    /// 間に合っていないのが確実か（自動フォールバックの基準）。
    /// 平均が予算を超えている、または超過ホップが 10 % 以上。
    pub critical: bool,
}

impl Load {
    /// 予算に対する使用率 [%]。
    pub fn usage_pct(&self) -> f64 {
        if self.budget_ms <= 0.0 {
            0.0
        } else {
            self.avg_ms / self.budget_ms * 100.0
        }
    }
}

/// 累計値の差分から直近の負荷を求める。
///
/// ホップが増えていない / 予算が未設定 / サンプルが少なすぎるときは `None`
/// （呼び出し側は前回値を保ったまま次の周期を待てばよい）。
pub fn sample(prev: DspStats, cur: DspStats) -> Option<Load> {
    let hops = cur.hops.checked_sub(prev.hops)?;
    if hops < MIN_HOPS || cur.budget_us == 0 {
        return None;
    }

    let sum_us = cur.sum_us.saturating_sub(prev.sum_us);
    let over = cur.over.saturating_sub(prev.over);
    let avg_us = sum_us as f64 / hops as f64;
    let over_pct = over as f64 * 100.0 / hops as f64;
    let budget_us = cur.budget_us as f64;

    Some(Load {
        hops,
        avg_ms: avg_us / 1000.0,
        budget_ms: budget_us / 1000.0,
        over_pct,
        max_ms: cur.max_us as f64 / 1000.0,
        slow: avg_us > budget_us * SLOW_AVG_RATIO || over_pct >= SLOW_OVER_PCT,
        critical: avg_us > budget_us * CRITICAL_AVG_RATIO || over_pct >= CRITICAL_OVER_PCT,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(hops: u64, over: u64, sum_us: u64, max_us: u32) -> DspStats {
        DspStats {
            hops,
            over,
            sum_us,
            max_us,
            budget_us: 10_000,
        }
    }

    #[test]
    fn no_new_hops_is_none() {
        let s = stats(1000, 0, 2_000_000, 3_000);
        assert!(sample(s, s).is_none());
    }

    #[test]
    fn too_few_hops_is_none() {
        let prev = stats(0, 0, 0, 0);
        let cur = stats(MIN_HOPS - 1, 0, 1000, 20);
        assert!(sample(prev, cur).is_none());
    }

    #[test]
    fn missing_budget_is_none() {
        let prev = stats(0, 0, 0, 0);
        let mut cur = stats(MIN_HOPS, 0, 1000, 20);
        cur.budget_us = 0;
        assert!(sample(prev, cur).is_none());
    }

    #[test]
    fn healthy_load_is_not_slow() {
        let prev = stats(0, 0, 0, 0);
        // 100 hop で 200 ms = 2.0 ms/hop（予算 10 ms）→ 使用率 20 %.
        let cur = stats(100, 0, 200_000, 3_500);
        let l = sample(prev, cur).expect("sample");
        assert_eq!(l.hops, 100);
        assert!((l.avg_ms - 2.0).abs() < 1e-9);
        assert!((l.budget_ms - 10.0).abs() < 1e-9);
        assert!((l.usage_pct() - 20.0).abs() < 1e-9);
        assert_eq!(l.over_pct, 0.0);
        assert!(!l.slow);
        assert!(!l.critical);
    }

    #[test]
    fn high_average_is_slow() {
        let prev = stats(0, 0, 0, 0);
        // 100 hop で 960 ms = 9.6 ms/hop = 予算の 96 %.
        let cur = stats(100, 0, 960_000, 12_000);
        let l = sample(prev, cur).expect("sample");
        assert!(l.slow);
        // 予算内ではあるので、自動フォールバックまではしない。
        assert!(!l.critical);
    }

    #[test]
    fn tight_but_working_is_not_slow() {
        // 速い CPU の実測（予算の 86 %、単発で超えるホップが 6 %）。
        // OBS が 21 ms ぶんをまとめて処理するので吸収され、警告は出さない。
        let prev = stats(0, 0, 0, 0);
        let cur = stats(100, 6, 860_000, 10_900);
        let l = sample(prev, cur).expect("sample");
        assert!((l.usage_pct() - 86.0).abs() < 1e-9);
        assert!((l.over_pct - 6.0).abs() < 1e-9);
        assert!(!l.slow);
        assert!(!l.critical);
    }

    #[test]
    fn occasional_overruns_are_slow() {
        let prev = stats(0, 0, 0, 0);
        // 平均は予算内 (5.0 ms/hop) でも、3 割のホップが予算超過。
        let cur = stats(100, 30, 500_000, 25_000);
        let l = sample(prev, cur).expect("sample");
        assert!((l.over_pct - 30.0).abs() < 1e-9);
        assert!(l.slow);
        assert!(!l.critical);
    }

    #[test]
    fn over_budget_average_is_critical() {
        let prev = stats(0, 0, 0, 0);
        // 100 hop で 1200 ms = 12.0 ms/hop（予算 10 ms を超えている）。
        let cur = stats(100, 90, 1_200_000, 25_000);
        let l = sample(prev, cur).expect("sample");
        assert!(l.slow);
        assert!(l.critical);
    }

    #[test]
    fn many_overruns_are_critical_even_if_the_average_looks_fine() {
        let prev = stats(0, 0, 0, 0);
        // 平均 5.0 ms/hop でも 6 割のホップが予算超過（他のプロセスに
        // オーディオスレッドを奪われているような状態）。
        let cur = stats(100, 60, 500_000, 30_000);
        let l = sample(prev, cur).expect("sample");
        assert!(l.slow);
        assert!(l.critical);
    }

    #[test]
    fn average_is_taken_over_the_interval_only() {
        // 直前の区間でどれだけ時間を使っていても、差分で見るので影響しない。
        let prev = stats(1000, 500, 9_000_000, 30_000);
        let cur = stats(1100, 500, 9_200_000, 30_000);
        let l = sample(prev, cur).expect("sample");
        assert_eq!(l.hops, 100);
        assert!((l.avg_ms - 2.0).abs() < 1e-9);
        assert_eq!(l.over_pct, 0.0);
        assert!(!l.slow);
        assert!(!l.critical);
    }
}
