#!/usr/bin/env python3
"""きれいな音声を「素通し」させたときの入出力 SNR（透明性）を測る。

ノイズ抑圧モデルは、入力が既にきれいなら何もしない（素通し）のが望ましい。
値が高いほど「余計な加工をしない」= 音を壊さない。dp8 は de-reverb のために
あえて加工するので最も低くなる（= 悪いという意味ではない）。

クリーン参照が要らないので、学習に使っていない音声でも測れる。
データは LibriSpeech dev-clean（クリーン音声）を使う。

usage:
  python tools/dpdfnet_mid/transparency.py \
      --models dp2=models/dpdfnet2_48khz_hr_2out.onnx \
               dp4=models/dpdfnet4_48khz_hr_2out.onnx \
               dp8=models/dpdfnet8_48khz_hr_2out.onnx --n 4
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_onnx_model import MODEL_FRAME_DELAY, istft_frames, stft_frames  # noqa: E402
from quality_libri import load_utts  # noqa: E402
from quality_report import run_stream, session, snr_db  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="+", required=True, help="name=path")
    ap.add_argument("--root", type=Path, default=Path("build/dpdfnet_train"))
    ap.add_argument("--n", type=int, default=4)
    ap.add_argument("--seconds", type=float, default=8.0)
    args = ap.parse_args()

    models = []
    for spec in args.models:
        name, path = spec.split("=", 1)
        models.append((name, session(Path(path))))

    utts = load_utts(args.root, args.n, args.seconds)
    print(f"[data] LibriSpeech dev-clean {len(utts)} 発話 x {args.seconds:.0f} s")
    print()
    fmt = "{:12s}" + "{:>14s}" * len(models)
    print(fmt.format("発話", *[m[0] + " (dB)" for m in models]))
    print("-" * (12 + 14 * len(models)))
    acc = {m[0]: [] for m in models}
    for idx, clean in utts:
        spec_in = stft_frames(clean, 960, 480)
        vals = []
        for name, sess in models:
            y = istft_frames(run_stream(sess, spec_in), 960, 480)[MODEL_FRAME_DELAY * 480:]
            n = min(len(clean), len(y))
            v = snr_db(clean[:n], y[:n])
            acc[name].append(v)
            vals.append(v)
        print(fmt.format(f"utt{idx}", *[f"{v:.2f}" for v in vals]))
    print("\n[平均] " + "  ".join(f"{m[0]}={np.mean(acc[m[0]]):.2f} dB" for m in models))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
