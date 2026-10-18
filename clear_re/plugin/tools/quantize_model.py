#!/usr/bin/env python3
"""動的量子化 (INT8) した DPDFNet を作り、CPU EP で速度と誤差を測る。

GPU に逃がす代わりに CPU 側を軽くする案の検証。重みだけを量子化する
dynamic quantization なので、入出力は float のまま (プラグイン側の変更は
モデルファイルの差し替えだけ)。

  * 速度: pytest 相当の 1 hop 同期時間 (batch_provider.py と同じ条件)
  * 誤差: 同じ spec 系列を流したときの出力の SNR (dB) と、状態のリセット直後の
    先頭数十 hop が特に敏感なので分けて出す

usage:
  python tools/quantize_model.py --models models/dpdfnet8_48khz_hr_2out.onnx
"""

from __future__ import annotations

import argparse
import statistics
import sys
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
from onnxruntime.quantization import QuantType, quantize_dynamic

HOP_MS = 10.0
KEYS = ("erb_norm_init", "spec_norm_init", "erb_norm_state_size", "spec_norm_state_size")


def session(path: Path, threads: int = 1) -> ort.InferenceSession:
    so = ort.SessionOptions()
    so.intra_op_num_threads = threads
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    so.log_severity_level = 3
    return ort.InferenceSession(str(path), sess_options=so, providers=["CPUExecutionProvider"])


def initial_state(sess: ort.InferenceSession, ins) -> np.ndarray:
    meta = sess.get_modelmeta().custom_metadata_map
    s = np.zeros(int(ins[1].shape[0]), np.float32)
    erb, sp = int(meta["erb_norm_state_size"]), int(meta["spec_norm_state_size"])
    s[:erb] = np.array([float(v) for v in meta["erb_norm_init"].split(",")], np.float32)
    s[erb:erb + sp] = np.array([float(v) for v in meta["spec_norm_init"].split(",")], np.float32)
    return s


def run_series(sess: ort.InferenceSession, specs: list[np.ndarray]) -> tuple[list[np.ndarray], list[float]]:
    ins, outs = sess.get_inputs(), [o.name for o in sess.get_outputs()]
    state = initial_state(sess, ins)
    i_out, i_state = outs.index("spec_out") if "spec_out" in outs else 0, outs.index("state_out")
    got, times = [], []
    for spec in specs:
        t0 = time.perf_counter()
        r = sess.run(outs, {ins[0].name: spec, ins[1].name: state})
        times.append((time.perf_counter() - t0) * 1000.0)
        state = r[i_state]
        got.append(np.asarray(r[i_out], np.float32).ravel())
    return got, times


def snr_db(ref: np.ndarray, test: np.ndarray) -> float:
    err = test - ref
    p_ref, p_err = float(np.mean(ref ** 2)), float(np.mean(err ** 2))
    if p_err <= 0:
        return float("inf")
    return 10.0 * np.log10(p_ref / p_err)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="+", required=True, type=Path)
    ap.add_argument("--out-dir", type=Path, default=Path("build/quantized"))
    ap.add_argument("--frames", type=int, default=300)
    a = ap.parse_args()

    a.out_dir.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(0)

    for m in a.models:
        if not m.is_file():
            print(f"ERROR: not found: {m}", file=sys.stderr)
            return 1
        q = a.out_dir / (m.stem + "_int8.onnx")
        quantize_dynamic(str(m), str(q), weight_type=QuantType.QUInt8)
        print(f"{m.name} -> {q} ({q.stat().st_size / 1e6:.1f} MB / fp32 {m.stat().st_size / 1e6:.1f} MB)")

        # モデルの契約 (metadata) と入出力名が保たれているか。
        for path, tag in ((m, "fp32"), (q, "int8")):
            s = session(path)
            md = s.get_modelmeta().custom_metadata_map
            miss = [k for k in KEYS if k not in md]
            names = ([i.name for i in s.get_inputs()], [o.name for o in s.get_outputs()])
            print(f"  {tag}: metadata欠け={miss or 'なし'} io={names}")
            if path == m:
                fb = int(s.get_inputs()[0].shape[-2])
                specs = [
                    (rng.standard_normal((1, 1, fb, 2)) * 0.1).astype(np.float32)
                    for _ in range(a.frames)
                ]

        ref_sess, q_sess = session(m), session(q)
        ref, t_ref = run_series(ref_sess, specs)
        got, t_q = run_series(q_sess, specs)

        worst = max(snr_db(r, g) for r, g in zip(ref, got))
        head = min(snr_db(r, g) for r, g in zip(ref[:25], got[:25]))
        med_ref, med_q = statistics.median(t_ref), statistics.median(t_q)
        print(
            f"  hop時間 fp32={med_ref:5.2f} ms  int8={med_q:5.2f} ms  "
            f"({med_ref / med_q:4.2f}x)   SNR 最悪={worst:6.1f} dB (先頭25hop 最悪={head:6.1f} dB)"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
