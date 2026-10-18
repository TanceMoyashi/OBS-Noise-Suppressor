#!/usr/bin/env python3
"""静的量子化 (INT8, QDQ) した DPDFNet を作り、CPU EP で速度と誤差を測る。

**結論: このモデルでは INT8 は使えない（2026-10-19 実測）**。理由まで含めて
記録として残す。動的量子化 (`quantize_model.py`) も同じく駄目だった。

    構成                                        hop時間 (fp32 比)   SNR
    --------------------------------------------------------------
    fp32 (基準)                                  2.91 ms            -
    QDQ + per-channel 重み                       -                  実行不可
    QOperator + per-channel 重み                 -                  実行不可
    QDQ + per-tensor                            3.47 ms (0.84x)    **0.0 dB**

  * per-channel は ORT の `QLinearMatMul` が
    「weight zero point must be a scalar, 1D tensor of size 1, or last to second
    dimension is 1」で**実行時に落ちる**（`/model/enc/df_fc_emb/.../MatMul`）。
    QOperator 形式でも同じ。このモデルの MatMul 形状が per-channel の
    zero point を許さないため。
  * per-tensor は動くが、**SNR 0 dB = 出力が壊れる**（先頭 25 hop は fp32 と
    完全一致、その後で破綻するので、再帰的な state 経路が量子化に耐えない）。
    しかも fp32 より遅い。
  * したがって CPU を軽くする手は「軽いモデル (dp2) に落とす」「セッション
    設定を詰める」「GPU に逃がす（CPU 使用率は下がるが壁時計は遅い）」の
    3 系統に限られる。

キャリブレーションには**本物の入力**を使う（プラグインと同じ窓・hop で実音声から
STFT し、fp32 モデルを流して state を進めながら (spec, state) の組を集める）。
乱数入力だと活性の範囲が実態とずれる。

usage:
  python tools/quantize_static.py --model models/dpdfnet8_48khz_hr_2out.onnx `
      --wav test_signals/speech_tail_48k.wav --out-dir C:/tmp/qstatic
"""

from __future__ import annotations

import argparse
import statistics
import sys
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
from onnxruntime.quantization import (
    CalibrationDataReader,
    CalibrationMethod,
    QuantFormat,
    QuantType,
    quantize_static,
)

HOP = 480
WIN = 960
FREQ_BINS = WIN // 2 + 1


def vorbis_window(n: int) -> np.ndarray:
    i = np.arange(n, dtype=np.float64)
    return np.sin(0.5 * np.pi * np.sin(np.pi * (i + 0.5) / n) ** 2).astype(np.float32)


def read_wav_mono(path: Path, target_sr: int = 48000) -> np.ndarray:
    import wave

    with wave.open(str(path), "rb") as w:
        sr, ch, width, frames = w.getframerate(), w.getnchannels(), w.getsampwidth(), w.getnframes()
        raw = w.readframes(frames)
    if width != 2:
        raise SystemExit("16-bit PCM のみ対応: %s" % path)
    x = np.frombuffer(raw, np.int16).astype(np.float32) / 32768.0
    if ch > 1:
        x = x.reshape(-1, ch).mean(axis=1)
    if sr != target_sr:
        raise SystemExit("48 kHz の WAV を指定してください: %s" % path)
    return x


def make_session(path: Path, threads: int = 1) -> ort.InferenceSession:
    so = ort.SessionOptions()
    so.intra_op_num_threads = threads
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    so.log_severity_level = 3
    return ort.InferenceSession(str(path), sess_options=so, providers=["CPUExecutionProvider"])


def io_names(sess: ort.InferenceSession) -> tuple[str, str, str, str]:
    """(spec_in, state_in, spec_out, state_out) を名前で拾う（版によって名前が違う）。"""
    ins = [i.name for i in sess.get_inputs()]
    outs = [o.name for o in sess.get_outputs()]
    spec_in = next(n for n in ins if n != "state_in")
    state_in = next(n for n in ins if n == "state_in")
    spec_out = next((n for n in outs if n in ("spec_e", "spec_out")), outs[0])
    state_out = next(n for n in outs if n == "state_out")
    return spec_in, state_in, spec_out, state_out


def initial_state(sess: ort.InferenceSession, state_in: str) -> np.ndarray:
    md = sess.get_modelmeta().custom_metadata_map
    shape = sess.get_inputs()[[i.name for i in sess.get_inputs()].index(state_in)].shape
    size = int(shape[0]) if isinstance(shape[0], int) else 90228
    s = np.zeros(size, np.float32)
    if "erb_norm_init" in md:
        erb = int(md["erb_norm_state_size"])
        s[:erb] = np.array([float(v) for v in md["erb_norm_init"].split(",")], np.float32)
        s[erb:erb + int(md["spec_norm_state_size"])] = np.array(
            [float(v) for v in md["spec_norm_init"].split(",")], np.float32
        )
    return s


