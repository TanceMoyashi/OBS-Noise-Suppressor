#!/usr/bin/env python3
"""新モデル (dp4) と既存 dp2 / dp8 を「実際の ONNX」で聴き比べ・数値比較する。

やること:
  * 手元のきれいな音声 (`test_signals/speech_tail_48k.wav` など) に合成ノイズを混ぜる
  * 3 つの 2 出力 ONNX をストリーミングで流す (プラグインと同じ手順)
  * 指標を出す:
      - 入力 SNR (ノイズ入り vs クリーン)
      - 出力 SNR (各モデル vs クリーン)  … 高いほどよく抑圧できている
      - dp8 との一致 (各モデル vs dp8 の出力) … 高いほど dp8 に近い
  * 聴き比べ用の WAV を出力

注意: dp4 の学習は LibriSpeech dev-clean の全発話を使ったので、ここで使う
`test_signals/*.wav` (プラグイン付属の自前録音) は学習に使っていない = ホールドアウト。

usage:
  python tools/dpdfnet_mid/quality_report.py \
      --models dp2=models/dpdfnet2_48khz_hr_2out.onnx \
               dp4=models/dpdfnet4_48khz_hr_2out.onnx \
               dp8=models/dpdfnet8_48khz_hr_2out.onnx \
      --wav test_signals/speech_tail_48k.wav --out-dir build/dpdfnet_train/listen
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np
import onnxruntime as ort

# tools/ (check_onnx_model.py) を import できるようにする
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from check_onnx_model import (MODEL_FRAME_DELAY, istft_frames, naive_resample, read_wav,  # noqa: E402
                              stft_frames)

SR = 48000


def session(path: Path) -> ort.InferenceSession:
    so = ort.SessionOptions()
    so.intra_op_num_threads = 1
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    so.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    so.log_severity_level = 3
    return ort.InferenceSession(str(path), sess_options=so, providers=["CPUExecutionProvider"])


def run_stream(sess: ort.InferenceSession, spec: np.ndarray) -> np.ndarray:
    """[T, F] complex を流して spec_e を [T, F] complex で返す (メタデータから state を初期化)。"""
    ins = sess.get_inputs()
    names = [o.name for o in sess.get_outputs()]
    fb = int(ins[0].shape[-2])
    meta = sess.get_modelmeta().custom_metadata_map
    ss = int(meta["state_size"])
    erb, sp = int(meta["erb_norm_state_size"]), int(meta["spec_norm_state_size"])
    state = np.zeros(ss, np.float32)
    state[:erb] = np.array([float(v) for v in meta["erb_norm_init"].split(",")], np.float32)
    state[erb:erb + sp] = np.array([float(v) for v in meta["spec_norm_init"].split(",")], np.float32)

    ri = np.zeros((1, 1, fb, 2), np.float32)
    out = np.zeros((spec.shape[0], fb), np.complex64)
    for t in range(spec.shape[0]):
        ri[0, 0, :, 0] = spec[t].real
        ri[0, 0, :, 1] = spec[t].imag
        got = sess.run(["spec_e", "state_out"], {ins[0].name: ri, ins[1].name: state})
        by = dict(zip(["spec_e", "state_out"], got))
        out[t] = by["spec_e"][0, 0, :, 0] + 1j * by["spec_e"][0, 0, :, 1]
        state = by["state_out"]
    return out


def synth_noise(kind: str, n: int, rng: np.random.Generator, babble: list[np.ndarray]) -> np.ndarray:
    if kind == "white":
        return rng.standard_normal(n).astype(np.float32)
    if kind == "pink":
        X = np.fft.rfft(rng.standard_normal(n))
        X /= (np.arange(len(X)) + 1.0) ** 0.5
        return np.fft.irfft(X, n=n).astype(np.float32)
    if kind == "hum":
        t = np.arange(n) / SR
        y = 0.45 * np.sin(2 * np.pi * 50 * t) + 0.2 * np.sin(2 * np.pi * 150 * t)
        return (y + 0.05 * rng.standard_normal(n)).astype(np.float32)
    if kind == "babble":
        y = np.zeros(n, np.float32)
        for s in babble:
            take = min(n, len(s))
            off = 0 if len(s) <= n else int(rng.integers(0, len(s) - n))
            y[:take] += s[off:off + take]
        return y
    raise ValueError(kind)


def snr_db(ref: np.ndarray, est: np.ndarray) -> float:
    n = min(len(ref), len(est))
    r, e = ref[:n].astype(np.float64), est[:n].astype(np.float64)
    return 10.0 * np.log10((np.sum(r * r) + 1e-30) / (np.sum((r - e) ** 2) + 1e-30))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="+", required=True, help="name=path")
    ap.add_argument("--wav", type=Path, nargs="+",
                    default=[Path("test_signals/speech_tail_48k.wav"),
                             Path("test_signals/speech_tail_reverb_48k.wav")])
    ap.add_argument("--snr", type=float, nargs="+", default=[5.0, 12.0])
    ap.add_argument("--kinds", nargs="+", default=["white", "pink", "hum", "babble"])
    ap.add_argument("--babble-wav", type=Path, nargs="*", default=None)
    ap.add_argument("--out-dir", type=Path, default=None)
    ap.add_argument("--write-wav", action="store_true")
    args = ap.parse_args()

    models = []
    for spec in args.models:
        name, path = spec.split("=", 1)
        models.append((name, Path(path), session(Path(path))))
        print(f"[model] {name}: {path}")

    # バブル用の音声 (プラグイン付属の WAV を重ねる)
    babble_src = args.babble_wav or list(args.wav)
    babble = []
    for w in babble_src:
        if Path(w).is_file():
            b, bsr = read_wav(Path(w))
            babble.append(naive_resample(b, bsr, SR).astype(np.float32))

    row_fmt = "{:26s}" + "{:>12s}" * (len(models) + 2)
    print()
    print(row_fmt.format("入力 / モデル", "in SNR", "dp8との一致",
                         *[m[0] + " outSNR" for m in models]))
    print("-" * 100)

    results = []
    rng = np.random.default_rng(2026)
    for w in args.wav:
        clean, sr = read_wav(Path(w))
        clean = naive_resample(clean, sr, SR).astype(np.float32)
        # 先頭に無音を足して定常状態を作る
        pad = np.zeros(4800, np.float32)
        for kind in args.kinds:
            for target in args.snr:
                nz = synth_noise(kind, len(clean), rng, babble)
                g = np.sqrt(np.mean(clean ** 2)) / (np.sqrt(np.mean(nz ** 2)) + 1e-12)
                g *= 10 ** (-target / 20.0)
                noisy = (clean + g * nz).astype(np.float32)
                x = np.concatenate([pad, noisy])
                spec_in = stft_frames(x, 960, 480)

                outs = {}
                for name, _, sess in models:
                    e = run_stream(sess, spec_in)
                    y = istft_frames(e, 960, 480)[MODEL_FRAME_DELAY * 480:]
                    outs[name] = y[len(pad):len(pad) + len(clean)]

                ref = outs.get("dp8")
                label = f"{Path(w).stem[:14]}/{kind}/+{target:.0f}dB"
                line = {"label": label, "kind": kind, "snr": target, "wav": Path(w).name,
                        "in_snr": snr_db(clean, noisy)}
                for name, _, _ in models:
                    line[name + "_out_snr"] = snr_db(clean, outs[name])
                    line[name] = outs[name]
                line["dp8_agree"] = {}
                for name, _, _ in models:
                    line["dp8_agree"][name] = snr_db(ref, outs[name]) if ref is not None else float("nan")
                print(row_fmt.format(
                    label, f"{line['in_snr']:.2f}",
                    f"{line['dp8_agree'].get('dp4', float('nan')):.2f}",
                    *[f"{line[m[0]+'_out_snr']:.2f}" for m in models]))
                results.append(line)

                if args.write_wav and args.out_dir is not None:
                    out_dir = Path(args.out_dir)
                    out_dir.mkdir(parents=True, exist_ok=True)
                    import wave as wavelib

                    def save(path: Path, y: np.ndarray) -> None:
                        y = np.clip(y, -1, 1)
                        with wavelib.open(str(path), "wb") as f:
                            f.setnchannels(1)
                            f.setsampwidth(2)
                            f.setframerate(SR)
                            f.writeframes((y * 32767).astype("<i2").tobytes())

                    tag = f"{Path(w).stem}_{kind}_{target:.0f}dB"
                    save(out_dir / f"{tag}_clean.wav", clean)
                    save(out_dir / f"{tag}_noisy.wav", noisy)
                    for name, _, _ in models:
                        save(out_dir / f"{tag}_{name}.wav", outs[name])

    print("\n[平均]")
    for name, _, _ in models:
        vals = [r[name + "_out_snr"] for r in results]
        ag = [r["dp8_agree"][name] for r in results if "dp8" in r["dp8_agree"]]
        print(f"  {name}: 出力 SNR vs クリーン = {np.mean(vals):+.2f} dB   "
              f"dp8 との一致 = {np.mean(ag):.2f} dB")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
