#!/usr/bin/env python3
"""OBS 実機確認用のテスト信号を作る。

生成物 (`test_signals/`):

  impulse_48k.wav
      0.5 s / 1.0 s / 1.5 s に 1 サンプルのインパルス。レイテンシ測定用。
      モデルは非音声を大きく抑圧するので、確定測定は Bypass ON で行う
      (Bypass も同じ遅延を保つ実装なので、通過位置がそのまま実遅延になる)。

  speech_tail_48k.wav
      音声 (sherpa の speech_with_noise を 48 kHz 化) を ~2 秒ずつに切り、
      間に 0.6 秒の**デジタル無音**を挿入したもの。
      → 無音区間 = 「プラグインが何も出してはいけない区間」。
        ノイズ抑圧と残響抑圧の効果がここに現れる。

  speech_tail_reverb_48k.wav
      上を合成 RIR (T60 ≈ 0.6 s) で畳み込んだもの。
      → 無音区間が残響テールで埋まる。de-reverb の測定用。

  rir_48k.wav
      使った合成 RIR (確認用)。

使い方:
  python tools/make_test_signals.py                 # test_signals/speech_with_noise.wav を使う
  python tools/make_test_signals.py --speech <wav>  # 手持ちの音声を使う

依存: numpy, scipy, soundfile
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np
import soundfile as sf
from scipy import signal

SR = 48000


# ---------------------------------------------------------------------------
def read_mono(path: Path) -> tuple[np.ndarray, int]:
    x, sr = sf.read(str(path), always_2d=False)
    if x.ndim > 1:
        x = x.mean(axis=1)
    return x.astype(np.float32), int(sr)


def to_48k(x: np.ndarray, sr: int) -> np.ndarray:
    """有理リサンプリング (16k/44.1k -> 48k)。"""
    if sr == SR:
        return x.astype(np.float32)
    g = np.gcd(int(sr), SR)
    return signal.resample_poly(x, SR // g, int(sr) // g).astype(np.float32)


def normalize_peak(x: np.ndarray, peak: float = 0.89) -> np.ndarray:
    m = float(np.max(np.abs(x))) or 1.0
    return (x * (peak / m)).astype(np.float32)


def fade(x: np.ndarray, ms: float = 5.0) -> np.ndarray:
    n = max(1, int(SR * ms / 1000.0))
    x = x.copy()
    w = np.linspace(0.0, 1.0, n, dtype=np.float32)
    x[:n] *= w
    x[-n:] *= w[::-1]
    return x


# ---------------------------------------------------------------------------
def make_impulse() -> np.ndarray:
    x = np.zeros(int(SR * 2.0), np.float32)
    for t in (0.5, 1.0, 1.5):
        x[int(SR * t)] = 0.5
    return x


def join_sources(paths: list[Path]) -> np.ndarray:
    """複数の音声を 48 kHz / 正規化して連結する (素材を長くするため)。"""
    parts = []
    for p in paths:
        y, sr = read_mono(p)
        y = normalize_peak(to_48k(y, sr), 0.7)
        parts.append(fade(y, 20.0))
        print(f"     + {p}  {len(y)/SR:.2f} s @ {SR} Hz")
    if not parts:
        return np.zeros(0, np.float32)
    return normalize_peak(np.concatenate(parts))


def split_with_gaps(x: np.ndarray, seg_s: float = 2.0, gap_s: float = 0.6) -> np.ndarray:
    """seg_s ごとに切り、間に gap_s のデジタル無音を挿入する。"""
    seg = int(SR * seg_s)
    gap = np.zeros(int(SR * gap_s), np.float32)
    out = [gap.copy()]                       # 先頭にも無音 (起動過渡を避ける)
    for s in range(0, len(x), seg):
        part = x[s:s + seg]
        if len(part) < int(SR * 0.2):
            break
        out.append(fade(part))
        out.append(gap.copy())
    return np.concatenate(out).astype(np.float32)


def make_rir(t60: float = 0.6) -> np.ndarray:
    """合成 RIR。直接音 (0 dB) + 指数減衰する拡散反射。

    残響だけを測りたいので、直接音は 1 サンプルに寄せる (early reflection は薄く)。
    """
    n = int(SR * 0.8)
    rng = np.random.default_rng(12345)
    t = np.arange(n, dtype=np.float32) / SR
    decay = 10.0 ** (-3.0 * t / t60)          # -60 dB になる時間が t60
    tail = rng.standard_normal(n).astype(np.float32) * decay
    # 拡散反射の立ち上がりを少し滑らかに
    tail = np.convolve(tail, np.ones(8, np.float32) / 8.0, mode="same").astype(np.float32)
    rir = tail
    rir[:12] *= 0.3                            # 直接音の直後を抑えて「残響主体」に
    rir[0] = 1.0                               # 直接音
    rir /= np.max(np.abs(rir)) or 1.0
    # 尾を 0 に向けて滑らかに切る
    f = np.linspace(1.0, 0.0, n, dtype=np.float32) ** 2
    return (rir * f).astype(np.float32)


# ---------------------------------------------------------------------------
def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--speech", nargs="+", type=Path, default=None,
                    help="入力音声 (複数指定すると連結する)")
    ap.add_argument("--out-dir", type=Path, default=Path("test_signals"))
    ap.add_argument("--t60", type=float, default=0.6)
    ap.add_argument("--seg", type=float, default=2.0)
    ap.add_argument("--gap", type=float, default=0.6)
    args = ap.parse_args()

    default_speech = [Path("test_signals/inp_16k.wav"), Path("test_signals/speech_with_noise.wav")]
    speech = args.speech if args.speech else [p for p in default_speech if p.is_file()]
    missing = [p for p in speech if not p.is_file()]
    if not speech or missing:
        print(f"ERROR: speech not found: {missing or speech}", file=sys.stderr)
        print("       test_signals/ の wav を README §3 の手順で取得するか、"
              "--speech で手持ちの音声を指定する。", file=sys.stderr)
        return 1

    args.out_dir.mkdir(parents=True, exist_ok=True)

    # 1) インパルス
    imp = make_impulse()
    sf.write(str(args.out_dir / "impulse_48k.wav"), imp, SR, subtype="PCM_16")
    print(f"[OK] impulse_48k.wav            ({len(imp)} samples, {len(imp)/SR:.2f} s)")

    # 2) 音声 + 無音ギャップ
    print("[..] 音声素材:")
    x = join_sources(speech)
    if len(x) < SR:
        print("ERROR: 音声素材が短すぎる", file=sys.stderr)
        return 1
    seg = split_with_gaps(x, args.seg, args.gap)
    sf.write(str(args.out_dir / "speech_tail_48k.wav"), seg, SR, subtype="PCM_16")
    n_gap = int(np.sum(seg == 0.0))
    print(f"[OK] speech_tail_48k.wav        ({len(seg)} samples, {len(seg)/SR:.2f} s, "
          f"無音 {n_gap/SR:.2f} s = {100.0*n_gap/len(seg):.0f}%)")

    # 3) RIR 畳み込み (残響つき)
    rir = make_rir(args.t60)
    sf.write(str(args.out_dir / "rir_48k.wav"), rir, SR, subtype="PCM_16")
    wet = signal.fftconvolve(seg, rir)[: len(seg)].astype(np.float32)
    wet = normalize_peak(wet)
    sf.write(str(args.out_dir / "speech_tail_reverb_48k.wav"), wet, SR, subtype="PCM_16")
    print(f"[OK] speech_tail_reverb_48k.wav ({len(wet)} samples, T60={args.t60} s)")
    print(f"[OK] rir_48k.wav                ({len(rir)} samples)")

    print("\n[次の手順] OBS_VERIFY.md の「1. テスト信号を鳴らす」以降を実行する。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
