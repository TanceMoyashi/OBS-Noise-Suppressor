#!/usr/bin/env python3
"""DPDFNet ONNX の 1 フレーム推論時間を測り、リアルタイム余裕 (RTF) を出す。

1 フレーム = 10 ms の音声 (hop=480 @48kHz)。したがって推論が 10 ms 未満なら
1 スレッドでリアルタイム。OBS のライブフィルタではステレオ = 2 インスタンスなので、
その 2 倍のコストになる点に注意。

usage:
  python tools/bench_model.py --models models/dpdfnet2_48khz_hr_2out.onnx models/dpdfnet8_48khz_hr_2out.onnx
  python tools/bench_model.py --models ... --threads 1 2 4
"""

from __future__ import annotations

import argparse
import statistics
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort


def build(model: Path, threads: int) -> ort.InferenceSession:
    so = ort.SessionOptions()
    so.intra_op_num_threads = threads
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    so.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    return ort.InferenceSession(str(model), sess_options=so, providers=["CPUExecutionProvider"])


def bench(sess: ort.InferenceSession, frames: int = 300, warmup: int = 30):
    ins, outs = sess.get_inputs(), [o.name for o in sess.get_outputs()]
    fb = int(ins[0].shape[-2])
    state_size = int(ins[1].shape[0])
    meta = sess.get_modelmeta().custom_metadata_map
    state = np.zeros(state_size, np.float32)
    erb, sp = int(meta["erb_norm_state_size"]), int(meta["spec_norm_state_size"])
    state[:erb] = np.array([float(v) for v in meta["erb_norm_init"].split(",")], np.float32)
    state[erb:erb + sp] = np.array([float(v) for v in meta["spec_norm_init"].split(",")], np.float32)

    rng = np.random.default_rng(0)
    feed_cache = {}
    for _ in range(warmup):
        spec = (rng.standard_normal((1, 1, fb, 2)) * 0.1).astype(np.float32)
        got = sess.run(outs, {ins[0].name: spec, ins[1].name: state})
        state = got[outs.index("state_out")]

    times = []
    for _ in range(frames):
        spec = (rng.standard_normal((1, 1, fb, 2)) * 0.1).astype(np.float32)
        t0 = time.perf_counter()
        got = sess.run(outs, {ins[0].name: spec, ins[1].name: state})
        times.append((time.perf_counter() - t0) * 1000.0)
        state = got[outs.index("state_out")]
    return times


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="+", required=True, type=Path)
    ap.add_argument("--threads", nargs="+", type=int, default=[1])
    ap.add_argument("--frames", type=int, default=300)
    args = ap.parse_args()

    hop_ms = 10.0

    for m in args.models:
        if not m.is_file():
            print(f"ERROR: not found: {m}", file=sys.stderr)
            return 1
        for th in args.threads:
            sess = build(m, th)
            t = bench(sess, args.frames)
            med = statistics.median(t)
            p90 = sorted(t)[int(0.9 * len(t))]
            rtf = med / hop_ms
            verdict = "OK" if rtf < 0.5 else ("ぎりぎり" if rtf < 1.0 else "リアルタイム不可")
            print(f"{m.name:32s} threads={th}  "
                  f"median={med:6.2f} ms  p90={p90:6.2f} ms  "
                  f"RTF={rtf:5.2f}  (mono, {'1 core で余裕' if rtf < 0.5 else verdict})")
            # ステレオ = 2 インスタンス
            print(f"{'':32s}             -> stereo x2 = {med*2:6.2f} ms/frame "
                  f"(RTF {rtf*2:5.2f})")
    print("\n[NOTE] 1 フレーム = 10 ms の音声。RTF < 1 なら 1 スレッドでリアルタイム。")
    print("       OBS ではステレオ = 2 インスタンスなので RTF が 2 倍になる。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
