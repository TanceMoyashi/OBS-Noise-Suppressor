#!/usr/bin/env python3
"""モデルの実測診断 (契約検証の補助ツール)。

確認すること:

  1. 公式実装 (dpdfnet.enhance: center=True + reflect + 先頭 4 hop の遅延補正) と
     自前の素朴な経路 (check_onnx_model.py と同じ枠組み) の入出力レベルを比べる。
     -> 片方だけレベルが大きく違うなら、遅延補正か枠組みのバグ。
  2. フレーム単位のスペクトル照合でモデルのフレーム遅延 d を推定する。
     -> 公式コードは 4 フレーム (ATTN_LIMIT_NOISY_FRAME_OFFSET) を前提にしている。
        C++ 側 (PluginProcessor の kLatencyHops) はこの値に依存する。

依存: numpy, onnxruntime, dpdfnet (公式パッケージ。無くても 1 の後半をスキップ)
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np
import onnxruntime as ort

from check_onnx_model import istft_frames, stft_frames, synth_test_signal


def make_session(model_path: Path) -> ort.InferenceSession:
    so = ort.SessionOptions()
    so.intra_op_num_threads = 1
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    return ort.InferenceSession(str(model_path), sess_options=so,
                                providers=["CPUExecutionProvider"])


def load_meta(sess: ort.InferenceSession) -> dict:
    meta = sess.get_modelmeta().custom_metadata_map
    state_size = int(meta["state_size"])
    erb = int(meta["erb_norm_state_size"])
    spec = int(meta["spec_norm_state_size"])
    init = np.zeros(state_size, dtype=np.float32)
    init[0:erb] = np.array([float(v) for v in meta["erb_norm_init"].split(",")],
                           dtype=np.float32)
    init[erb:erb + spec] = np.array([float(v) for v in meta["spec_norm_init"].split(",")],
                                    dtype=np.float32)
    return {"state_size": state_size, "init": init}


def rms(v: np.ndarray) -> float:
    v = np.asarray(v, dtype=np.float64)
    return float(np.sqrt(np.mean(v * v))) if v.size else 0.0


def run_frames(sess, init_state, spec_in: np.ndarray, freq_bins: int):
    """spec_in [T,F] complex -> (spec_out [T,F] complex, per-frame in/out energy)"""
    spec_ri = np.zeros((1, 1, freq_bins, 2), dtype=np.float32)
    state = init_state.copy()
    out = []
    e_in, e_out = [], []
    for t in range(spec_in.shape[0]):
        spec_ri[0, 0, :, 0] = spec_in[t].real
        spec_ri[0, 0, :, 1] = spec_in[t].imag
        spec_e, state = sess.run(["spec_e", "state_out"],
                                 {"spec": spec_ri, "state_in": state})
        ri = spec_e[0, 0]
        out.append(ri.copy())
        e_in.append(float(np.sum(np.abs(spec_in[t]) ** 2)))
        e_out.append(float(np.sum(ri[:, 0] ** 2 + ri[:, 1] ** 2)))
    spec_out = np.stack(out, axis=0).astype(np.float32)
    return spec_out, np.array(e_in), np.array(e_out)


def estimate_frame_delay(spec_in: np.ndarray, spec_out: np.ndarray,
                         valid_from: int = 10) -> list[tuple[int, float]]:
    """各出力フレームに最も近い入力フレームを探し、その差 (遅延) を集計する。

    ゲイン変化に強いよう、正規化した大きさスペクトル同士の相関で照合する。
    """
    mi = np.abs(spec_in)
    mo = np.abs(spec_out)
    mi /= (np.linalg.norm(mi, axis=1, keepdims=True) + 1e-12)
    mo /= (np.linalg.norm(mo, axis=1, keepdims=True) + 1e-12)

    hits: list[tuple[int, float]] = []
    for t in range(valid_from, spec_out.shape[0]):
        if np.linalg.norm(mo[t]) < 1e-9:
            continue
        sims = mi @ mo[t]
        k = int(np.argmax(sims))
        hits.append((t - k, float(sims[k])))
    return hits


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", type=Path, default=Path("models/dpdfnet2_48khz_hr.onnx"))
    ap.add_argument("--wav", type=Path, default=None,
                    help="実音声 WAV (あれば合成信号の代わりに使う)")
    ap.add_argument("--seconds", type=float, default=3.0)
    args = ap.parse_args()

    if not args.model.is_file():
        print(f"ERROR: model not found: {args.model}", file=sys.stderr)
        return 1

    sess = make_session(args.model)
    info = load_meta(sess)
    freq_bins = int(sess.get_inputs()[0].shape[-2])
    win_len = (freq_bins - 1) * 2
    hop = win_len // 2
    print(f"[IO] freq_bins={freq_bins} win_len={win_len} hop={hop} "
          f"state_size={info['state_size']}")

    # ---- 入力 ----
    sr = 48000
    if args.wav is not None:
        import wave
        with wave.open(str(args.wav), "rb") as w:
            raw = w.readframes(w.getnframes())
            sr_in = w.getframerate()
            width = w.getsampwidth()
            nch = w.getnchannels()
        if width == 2:
            x = np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0
        else:
            x = np.frombuffer(raw, dtype="<i4").astype(np.float32) / 2147483648.0
        if nch > 1:
            x = x.reshape(-1, nch).mean(axis=1)
        if sr_in != sr:
            t_in = np.arange(len(x), dtype=np.float64)
            n_out = int(round(len(x) * sr / sr_in))
            x = np.interp(np.linspace(0.0, len(x) - 1.0, n_out), t_in, x).astype(np.float32)
        print(f"[WAV] {args.wav.name}: {len(x)} samples @ {sr} Hz")
    else:
        x = synth_test_signal(sr, args.seconds)
        print(f"[WAV] synthetic (voice + noise + reverb): {len(x)} samples @ {sr} Hz")

    spec_in = stft_frames(x, win_len, hop)
    print(f"[STFT] {spec_in.shape[0]} frames (center=True, reflect)")

    # ---- 1a. 自前の素朴な経路 (遅延補正なし) ----
    # istft_frames は center=True の解析に対応して先頭 win_len//2 を捨てるので、
    # 座標系は torch.istft(center=True) と同じ。モデル出力の遅延がそのまま出る。
    spec_out, e_in, e_out = run_frames(sess, info["init"], spec_in, freq_bins)
    y_naive = istft_frames(spec_out[..., 0] + 1j * spec_out[..., 1], win_len, hop)

    # ---- 1b. 公式コードと同じく先頭 2*win_len を捨てる (= 4 hop = 1920 の遅延補正) ----
    adv = 2 * win_len
    y_fixed = np.concatenate([y_naive[adv:], np.zeros(adv, dtype=np.float32)])

    n = min(len(x), len(y_fixed))

    def lag_of(a: np.ndarray, b: np.ndarray) -> tuple[int, float]:
        m = min(len(a), len(b))
        aa = a[:m] - a[:m].mean()
        bb = b[:m] - b[:m].mean()
        den = (np.linalg.norm(aa) * np.linalg.norm(bb)) or 1.0
        c = np.correlate(bb, aa, mode="full") / den
        return int(np.argmax(np.abs(c)) - (m - 1)), float(np.max(np.abs(c)))

    l0, c0 = lag_of(x, y_naive)
    l1, c1 = lag_of(x, y_fixed)
    print(f"[LEVEL] in={rms(x):.6f}  naive out={rms(y_naive):.6f} ({20*np.log10(rms(y_naive)/rms(x)):+.2f} dB)")
    print(f"[LEVEL] in={rms(x):.6f}  delayed-compensated out={rms(y_fixed):.6f} "
          f"({20*np.log10(rms(y_fixed)/rms(x)):+.2f} dB)")
    print(f"[LAG] naive (no compensation)      = {l0} samples (corr {c0:.3f})")
    print(f"[LAG] after dropping 2*win_len     = {l1} samples (corr {c1:.3f})")

    # ---- フレーム単位のエネルギー比 ----
    ratio = e_out / np.maximum(e_in, 1e-20)
    print(f"[GAIN] frame energy out/in: median={np.median(ratio):.4f} "
          f"({10*np.log10(np.median(ratio) + 1e-20):+.1f} dB)  "
          f"p90={np.percentile(ratio, 90):.4f}")
    spec_out_c = spec_out[..., 0] + 1j * spec_out[..., 1]
    frame_lags = np.array([h[0] for h in estimate_frame_delay(spec_in, spec_out_c)])
    if frame_lags.size:
        vals, counts = np.unique(frame_lags, return_counts=True)
        top = sorted(zip(counts, vals), reverse=True)[:5]
        print("[DELAY] most frequent spectral-match frame lags: "
              + ", ".join(f"{v} frames ({c}x)" for c, v in top))
        print(f"[DELAY] median frame lag = {int(np.median(frame_lags))} frames "
              f"(= {int(np.median(frame_lags)) * hop} samples)")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