def specs_from_wav(x: np.ndarray, window: np.ndarray) -> list[np.ndarray]:
    """プラグインと同じ手順（hop 左シフト -> 窓掛け -> rfft）で spec を作る。"""
    n_hops = max(1, int(np.ceil(len(x) / HOP)))
    pad = np.zeros(n_hops * HOP, np.float32)
    pad[: len(x)] = x[: n_hops * HOP]
    inbuf = np.zeros(WIN, np.float32)
    out = []
    for s in range(n_hops):
        inbuf = np.concatenate([inbuf[HOP:], pad[s * HOP:(s + 1) * HOP]]).astype(np.float32)
        frame = np.fft.rfft(inbuf * window, n=WIN)
        ri = np.zeros((1, 1, FREQ_BINS, 2), np.float32)
        ri[0, 0, :, 0] = frame.real
        ri[0, 0, :, 1] = frame.imag
        out.append(ri)
    return out


def collect_calibration(sess: ort.InferenceSession, specs: list[np.ndarray], limit: int):
    """fp32 モデルを流して (spec, state) の実データを集める。"""
    spec_in, state_in, _, state_out = io_names(sess)
    state = initial_state(sess, state_in)
    data = []
    for spec in specs[:limit]:
        data.append({spec_in: spec, state_in: state.copy()})
        state = sess.run([state_out], {spec_in: spec, state_in: state})[0]
    return data


class Reader(CalibrationDataReader):
    def __init__(self, data: list[dict]):
        self.data = data
        self.i = 0

    def get_next(self):
        if self.i >= len(self.data):
            return None
        d = self.data[self.i]
        self.i += 1
        return d

    def rewind(self):
        self.i = 0


def run_series(sess, specs, names):
    spec_in, state_in, spec_out, state_out = names
    state = initial_state(sess, state_in)
    got, times = [], []
    for spec in specs:
        t0 = time.perf_counter()
        r = sess.run([spec_out, state_out], {spec_in: spec, state_in: state})
        times.append((time.perf_counter() - t0) * 1000.0)
        state = r[1]
        got.append(np.asarray(r[0], np.float32).ravel())
    return got, times


def snr_db(ref: np.ndarray, test: np.ndarray) -> float:
    err = test - ref
    p_ref, p_err = float(np.mean(ref ** 2)), float(np.mean(err ** 2))
    if p_err <= 0:
        return float("inf")
    return 10.0 * np.log10(p_ref / p_err)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", nargs="+", required=True, type=Path)
    ap.add_argument("--wav", type=Path, default=Path("test_signals/speech_tail_48k.wav"))
    ap.add_argument("--out-dir", type=Path, required=True)
    ap.add_argument("--calib-frames", type=int, default=400)
    ap.add_argument("--frames", type=int, default=300)
    ap.add_argument("--format", choices=("qdq", "qoperator"), default="qdq")
    ap.add_argument("--per-tensor", action="store_true",
                    help="重みも per-tensor（per-channel が ORT のカーネル制約に当たるとき用）")
    a = ap.parse_args()

    a.out_dir.mkdir(parents=True, exist_ok=True)
    window = vorbis_window(WIN)
    specs = specs_from_wav(read_wav_mono(a.wav), window)
    print(f"calib source: {a.wav.name} ({len(specs)} hops)  format={a.format} per_channel={not a.per_tensor}")

    for m in a.model:
        tag = "qop" if a.format == "qoperator" else "qdq"
        if a.per_tensor:
            tag += "_pertensor"
        q = a.out_dir / (m.stem.replace("_2out", "") + f"_int8_{tag}.onnx")
        base = make_session(m)
        calib = collect_calibration(base, specs, a.calib_frames)
        t0 = time.perf_counter()
        quantize_static(
            str(m), str(q), Reader(calib),
            quant_format=QuantFormat.QOperator if a.format == "qoperator" else QuantFormat.QDQ,
            activation_type=QuantType.QUInt8,
            weight_type=QuantType.QInt8,
            per_channel=not a.per_tensor,
            calibrate_method=CalibrationMethod.Percentile,
        )
        print(f"{m.name} -> {q.name} ({q.stat().st_size / 1e6:.1f} MB / fp32 {m.stat().st_size / 1e6:.1f} MB, "
              f"量子化 {time.perf_counter() - t0:.0f} s)")

        test_specs = specs[: a.frames]
        ref, t_ref = run_series(base, test_specs, io_names(base))
        try:
            qs = make_session(q)
            got, t_q = run_series(qs, test_specs, io_names(qs))
        except Exception as exc:  # 量子化形式が ORT のカーネルに合わない場合
            print(f"  ERROR: 実行できない ({str(exc).splitlines()[-1][:120]})")
            continue
        per = [snr_db(r, g) for r, g in zip(ref, got)]
        med_ref, med_q = statistics.median(t_ref), statistics.median(t_q)
        print(f"  hop time fp32={med_ref:5.2f} ms  int8={med_q:5.2f} ms ({med_ref / med_q:4.2f}x)"
              f"  SNR median={statistics.median(per):5.1f} dB  worst={min(per):5.1f} dB"
              f"  先頭25hop最悪={min(per[:25]):5.1f} dB")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
