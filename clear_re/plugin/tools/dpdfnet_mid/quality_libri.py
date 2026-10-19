#!/usr/bin/env python3
"""LibriSpeech (クリーン参照あり) + 合成ノイズで 3 モデルの抑圧性能を測る。

`quality_report.py` はプラグイン付属の WAV (ノイズ入り/残響入りでクリーン参照が無い) を
使うのに対し、こちらは**クリーン参照がある**ので「出力 SNR」が意味を持つ。

注意 (正直に書いておく): LibriSpeech dev-clean は dp4 の蒸留に使った。つまり
dp4 にとっては「学習に近いデータ」で、dp2 / dp8 にとっては未知データ。したがって
この表は dp4 に有利に出る可能性がある。学習に使っていない音声での比較は
`quality_report.py` (dp8 との一致度) と `transparency` (クリーン音声をどれだけ
素通しするか) を参照。

usage:
  python tools/dpdfnet_mid/quality_libri.py \
      --models dp2=models/dpdfnet2_48khz_hr_2out.onnx \
               dp4=models/dpdfnet4_48khz_hr_2out.onnx \
               dp8=models/dpdfnet8_48khz_hr_2out.onnx --n 6
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_onnx_model import MODEL_FRAME_DELAY, istft_frames, stft_frames  # noqa: E402
from quality_report import run_stream, session, snr_db, synth_noise  # noqa: E402

SR = 48000


def load_utts(root: Path, n: int, seconds: float = 8.0) -> list[tuple[int, np.ndarray]]:
    meta = json.loads((root / f"speech_{SR}.json").read_text(encoding="utf-8"))
    raw = np.memmap(root / meta["raw_file"], dtype=np.float32, mode="r")
    lengths = np.asarray(meta["lengths"], np.int64)
    offs = np.concatenate([[0], np.cumsum(lengths)])
    # 決定論的に「長い順」から選ぶ (学習は全発話をランダム窓で使っている)
    order = np.argsort(-lengths)
    need = int(seconds * SR)
    out = []
    for i in order:
        if lengths[i] >= need:
            out.append((int(i), np.asarray(raw[offs[i]:offs[i] + need], np.float32)))
        if len(out) >= n:
            break
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="+", required=True, help="name=path")
    ap.add_argument("--root", type=Path, default=Path("build/dpdfnet_train"))
    ap.add_argument("--n", type=int, default=6)
    ap.add_argument("--seconds", type=float, default=8.0)
    ap.add_argument("--snr", type=float, nargs="+", default=[0.0, 10.0])
    ap.add_argument("--kinds", nargs="+", default=["white", "babble"])
    ap.add_argument("--out-dir", type=Path, default=None)
    ap.add_argument("--write-wav", action="store_true")
    args = ap.parse_args()

    models = []
    for spec in args.models:
        name, path = spec.split("=", 1)
        models.append((name, session(Path(path))))
        print(f"[model] {name}: {path}")

    utts = load_utts(args.root, args.n)
    print(f"[data] LibriSpeech dev-clean から {len(utts)} 発話 x {args.seconds:.0f} s")

    babble = [u for _, u in utts]
    rng = np.random.default_rng(4242)
    fmt = "{:22s}" + "{:>10s}" * (len(models) + 2)
    print()
    print(fmt.format("条件", "in SNR", "dp8との一致", *[f"{m[0]} outSNR" for m in models]))
    print("-" * 96)
    rows = []
    for idx, clean in utts:
        for kind in args.kinds:
            for target in args.snr:
                nz = synth_noise(kind, len(clean), rng, babble)
                g = np.sqrt(np.mean(clean ** 2)) / (np.sqrt(np.mean(nz ** 2)) + 1e-12)
                g *= 10 ** (-target / 20.0)
                noisy = (clean + g * nz).astype(np.float32)
                pad = np.zeros(4800, np.float32)
                spec_in = stft_frames(np.concatenate([pad, noisy]), 960, 480)
                outs = {}
                for name, sess in models:
                    y = istft_frames(run_stream(sess, spec_in), 960, 480)
                    y = y[MODEL_FRAME_DELAY * 480:]
                    outs[name] = y[len(pad):len(pad) + len(clean)]
                ref = outs["dp8"]
                row = {"utt": idx, "kind": kind, "snr": target, "in": snr_db(clean, noisy)}
                for name, _ in models:
                    row[name] = snr_db(clean, outs[name])
                    row[name + "_agree"] = snr_db(ref, outs[name])
                rows.append(row)
                print(fmt.format(
                    f"utt{idx}/{kind}/+{target:.0f}dB", f"{row['in']:.2f}",
                    f"{row['dp4_agree']:.2f}", *[f"{row[m[0]]:.2f}" for m in models]))

                if args.write_wav and args.out_dir is not None and len(rows) == 1:
                    import wave as wavelib

                    out_dir = Path(args.out_dir)
                    out_dir.mkdir(parents=True, exist_ok=True)

                    def save(p: Path, y: np.ndarray) -> None:
                        y = np.clip(y, -1, 1)
                        with wavelib.open(str(p), "wb") as f:
                            f.setnchannels(1)
                            f.setsampwidth(2)
                            f.setframerate(SR)
                            f.writeframes((y * 32767).astype("<i2").tobytes())

                    save(out_dir / "libri_clean.wav", clean)
                    save(out_dir / "libri_noisy.wav", noisy)
                    for name, _ in models:
                        save(out_dir / f"libri_{name}.wav", outs[name])

    print("\n[平均]")
    for name, _ in models:
        print(f"  {name}: 出力 SNR vs クリーン = {np.mean([r[name] for r in rows]):+.2f} dB   "
              f"dp8 との一致 = {np.mean([r[name+'_agree'] for r in rows]):.2f} dB")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
