#!/usr/bin/env python3
"""2 出力化した DPDFNet ONNX を検証する。

tools/add_second_output.py のグラフ手術は「Identity を足して出力を増やす」だけなので、
既存の計算経路 (spec_e / state_out) は一切変わってはいけない。このツールはそれを含めて
次を確認する:

  1. spec_e    : 元モデルと完全一致 (手術が既存経路を壊していない)
  2. state_out : 元モデルと完全一致 (state の更新も不変)
  3. spec_ns   : spec_e と「違う」こと (マスク経路のみ = 別信号)
  4. spec_ns   : 内容遅延が spec_e と同じ MODEL_FRAME_DELAY フレームであること
  5. spec_ns / spec_e のレベル差 (Voice Reverb ノブの効き幅)

usage:
  python tools/verify_two_output.py \
      --base models/dpdfnet2_48khz_hr.onnx \
      --two  models/dpdfnet2_48khz_hr_2out.onnx \
      --wav  test_signals/speech_with_noise.wav
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np
import onnxruntime as ort

from check_onnx_model import (
    MODEL_FRAME_DELAY,
    istft_frames,
    naive_resample,
    read_wav,
    stft_frames,
)


def make_session(path: Path) -> ort.InferenceSession:
    so = ort.SessionOptions()
    so.intra_op_num_threads = 1
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    return ort.InferenceSession(str(path), sess_options=so, providers=["CPUExecutionProvider"])


def init_state(sess: ort.InferenceSession) -> np.ndarray:
    m = sess.get_modelmeta().custom_metadata_map
    erb, sp = int(m["erb_norm_state_size"]), int(m["spec_norm_state_size"])
    s = np.zeros(int(m["state_size"]), np.float32)
    s[:erb] = np.array([float(v) for v in m["erb_norm_init"].split(",")], np.float32)
    s[erb:erb + sp] = np.array([float(v) for v in m["spec_norm_init"].split(",")], np.float32)
    return s


def run(sess: ort.InferenceSession, spec_in: np.ndarray, want_ns: bool):
    """全フレームを流し、(spec_e, spec_ns or None, 最終 state) を返す。"""
    ins, outs = sess.get_inputs(), sess.get_outputs()
    names = [o.name for o in outs]
    freq_bins = int(ins[0].shape[-2])
    hop_names = ["spec_e"]
    if want_ns:
        if "spec_ns" not in names:
            raise SystemExit("ERROR: spec_ns が出力に無い (2 出力化されていない)")
        hop_names.append("spec_ns")
    if "state_out" not in names:
        raise SystemExit("ERROR: state_out が出力に無い")

    state = init_state(sess)
    ri = np.zeros((1, 1, freq_bins, 2), np.float32)
    e_list, ns_list = [], []

    for t in range(spec_in.shape[0]):
        ri[0, 0, :, 0] = spec_in[t].real
        ri[0, 0, :, 1] = spec_in[t].imag
        got = sess.run(hop_names + ["state_out"],
                       {ins[0].name: ri, ins[1].name: state})
        by_name = dict(zip(hop_names + ["state_out"], got))
        e_list.append(by_name["spec_e"][0, 0].copy())
        if want_ns:
            ns_list.append(by_name["spec_ns"][0, 0].copy())
        state = by_name["state_out"]

    e = np.stack(e_list)
    ns = np.stack(ns_list) if ns_list else None
    return e, ns, state


def to_complex(a: np.ndarray) -> np.ndarray:
    return (a[..., 0] + 1j * a[..., 1]).astype(np.complex64)


def align_and_level(x: np.ndarray, spec: np.ndarray, win_len: int, hop: int):
    """istft + MODEL_FRAME_DELAY 補正後の (lag, corr, rms) を返す。"""
    y = istft_frames(to_complex(spec), win_len, hop)
    y = y[MODEL_FRAME_DELAY * hop:]
    n = min(len(x), len(y))
    a = x[:n] - x[:n].mean()
    b = y[:n] - y[:n].mean()
    den = (np.linalg.norm(a) * np.linalg.norm(b)) or 1.0
    c = np.correlate(b, a, mode="full") / den
    lag = int(np.argmax(np.abs(c)) - (n - 1))
    rms = float(np.sqrt(np.mean(np.square(y[:n])))) if n else 0.0
    return lag, float(np.abs(c).max()), rms


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", required=True, type=Path)
    ap.add_argument("--two", required=True, type=Path)
    ap.add_argument("--wav", type=Path, default=Path("test_signals/speech_with_noise.wav"))
    args = ap.parse_args()

    for p in (args.base, args.two):
        if not p.is_file():
            print(f"ERROR: not found: {p}", file=sys.stderr)
            return 1

    s_base, s_two = make_session(args.base), make_session(args.two)
    print(f"[IO] base outputs = {[o.name for o in s_base.get_outputs()]}")
    print(f"[IO] two  outputs = {[o.name for o in s_two.get_outputs()]}")

    ins = s_base.get_inputs()
    freq_bins = int(ins[0].shape[-2])
    win_len = (freq_bins - 1) * 2
    hop = win_len // 2

    x, sr = read_wav(args.wav)
    x = naive_resample(x, sr, 48000)
    # 先頭に無音を足す (相関の窓を安定させる)
    x = np.concatenate([np.zeros(4800, np.float32), x]).astype(np.float32)
    spec_in = stft_frames(x, win_len, hop)
    print(f"[WAV] {args.wav.name}: {len(x)} samples, {spec_in.shape[0]} frames")

    e_b, ns_b, st_b = run(s_base, spec_in, want_ns=False)
    e_t, ns_t, st_t = run(s_two, spec_in, want_ns=True)

    ok = True

    # 1. spec_e がビット一致
    d_e = float(np.max(np.abs(e_b - e_t)))
    print(f"[1] spec_e   max|diff| = {d_e:.3e}  -> {'OK' if d_e == 0.0 else 'NG'}")
    ok &= (d_e == 0.0)

    # 2. state_out がビット一致
    d_s = float(np.max(np.abs(st_b - st_t)))
    print(f"[2] state_out max|diff| = {d_s:.3e}  -> {'OK' if d_s == 0.0 else 'NG'}")
    ok &= (d_s == 0.0)

    # 3. spec_ns が spec_e と異なる
    diff = float(np.sqrt(np.mean(np.square(np.abs(ns_t - e_t)))))
    ref = float(np.sqrt(np.mean(np.square(np.abs(e_t))))) or 1.0
    rel = diff / ref
    print(f"[3] |spec_ns - spec_e| = {diff:.6f} ({20*np.log10(rel+1e-30):+.1f} dB rel) "
          f"-> {'OK (別信号)' if rel > 1e-3 else 'NG (同一)'}")
    ok &= (rel > 1e-3)

    # 4/5. 遅延とレベル
    rms_in = float(np.sqrt(np.mean(np.square(x))))
    for name, spec in (("spec_e ", e_t), ("spec_ns", ns_t)):
        lag, corr, rms = align_and_level(x, spec, win_len, hop)
        db_in = 20 * np.log10(rms / rms_in) if rms_in else 0.0
        print(f"[4] {name}: lag={lag:6d} samples ({lag/hop:.2f} hop)  corr={corr:.3f}  "
              f"rms={rms:.6f} ({db_in:+.2f} dB vs input)")
        if abs(lag) > 8:
            print(f"    [WARN] {name} の整列が想定外 (MODEL_FRAME_DELAY={MODEL_FRAME_DELAY} を確認)")
            ok = False

    print(f"\n[RESULT] {'PASS' if ok else 'FAIL'}")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
