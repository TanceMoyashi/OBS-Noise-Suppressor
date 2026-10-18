#!/usr/bin/env python3
"""DPDFNet を実行プロバイダ (CPU / GPU=DirectML) ごとに測る。

`bench_model.py` は CPU EP のスレッド数だけを見るのに対し、こちらは

  * 1 hop = 480 samples @48kHz = 10 ms の**同期**推論時間 (オーディオスレッドが
    実際に待つ時間)
  * その間にプラグインが消費した CPU (プロセス CPU 時間 / 経過時間 = 1 コア換算 %)

の 2 つを出す。GPU に逃がす目的は 2 つ目を下げることなので、両方見ないと判断できない。

`--instances 2` は OBS のステレオ (インスタンス 2 本) を再現する。2 本を別スレッドで
同時に回し、**互いに干渉したときの** 1 hop 時間とコア消費を見る。

usage:
  python tools/bench_provider.py --models models/dpdfnet8_48khz_hr_2out.onnx --providers cpu dml
  python tools/bench_provider.py --models ... --providers dml --cpu-fallback off
"""

from __future__ import annotations

import argparse
import statistics
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np
import onnxruntime as ort

HOP_MS = 10.0  # hop 480 @48kHz


def build(
    model: Path, provider: str, threads: int, device_id: int, allow_cpu_fallback: bool = True
) -> ort.InferenceSession:
    so = ort.SessionOptions()
    so.intra_op_num_threads = threads
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    so.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    so.log_severity_level = 3  # WARNING 以上のみ
    if not allow_cpu_fallback:
        # 1 ノードでも CPU 実行に落ちていれば初期化に失敗する = GPU 配置の確認。
        so.add_session_config_entry("session.disable_cpu_ep_fallback", "1")

    if provider == "dml":
        providers: list = [("DmlExecutionProvider", {"device_id": device_id})]
        if allow_cpu_fallback:
            providers.append("CPUExecutionProvider")
    elif provider == "cuda":
        providers = [("CUDAExecutionProvider", {"device_id": device_id})]
        if allow_cpu_fallback:
            providers.append("CPUExecutionProvider")
    else:
        providers = ["CPUExecutionProvider"]

    return ort.InferenceSession(str(model), sess_options=so, providers=providers)


def reset_state(sess: ort.InferenceSession, ins) -> np.ndarray:
    meta = sess.get_modelmeta().custom_metadata_map
    state = np.zeros(int(ins[1].shape[0]), np.float32)
    erb, sp = int(meta["erb_norm_state_size"]), int(meta["spec_norm_state_size"])
    state[:erb] = np.array([float(v) for v in meta["erb_norm_init"].split(",")], np.float32)
    state[erb:erb + sp] = np.array([float(v) for v in meta["spec_norm_init"].split(",")], np.float32)
    return state


def bench(sess: ort.InferenceSession, frames: int, warmup: int) -> tuple[list[float], float, float]:
    """1 セッションを直列に回し、(hop ごとの ms, プロセス CPU 秒, 経過秒) を返す。"""
    ins, outs = sess.get_inputs(), [o.name for o in sess.get_outputs()]
    fb = int(ins[0].shape[-2])
    state = reset_state(sess, ins)
    rng = np.random.default_rng(0)

    def hop() -> list:
        spec = (rng.standard_normal((1, 1, fb, 2)) * 0.1).astype(np.float32)
        return sess.run(outs, {ins[0].name: spec, ins[1].name: state})

    for _ in range(warmup):
        state = hop()[outs.index("state_out")]

    times: list[float] = []
    cpu0, wall0 = time.process_time(), time.perf_counter()
    for _ in range(frames):
        t0 = time.perf_counter()
        state = hop()[outs.index("state_out")]
        times.append((time.perf_counter() - t0) * 1000.0)
    cpu, wall = time.process_time() - cpu0, time.perf_counter() - wall0
    return times, cpu, wall


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="+", required=True, type=Path)
    ap.add_argument("--providers", nargs="+", default=["cpu", "dml"])
    ap.add_argument("--threads", type=int, default=1)
    ap.add_argument("--device-id", type=int, default=0)
    ap.add_argument("--instances", nargs="+", type=int, default=[1], help="同時に走らせるセッション数 (ステレオ=2)")
    ap.add_argument("--frames", type=int, default=300)
    ap.add_argument("--warmup", type=int, default=40)
    ap.add_argument("--cpu-fallback", choices=["on", "off"], default="on")
    a = ap.parse_args()

    missing = [m for m in a.models if not m.is_file()]
    if missing:
        print("ERROR: not found: " + ", ".join(str(m) for m in missing), file=sys.stderr)
        return 1

    for m in a.models:
        for prov in a.providers:
            label = {"cpu": "CPU", "dml": "GPU (DirectML)", "cuda": "GPU (CUDA)"}.get(prov, prov)
            for n in a.instances:
                try:
                    sessions = [
                        build(m, prov, a.threads, a.device_id, a.cpu_fallback == "on")
                        for _ in range(n)
                    ]
                except Exception as e:  # noqa: BLE001 - EP 初期化失敗はそのまま見せる
                    print(f"{m.name:32s} {label:16s} n={n} 初期化失敗: {e}")
                    continue

                per_inst: list[list[float]] = []
                cpus: list[float] = []
                wall0 = time.perf_counter()
                with ThreadPoolExecutor(max_workers=n) as ex:
                    futs = [ex.submit(bench, s, a.frames, a.warmup) for s in sessions]
                    for f in futs:
                        times, cpu, _wall = f.result()
                        per_inst.append(times)
                        cpus.append(cpu)
                wall = time.perf_counter() - wall0

                med = max(statistics.median(t) for t in per_inst)
                worst_p90 = max(sorted(t)[int(0.9 * len(t))] for t in per_inst)
                cores = sum(cpus) / wall if wall > 0 else 0.0
                print(
                    f"{m.name:32s} {label:16s} n={n} "
                    f"median={med:6.2f} ms  p90={worst_p90:6.2f}  "
                    f"RTF={med / HOP_MS:5.2f}  CPU={cores * 100:6.1f} %"
                )
                print(f"{'':32s} {'':16s}      EP={sessions[0].get_providers()}")

    print("\n[NOTE] median は 1 hop (10 ms 音声) の同期時間。CPU % は全スレッド合計 / 経過時間。")
    print("       n=2 が OBS のステレオ相当。RTF >= 1 はリアルタイム不可。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
