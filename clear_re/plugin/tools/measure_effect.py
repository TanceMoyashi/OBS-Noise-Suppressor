#!/usr/bin/env python3
"""2 出力モデルでテスト信号を処理し、「期待値」を数値で出す。

OBS で聴く前に、プラグインが**出すはずの数字**をここで確定しておく。
指標は tools/analyze_obs_capture.py と同じ関数を使うので、そのまま比較できる。

    - 全体 RMS (入力比 dB)
    - 低活動区間 (無音ギャップ) の減衰 dB ← ノイズ抑圧 + 残響抑圧の総合効果
    - Voice Reverb = 0 (spec_ns = マスクのみ) と 1 (spec_e = マスク + DF) の差
      ← これが de-reverb の効き幅

usage:
  python tools/measure_effect.py --model models/dpdfnet2_48khz_hr_2out.onnx \
      --wav test_signals/speech_tail_reverb_48k.wav
  python tools/measure_effect.py --model models/dpdfnet8_48khz_hr_2out.onnx \
      --wav test_signals/speech_tail_48k.wav      # 残響なし (ノイズ抑圧だけを見る)
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np
import soundfile as sf

from analyze_obs_capture import (SILENCE_FLOOR, db, low_activity_regions, region_stats,
                                 rms, summarize_region_rows)
from check_onnx_model import MODEL_FRAME_DELAY, istft_frames, naive_resample, stft_frames
from verify_two_output import make_session, run


def to_complex(a: np.ndarray) -> np.ndarray:
    return (a[..., 0] + 1j * a[..., 1]).astype(np.complex64)


def load_mono(path: Path) -> tuple[np.ndarray, int]:
    """soundfile で読む (PCM_16 でも float32 でも可)。"""
    x, sr = sf.read(str(path), always_2d=True)
    return x.mean(axis=1).astype(np.float32), int(sr)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True, type=Path)
    ap.add_argument("--wav", required=True, type=Path)
    ap.add_argument("--below-db", type=float, default=-35.0)
    ap.add_argument("--lead-silence", type=float, default=0.1,
                    help="先頭に足す無音 [秒] (相関と区間検出の安定用)")
    args = ap.parse_args()

    for p in (args.model, args.wav):
        if not p.is_file():
            print(f"ERROR: not found: {p}", file=sys.stderr)
            return 1

    sess = make_session(args.model)
    names = [o.name for o in sess.get_outputs()]
    if "spec_ns" not in names:
        print(f"ERROR: {args.model.name} に spec_ns が無い (1 出力モデル)。"
              f"OBS では Voice Reverb がグレーアウトする。", file=sys.stderr)
        return 1

    ins = sess.get_inputs()
    freq_bins = int(ins[0].shape[-2])
    win_len = (freq_bins - 1) * 2
    hop = win_len // 2

    x, sr = load_mono(args.wav)
    if sr != 48000:
        x = naive_resample(x, sr, 48000).astype(np.float32)
    x = np.concatenate([np.zeros(int(48000 * args.lead_silence), np.float32), x])
    spec_in = stft_frames(x, win_len, hop)
    print(f"[MODEL] {args.model.name}  [WAV] {args.wav.name}")
    print(f"[WAV] {len(x)} samples ({len(x)/48000:.2f} s), {spec_in.shape[0]} frames")

    e, ns, _ = run(sess, spec_in, want_ns=True)
    assert ns is not None

    drop = MODEL_FRAME_DELAY * hop
    out = {}
    for beta, label in ((0.0, "Voice Reverb = 0  (spec_ns: マスクのみ)"),
                        (1.0, "Voice Reverb = 1  (spec_e : マスク + DF)")):
        blended = ((1.0 - beta) * ns + beta * e).astype(np.float32)
        y = istft_frames(to_complex(blended), win_len, hop)[drop:]
        out[beta] = y

    # 指標 (analyze_obs_capture.py と同じ関数)
    ref = x[: min(len(out[0.0]), len(x))]
    regions = low_activity_regions(ref, 48000, args.below_db)
    print(f"\n[区間] 低活動 (peak-{abs(args.below_db):.0f} dB 未満) = {len(regions)} 箇所")
    results = {}
    for beta in (0.0, 1.0):
        y = out[beta][: len(ref)]
        rows = region_stats(ref, y, regions, 48000)
        rel = [d for r, o, d in rows if r >= SILENCE_FLOOR]
        results[beta] = float(np.median(rel)) if rel else float("nan")
        print(f"\n  Voice Reverb = {int(beta)}   全体 {db(rms(y))-db(rms(ref)):+6.2f} dB")
        for line in summarize_region_rows(rows).splitlines():
            print("      " + line)

    if not np.isnan(results[0.0]) and not np.isnan(results[1.0]):
        delta = results[1.0] - results[0.0]
        print(f"\n[de-reverb の効き幅] {delta:+.2f} dB "
              f"(Voice Reverb 0 -> 1 で低活動区間がさらに {abs(delta):.2f} dB 下がる)")
    else:
        print("\n[NOTE] 低活動区間がデジタル無音なので「減衰量」は定義できない。"
              "上の「出力 = ... dBFS」が、プラグインが漏らした量そのもの。"
              "\n       残響つき (speech_tail_reverb_48k.wav) で測ると de-reverb の効き幅が出る。")
    print("\n[NOTE] OBS で同じファイルを鳴らして録れば、"
          "analyze_obs_capture.py が同じ指標を出す。数字が近ければ正常。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
