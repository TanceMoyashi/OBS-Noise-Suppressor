#!/usr/bin/env python3
"""OBS で録った音から、プラグインの遅延と抑圧量を測る。

OBS でレイテンシを測るには「参照経路」が必要。そこで **2 トラック録音**を使う:

    Track 1 = メディアソース A (atkAudio → DPDFNet Voice)   ← 処理後
    Track 2 = メディアソース B (同じファイル、フィルタ無し)  ← 参照 (dry)

同じシーンに 2 つ置けば同時に再生が始まるので、トラック間のずれが
そのままプラグインの遅延になる。手順は OBS_VERIFY.md を参照。

使い方 (2 トラック録音):
  python tools/analyze_obs_capture.py --two-track obs_capture.wav --expect-lag 2880

使い方 (1 ファイル比較。遅延は絶対値では測れない):
  python tools/analyze_obs_capture.py --input test_signals/speech_tail_reverb_48k.wav \
      --capture obs_capture.wav

判定:
  遅延     : 2880 (= 6 hop) に一致 → atkAudio が PDC を実装していない場合は正しい
             0              → PDC が働いて完全に補正されている (これも正しい)
             それ以外        → 不一致。プラグインか atkAudio の設定を見直す
  無音区間 : 入力が「ほぼ無音」の区間で出力が何 dB 下がっているか。
             ノイズ抑圧 + 残響抑圧の総合効果がここに出る。
             Voice Reverb を 0 と 1 で録り比べれば、その差が de-reverb の効き幅。
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np
import soundfile as sf

HOP = 480          # 48 kHz での DPDFNet の hop
EXPECT = 6 * HOP   # = 2880


# ---------------------------------------------------------------------------
def load(path: Path) -> tuple[np.ndarray, int]:
    x, sr = sf.read(str(path), always_2d=True)
    return x.astype(np.float32), int(sr)


def mono(x: np.ndarray) -> np.ndarray:
    return x.mean(axis=1) if x.ndim > 1 else x


def rms(v: np.ndarray) -> float:
    return float(np.sqrt(np.mean(np.square(v)))) if v.size else 0.0


def db(v: float) -> float:
    return 20.0 * np.log10(v) if v > 1e-12 else -240.0


def envelope(x: np.ndarray, sr: int, win_ms: float = 20.0, hop_ms: float = 10.0):
    win = max(1, int(sr * win_ms / 1000.0))
    hop = max(1, int(sr * hop_ms / 1000.0))
    n = 1 + max(0, (len(x) - win) // hop)
    if n <= 0:
        return np.zeros(0, np.float32), win, hop
    idx = np.arange(win)[None, :] + hop * np.arange(n)[:, None]
    return np.sqrt(np.mean(np.square(x[idx]), axis=1)).astype(np.float32), win, hop


# ---------------------------------------------------------------------------
def measure_lag(a: np.ndarray, b: np.ndarray, sr: int, max_lag_ms: float = 500.0):
    """b が a より何サンプル遅れているか (a=参照, b=処理後) を返す。

    包絡線で粗く合わせ、その周辺で生波形の相互相関により精密化する。
    正の値 = b が a より遅い。
    """
    ea, _, hop = envelope(a, sr)
    eb, _, _ = envelope(b, sr)
    ea, eb = ea - ea.mean(), eb - eb.mean()
    if len(ea) < 3 or len(eb) < 3:
        return 0, 0.0

    c = np.correlate(eb, ea, mode="full")
    k = int(np.argmax(np.abs(c)) - (len(ea) - 1))     # 包絡線のずれ (フレーム)
    coarse = k * hop

    # 精密化 (±3 包絡フレーム = ±30 ms 以内の生波形相関)
    r = int(3 * hop)
    lo, hi = coarse - r, coarse + r
    n = min(len(a), len(b) - abs(hi) - 1)
    if n < 2 * r + 16:
        return coarse, 0.0
    seg_a = a[:n]
    cc = np.correlate(b[: n + hi], seg_a, mode="valid")
    if len(cc) == 0:
        return coarse, 0.0
    # b のずれ候補 lo..hi の中での最良を選ぶ
    best, bestv = coarse, 0.0
    for s in range(lo, hi + 1):
        if s >= 0:
            sb = b[s: s + n]
            ta = a[:n]
        else:
            sb = b[: n + s]
            ta = a[-s: n]
        if len(sb) < 2:
            continue
        sb = sb - sb.mean()
        ta = ta - ta.mean()
        den = (np.linalg.norm(sb) * np.linalg.norm(ta)) or 1.0
        v = float(np.dot(sb, ta) / den)
        if abs(v) > abs(bestv):
            best, bestv = s, v
    return best, float(bestv)


def low_activity_regions(x: np.ndarray, sr: int, below_db: float = -35.0,
                         min_ms: float = 150.0):
    """入力の「ほぼ無音」区間 (残響テール / ノイズ床) を [start, end) で返す。"""
    env, win, hop = envelope(x, sr)
    if len(env) == 0:
        return []
    peak = float(env.max())
    if peak <= 0:
        return []
    mask = env < peak * (10.0 ** (below_db / 20.0))
    need = max(1, int(min_ms / (hop / sr * 1000.0)))

    regions, i = [], 0
    while i < len(mask):
        if not mask[i]:
            i += 1
            continue
        j = i
        while j < len(mask) and mask[j]:
            j += 1
        if (j - i) >= need:
            s = i * hop
            e = min(len(x), (j - 1) * hop + win)
            if e > s:
                regions.append((s, e))
        i = j
    return regions


def region_stats(ref: np.ndarray, out: np.ndarray, regions, sr: int):
    """区間ごとの (参照 RMS, 出力 RMS, 減衰 dB)。"""
    rows = []
    for s, e in regions:
        r = rms(ref[s:e])
        o = rms(out[s:e])
        rows.append((r, o, db(o) - db(r)))
    return rows


SILENCE_FLOOR = 1e-5      # -100 dBFS。これ未満の区間は「デジタル無音」とみなす


def summarize_region_rows(rows) -> str:
    """区間統計を人間が読める形にする。

    参照がデジタル無音 (0.0) の区間は「減衰 dB」が定義できない (0 で割ることになる)。
    その場合は出力の絶対レベル (= プラグインが漏らした量) を報告する。
    """
    rel = [d for r, o, d in rows if r >= SILENCE_FLOOR]
    silent = [o for r, o, d in rows if r < SILENCE_FLOOR]
    lines = []
    if rel:
        a = np.array(rel)
        lines.append(f"相対減衰 中央値 = {float(np.median(a)):+6.1f} dB "
                     f"(p25 {np.percentile(a,25):+.1f} / p75 {np.percentile(a,75):+.1f}) "
                     f"[{len(rel)} 区間]")
    if silent:
        lines.append(f"デジタル無音区間 [{len(silent)} 区間]: 出力 = "
                     f"{db(float(np.median(np.array(silent)))):.1f} dBFS "
                     f"(プラグインの残留。低いほど良い)")
    if not lines:
        lines.append("(区間が無い。--below-db を緩める)")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
def report_two_track(path: Path, expect: int, below_db: float) -> int:
    x, sr = load(path)
    print(f"[WAV] {path.name}: {x.shape[0]} samples, {x.shape[1]} ch @ {sr} Hz")
    if sr != 48000:
        print(f"[WARN] OBS の録音が {sr} Hz。本プラグインは 48 kHz 専用 "
              f"(44.1 kHz ではパススルー)。設定 → 音声 → サンプルレートを 48 kHz に。")
    if x.shape[1] < 2:
        print("ERROR: 2 トラック必要 (Track1=処理後, Track2=参照)。"
              "OBS の詳細音声プロパティでトラックを分けること。", file=sys.stderr)
        return 1

    out = x[:, 0]          # 処理後
    ref = x[:, 1]          # 参照 (dry)

    lag, corr = measure_lag(ref, out, sr)
    print(f"\n[遅延] 処理後 - 参照 = {lag} samples "
          f"({lag/sr*1000:.1f} ms = {lag/HOP:.2f} hop)  corr={corr:.3f}")
    if lag == 0:
        print("       -> PDC (プラグイン遅延の自動補正) が完全に働いている")
    elif abs(lag - expect) <= 2:
        print(f"       -> 期待値 {expect} (= {expect/HOP:.2f} hop) と一致。"
              f"atkAudio が PDC を掛けていない場合はこれが正しい")
    else:
        print(f"       [WARN] 期待値 {expect} (= {expect/HOP:.2f} hop) とも 0 とも違う。"
              f"プラグインの kLatencyHops / atkAudio の設定を確認")

    # 参照 (dry) に lag ぶんの遅延を戻して時間を合わせる
    # lag > 0 = 処理後の方が遅い → 処理後を lag だけ進める
    if lag > 0:
        out = out[lag:]
        ref = ref[: len(out)]
    elif lag < 0:
        ref = ref[-lag:]
        out = out[: len(ref)]

    regions = low_activity_regions(ref, sr, below_db)
    rows = region_stats(ref, out, regions, sr)
    active = ~np.isin(np.arange(len(ref)) // sr,
                      np.concatenate([np.arange(s // sr, max(s // sr + 1, e // sr))
                                      for s, e in regions]) if regions
                      else np.zeros(0, int))
    print(f"\n[区間] 低活動 (参照が peak-{abs(below_db):.0f} dB 未満, "
          f"{len(regions)} 箇所)")
    if rows:
        arr = np.array(rows)
        print(f"       参照 RMS 中央値 = {db(float(np.median(arr[:,0]))):7.1f} dBFS   "
              f"出力 = {db(float(np.median(arr[:,1]))):7.1f} dBFS")
        for line in summarize_region_rows(rows).splitlines():
            print("       " + line)
    else:
        print("       (区間が見つからない。--below-db を緩める)")

    r_ref, r_out = rms(ref), rms(out)
    print(f"\n[全体] 参照 RMS = {db(r_ref):.2f} dBFS / 出力 = {db(r_out):.2f} dBFS "
          f"({db(r_out)-db(r_ref):+.2f} dB)")
    print("\n[次] Voice Reverb を 0 と 1 でそれぞれ録り、低活動区間の減衰を比べる。"
          "\n     その差が de-reverb の効き幅 (OBS_VERIFY.md §4)。")
    return 0


def report_input_capture(inp: Path, cap: Path, below_db: float, expect: int) -> int:
    a, sr_a = load(inp)
    b, sr_b = load(cap)
    a, b = mono(a), mono(b)
    print(f"[WAV] input {inp.name}: {len(a)} samples @ {sr_a} Hz")
    print(f"[WAV] capture {cap.name}: {len(b)} samples @ {sr_b} Hz")
    if sr_a != sr_b:
        print("[WARN] サンプルレートが違う。比較できない。CSS の 48 kHz 設定を確認。")

    lag, corr = measure_lag(a, b, sr_a)
    print(f"\n[ずれ] capture - input = {lag} samples ({lag/sr_a*1000:.1f} ms)  corr={corr:.3f}")
    print("       ※ メディアソースの再生開始位置が任意なので、絶対遅延は測れない。"
          "\n         正確に測るには --two-track (OBS_VERIFY.md §3)。")

    # capture の方が lag だけ遅れている → capture を進めて揃える
    if lag > 0:
        a2, b2 = a[: len(b)], b[lag:]
    else:
        a2, b2 = a[-lag:], b
    n = min(len(a2), len(b2))
    a2, b2 = a2[:n], b2[:n]

    regions = low_activity_regions(a2, sr_a, below_db)
    rows = region_stats(a2, b2, regions, sr_a)
    print(f"\n[区間] 低活動 {len(regions)} 箇所 (入力が peak-{abs(below_db):.0f} dB 未満)")
    if rows:
        for line in summarize_region_rows(rows).splitlines():
            print("       " + line)
    print(f"\n[全体] 入力 {db(rms(a2)):.2f} dBFS / 出力 {db(rms(b2)):.2f} dBFS "
          f"({db(rms(b2))-db(rms(a2)):+.2f} dB)")
    return 0


def selftest(source: Path, lag: int, atten_db: float) -> int:
    """既知の「遅延 + 低活動区間の減衰」を人工的に作り、測定器自体を検証する。"""
    if not source.is_file():
        print(f"ERROR: {source} が無い (先に tools/make_test_signals.py を実行)", file=sys.stderr)
        return 1
    x, sr = load(source)
    ref = mono(x)
    out = np.zeros_like(ref)
    if lag > 0:
        out[lag:] = ref[: len(ref) - lag]
    elif lag < 0:
        out[: len(ref) + lag] = ref[-lag:]
    else:
        out[:] = ref

    regions = low_activity_regions(ref, sr)
    g = 10.0 ** (atten_db / 20.0)
    for s, e in regions:
        ss, ee = s + lag, e + lag
        ss, ee = max(0, ss), min(len(out), ee)
        if ee > ss:
            out[ss:ee] *= g

    tmp = Path("test_signals") / "_selftest_two_track.wav"
    tmp.parent.mkdir(parents=True, exist_ok=True)
    sf.write(str(tmp), np.stack([out, ref], axis=1), sr, subtype="FLOAT")

    print(f"[SELFTEST] {source.name} に {lag} サンプルの遅延と "
          f"低活動区間 {atten_db:+.1f} dB の減衰を仕込んだ ({len(regions)} 区間)")
    rc = report_two_track(tmp, lag, -35.0)
    print(f"[SELFTEST] 期待: 遅延 {lag} / 減衰 中央値 {atten_db:+.1f} dB 前後")
    return rc


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--two-track", type=Path, default=None,
                    help="OBS の 2 トラック録音 (ch0=処理後, ch1=参照)")
    ap.add_argument("--input", type=Path, default=None, help="鳴らしたファイル")
    ap.add_argument("--capture", type=Path, default=None, help="OBS の録音")
    ap.add_argument("--below-db", type=float, default=-35.0,
                    help="低活動区間とみなす閾値 (ピークからの相対 dB)")
    ap.add_argument("--expect-lag", type=int, default=EXPECT)
    ap.add_argument("--selftest", action="store_true",
                    help="既知の遅延/減衰を仕込んで測定器を検証する")
    ap.add_argument("--selftest-source", type=Path,
                    default=Path("test_signals/speech_tail_reverb_48k.wav"))
    ap.add_argument("--selftest-lag", type=int, default=EXPECT)
    ap.add_argument("--selftest-atten-db", type=float, default=-12.0)
    args = ap.parse_args()

    if args.selftest:
        return selftest(args.selftest_source, args.selftest_lag, args.selftest_atten_db)
    if args.two_track:
        return report_two_track(args.two_track, args.expect_lag, args.below_db)
    if args.input and args.capture:
        return report_input_capture(args.input, args.capture, args.below_db, args.expect_lag)
    print("ERROR: --two-track か (--input と --capture) か --selftest が必要", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
