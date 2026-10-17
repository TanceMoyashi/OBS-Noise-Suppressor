#!/usr/bin/env python3
"""DPDFNet ONNX モデルの契約検証ツール。

C++ 実装 (VorbisStft.cpp / DpdfNet.cpp) とは独立に numpy だけで STFT/iSTFT を
実装し、次を確認する:

  1. metadata の必須キーと win_len の復元 (freq_bins - 1) * 2
  2. 初期 state の構築 (erb_norm_init / spec_norm_init + ゼロ埋め)
  3. 入出力レベルが整合するか (wnorm を外から掛けない契約の確認)
  4. 再構成の内容が入力と同位相で整列しているか (相互相関のラグ ~ 0)

注意: リアルタイム遅延 (6 hop = 2880 サンプル = 60 ms @48 kHz) は「ストリーミングで
いつ出力できるか」の性質であり、このバッチ相関では測れない。実測は tools/sim_stream.py
が行う (出力位置の規約まで含めてシミュレーションする)。このツールの役割は契約
(I/O 形状 / state / レベル / 4 フレーム遅延) の検証。

依存: numpy, onnxruntime  (soundfile は不要。WAV は標準ライブラリ wave で扱う)
"""

from __future__ import annotations

import argparse
import sys
import wave
from pathlib import Path

import numpy as np
import onnxruntime as ort

PI = 3.1415926535897932384626433

# モデルのフレーム遅延。出力フレーム t は入力フレーム t-4 の内容。
# 根拠: 公式実装の ATTN_LIMIT_NOISY_FRAME_OFFSET = 4、公式オフライン経路が
# istft 出力の先頭 2*win_len = 4 hop を捨てる補正、diag_model.py の実測。
MODEL_FRAME_DELAY = 4


# ---------------------------------------------------------------------------
# WAV I/O (16-bit PCM / float32 のみ)
# ---------------------------------------------------------------------------
def read_wav(path: Path) -> tuple[np.ndarray, int]:
    with wave.open(str(path), "rb") as w:
        n_ch = w.getnchannels()
        width = w.getsampwidth()
        rate = w.getframerate()
        n = w.getnframes()
        raw = w.readframes(n)

    if width == 2:
        x = np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0
    elif width == 4:
        x = np.frombuffer(raw, dtype="<i4").astype(np.float32) / 2147483648.0
    else:
        raise SystemExit(f"unsupported WAV sample width: {width} bytes")

    if n_ch > 1:
        x = x.reshape(-1, n_ch).mean(axis=1)

    return x, rate


def write_wav(path: Path, x: np.ndarray, rate: int) -> None:
    y = np.clip(x, -1.0, 1.0)
    pcm = (y * 32767.0).astype("<i2")
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(pcm.tobytes())


def naive_resample(x: np.ndarray, src: int, dst: int) -> np.ndarray:
    if src == dst:
        return x
    n_out = int(round(len(x) * dst / src))
    t_in = np.arange(len(x), dtype=np.float64)
    t_out = np.linspace(0.0, len(x) - 1.0, n_out)
    return np.interp(t_out, t_in, x).astype(np.float32)


def synth_test_signal(rate: int, seconds: float = 3.0) -> np.ndarray:
    """契約検証用の合成信号 (--wav 省略時)。

    「声 + 暗騒音 + 残響」を模した信号を作る。目的は音質評価ではなく、
    抑圧と残響除去の両方の経路に意味のある入力変化を与えて、契約
    (レベル整合 / 内容の整列 / state の引き回し) を検証すること。
    """
    rng = np.random.default_rng(12345)
    n = int(round(rate * seconds))
    t = np.arange(n, dtype=np.float64) / rate

    # 声: F0 120 Hz の倍音 (振幅 1/k)。F0 をゆっくり揺らして定常にしない。
    f0 = 120.0 * (1.0 + 0.03 * np.sin(2.0 * PI * 0.7 * t))
    phase = 2.0 * PI * np.cumsum(f0) / rate
    voice = np.zeros(n, dtype=np.float64)
    for k in range(1, 41):
        if k * 120.0 >= rate / 2.0 - 100.0:
            break
        voice += (1.0 / k) * np.sin(k * phase)
    voice /= (np.max(np.abs(voice)) + 1e-12)

    # 残響: 60 ms / 130 ms の 2 つの反射
    reverb = np.zeros(n, dtype=np.float64)
    for delay_ms, gain in ((60.0, 0.45), (130.0, 0.25)):
        d = int(round(rate * delay_ms / 1000.0))
        reverb[d:] += gain * voice[:-d]

    # 暗騒音: -28 dBFS の白色雑音
    noise = rng.standard_normal(n)
    noise *= 10.0 ** (-28.0 / 20.0) / (np.sqrt(np.mean(noise ** 2)) + 1e-12)

    y = 0.5 * (voice + reverb) + noise
    y *= 10.0 ** (-18.0 / 20.0) / (np.sqrt(np.mean(y ** 2)) + 1e-12)
    return y.astype(np.float32)


# ---------------------------------------------------------------------------
# STFT / iSTFT (モデル側 model/utils.py と同一の Vorbis 窓)
# ---------------------------------------------------------------------------
def vorbis_window(win_len: int) -> np.ndarray:
    half = win_len / 2.0
    i = np.arange(win_len, dtype=np.float64)
    s = np.sin(0.5 * PI * (i + 0.5) / half)
    return np.sin(0.5 * PI * s * s).astype(np.float32)


def stft_frames(x: np.ndarray, win_len: int, hop: int) -> np.ndarray:
    """center=True (reflect) 相当。戻り値 [T, F] complex64"""
    w = vorbis_window(win_len)
    pad = win_len // 2
    xp = np.pad(x, (pad, pad), mode="reflect")
    n_frames = 1 + (len(xp) - win_len) // hop
    idx = np.arange(win_len)[None, :] + (np.arange(n_frames)[:, None] * hop)
    frames = xp[idx] * w[None, :]
    return np.fft.rfft(frames, n=win_len).astype(np.complex64)


def istft_frames(spec: np.ndarray, win_len: int, hop: int) -> np.ndarray:
    """窓包絡線で正規化する WOLA。フレーム j は位置 j*hop を中心に置かれる。

    stft_frames は center=True (frame j の中心 = x[j*hop]) なので、素直に加算すると
    出力は win_len//2 = hop サンプル後ろにずれる。先頭を捨てて x と同じ座標系に戻す。
    """
    w = vorbis_window(win_len)
    n_frames, n_bins = spec.shape
    n_fft = (n_bins - 1) * 2

    time = np.fft.irfft(spec, n=n_fft, axis=1).astype(np.float32) * w[None, :]

    total = (n_frames - 1) * hop + win_len
    acc = np.zeros(total, dtype=np.float32)
    env = np.zeros(total, dtype=np.float32)
    w2 = (w * w).astype(np.float32)

    for j in range(n_frames):
        s = j * hop
        acc[s:s + win_len] += time[j]
        env[s:s + win_len] += w2

    env[env < 1e-8] = 1.0
    y = acc / env
    return y[win_len // 2:]


# ---------------------------------------------------------------------------
def main() -> int:
    ap = argparse.ArgumentParser(description="Verify a DPDFNet streaming ONNX export.")
    ap.add_argument("--model", required=True, type=Path)
    ap.add_argument("--wav", type=Path, default=None,
                    help="入力 WAV。省略時は合成した音声風テスト信号 (声 + 暗騒音 + 残響) を使う。")
    ap.add_argument("--out", type=Path, default=Path("out_check.wav"))
    ap.add_argument("--attn-limit-db", type=float, default=None,
                    help="Offline-style attenuation limit (noisy blend weight = 10^(-L/20)).")
    args = ap.parse_args()

    if not args.model.is_file():
        print(f"ERROR: model not found: {args.model}", file=sys.stderr)
        return 1

    so = ort.SessionOptions()
    so.intra_op_num_threads = 1
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    sess = ort.InferenceSession(str(args.model), sess_options=so,
                                providers=["CPUExecutionProvider"])

    # ---- 1. I/O 契約 ----
    ins, outs = sess.get_inputs(), sess.get_outputs()
    print(f"[IO] providers     : {sess.get_providers()}")
    print(f"[IO] inputs        : {[(i.name, i.shape, i.type) for i in ins]}")
    print(f"[IO] outputs       : {[(o.name, o.shape, o.type) for o in outs]}")

    if len(ins) < 2:
        print("ERROR: expected 2 inputs (spec, state_in)", file=sys.stderr)
        return 1

    spec_shape = ins[0].shape
    if not (isinstance(spec_shape[-2], int) and spec_shape[-2] > 1):
        print("ERROR: spec input shape must be static [1,1,F,2]", file=sys.stderr)
        return 1

    freq_bins = int(spec_shape[-2])
    win_len = (freq_bins - 1) * 2
    hop = win_len // 2
    print(f"[STFT] freq_bins={freq_bins}  win_len={win_len}  hop={hop}")

    # ---- metadata ----
    meta = sess.get_modelmeta().custom_metadata_map
    for k in ("model_type", "profile", "sample_rate", "n_fft", "hop_length",
              "window_length", "window_type", "state_size",
              "erb_norm_state_size", "spec_norm_state_size"):
        if k not in meta:
            print(f"ERROR: metadata key missing: {k}", file=sys.stderr)
            return 1
        print(f"[META] {k:22s} = {meta[k]}")

    if meta["window_type"] != "vorbis":
        print(f"ERROR: unexpected window_type={meta['window_type']}", file=sys.stderr)
        return 1

    model_sr = int(meta["sample_rate"])
    state_size = int(meta["state_size"])
    erb_size = int(meta["erb_norm_state_size"])
    spec_size = int(meta["spec_norm_state_size"])

    # ---- 2. 初期 state ----
    erb_init = np.array([float(v) for v in meta["erb_norm_init"].split(",")], dtype=np.float32)
    spec_init = np.array([float(v) for v in meta["spec_norm_init"].split(",")], dtype=np.float32)

    if len(erb_init) != erb_size or len(spec_init) != spec_size:
        print(f"ERROR: init sizes mismatch: erb {len(erb_init)}/{erb_size}, "
              f"spec {len(spec_init)}/{spec_size}", file=sys.stderr)
        return 1

    init_state = np.zeros(state_size, dtype=np.float32)
    init_state[0:erb_size] = erb_init
    init_state[erb_size:erb_size + spec_size] = spec_init
    print(f"[STATE] state_size={state_size} (erb {erb_size} + spec {spec_size} + "
          f"{state_size - erb_size - spec_size} zeros)")

    # ---- 入力 ----
    if args.wav is not None:
        if not args.wav.is_file():
            print(f"ERROR: wav not found: {args.wav}", file=sys.stderr)
            return 1
        x, sr = read_wav(args.wav)
        x = naive_resample(x, sr, model_sr)
        print(f"[WAV] {args.wav.name}: {len(x)} samples @ {model_sr} Hz (resampled from {sr})")
    else:
        x = synth_test_signal(model_sr)
        print(f"[WAV] synthetic (voice + noise + reverb): {len(x)} samples @ {model_sr} Hz")

    spec_in = stft_frames(x, win_len, hop)          # [T, F] complex
    print(f"[STFT] {spec_in.shape[0]} frames")

    spec_ri = np.zeros((1, 1, freq_bins, 2), dtype=np.float32)
    state = init_state.copy()
    out_frames: list[np.ndarray] = []

    in_name, state_name = ins[0].name, ins[1].name
    out_spec_name = next((o.name for o in outs if o.name == "spec_e"), outs[0].name)
    out_state_name = next((o.name for o in outs if o.name == "state_out"), outs[1].name)

    for t in range(spec_in.shape[0]):
        spec_ri[0, 0, :, 0] = spec_in[t].real
        spec_ri[0, 0, :, 1] = spec_in[t].imag
        spec_e, state = sess.run([out_spec_name, out_state_name],
                                 {in_name: spec_ri, state_name: state})
        out_frames.append(spec_e[0, 0].copy())

    spec_out = np.stack(out_frames, axis=0)          # [T, F, 2]
    spec_out_c = (spec_out[..., 0] + 1j * spec_out[..., 1]).astype(np.complex64)

    # ---- attenuation limit (offline の定義と同じ) ----
    if args.attn_limit_db is not None:
        alpha = 10.0 ** (-args.attn_limit_db / 20.0)
        # ストリーミングでは整列は厳密に MODEL_FRAME_DELAY フレーム
        aligned = np.zeros_like(spec_in)
        aligned[MODEL_FRAME_DELAY:] = spec_in[:-MODEL_FRAME_DELAY]
        spec_out_c = (alpha * aligned + (1.0 - alpha) * spec_out_c).astype(np.complex64)
        print(f"[ATTN] limit={args.attn_limit_db} dB -> noisy weight={alpha:.4f} "
              f"(noisy を {MODEL_FRAME_DELAY} フレームで整列)")

    # ---- 3/4. 再構成と整合性チェック ----
    y = istft_frames(spec_out_c, win_len, hop)

    # モデルの 4 フレーム遅延ぶんを落とすと x と同じ座標系になる
    # (公式オフライン経路の postprocess_spec が先頭 2*win_len = 4 hop を捨てるのと同じ補正)。
    drop = MODEL_FRAME_DELAY * hop
    y_aligned = y[drop:]

    def rms(v: np.ndarray) -> float:
        return float(np.sqrt(np.mean(np.square(v)))) if v.size else 0.0

    # 内容の整列 (バッチ再構成 + 4 フレーム補正でラグ ~0 が期待値)
    n = min(len(x), len(y_aligned))
    a, b = x[:n] - x[:n].mean(), y_aligned[:n] - y_aligned[:n].mean()
    denom = (np.linalg.norm(a) * np.linalg.norm(b)) or 1.0
    corr = np.correlate(b, a, mode="full") / denom
    lag = int(np.argmax(np.abs(corr)) - (n - 1))
    peak = float(np.abs(corr).max())

    rms_in, rms_out = rms(x[:n]), rms(y_aligned[:n])
    print(f"[LEVEL] rms in={rms_in:.6f} out={rms_out:.6f} "
          f"({20 * np.log10((rms_out / rms_in) if rms_in else 1.0):+.2f} dB)")

    if rms_in > 1e-6 and abs(20 * np.log10((rms_out / rms_in) or 1.0)) > 12.0:
        print("[WARN] 入出力レベル差が 12 dB 超。wnorm の扱いを確認すること。")

    print(f"[ALIGN] content lag = {lag} samples (4 フレーム補正後は ~0 が期待値)  "
          f"corr={peak:.3f}")
    if args.wav is None:
        print("[WARN] 合成信号は「音声ではない」とモデルが判断し -40 dB 以下に抑圧される。"
              "レベルと遅延の検証は --wav test_signals/speech_with_noise.wav で行うこと。")

    write_wav(args.out, y_aligned[: len(x)], model_sr)
    print(f"[OK] wrote {args.out}")
    print("[NOTE] リアルタイム遅延 (6 hop = 2880 サンプル = 60 ms) は tools/sim_stream.py で実測する。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
