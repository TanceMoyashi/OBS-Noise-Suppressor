#!/usr/bin/env python3
"""ストリーミング遅延の実測 (C++ 実装 vs 公式実装)。

このツールが答える問い:
  「getLatencySamples() に設定すべき値は何か?」

遅延の定義:
  出力ストリームの位置 p のサンプルが、入力の位置 p-L のサンプルを強調した
  ものになっているとき L を内容遅延と呼ぶ。ホストの遅延補償 (PDC) はこの L を
  打ち消す。L を正確に報告しないと OBS で他トラックとずれる。

計測:
  実装を「出力位置 p は入力位置 p を読んでいる最中に書く」という実際の規約で
  シミュレーションし、入力と出力の相互相関のピーク位置を L とする。
  出力の「先頭に何サンプルの無音が入るか」が L に直接効くので、規約を
  推測せずに実装と同じ手順で書くことが重要。

評価する出力位置の規約:
  (a) sample-loop: 現在の C++ (runHop が hop を完成させた時点で次のサンプルから書く)
  (b) FIFO+prime: 完成した hop を順に詰め、先頭に prime hop の無音を置く
      (ブロック境界に依存せず常に健全。prime が大きいほど遅延が増える)
  (c) official: 公式 StreamEnhancer (causal, 呼び出しごとに 1 hop 返す)

参照: dpdfnet.enhance (オフライン) は先頭 4 hop を捨てる補正が入るので L=0。
"""

from __future__ import annotations

import argparse
import sys
import wave
from pathlib import Path

import numpy as np
import onnxruntime as ort

from check_onnx_model import synth_test_signal, vorbis_window


def read_wav_mono(path: Path, target_sr: int) -> np.ndarray:
    import librosa

    with wave.open(str(path), "rb") as w:
        n_ch, width, sr, n = w.getnchannels(), w.getsampwidth(), w.getframerate(), w.getnframes()
        raw = w.readframes(n)
    if width == 2:
        x = np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0
    else:
        x = np.frombuffer(raw, dtype="<i4").astype(np.float32) / 2147483648.0
    if n_ch > 1:
        x = x.reshape(-1, n_ch).mean(axis=1)
    if sr != target_sr:
        x = librosa.resample(np.asarray(x, np.float32), orig_sr=sr, target_sr=target_sr)
    return np.asarray(x, dtype=np.float32)


def make_session(model: Path) -> ort.InferenceSession:
    so = ort.SessionOptions()
    so.intra_op_num_threads = 1
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    return ort.InferenceSession(str(model), sess_options=so, providers=["CPUExecutionProvider"])


def load_init_state(sess) -> np.ndarray:
    m = sess.get_modelmeta().custom_metadata_map
    erb, sp = int(m["erb_norm_state_size"]), int(m["spec_norm_state_size"])
    s = np.zeros(int(m["state_size"]), np.float32)
    s[:erb] = np.array([float(v) for v in m["erb_norm_init"].split(",")], np.float32)
    s[erb:erb + sp] = np.array([float(v) for v in m["spec_norm_init"].split(",")], np.float32)
    return s


def rms(v: np.ndarray) -> float:
    v = np.asarray(v, np.float64)
    return float(np.sqrt(np.mean(v * v))) if v.size else 0.0


def lag_samples(ref: np.ndarray, test: np.ndarray, max_lag: int = 24000) -> tuple[int, float]:
    """test が ref より何サンプル遅れているか (正 = test が遅い)。"""
    n = min(len(ref), len(test))
    a = ref[:n] - ref[:n].mean()
    b = test[:n] - test[:n].mean()
    den = (np.linalg.norm(a) * np.linalg.norm(b)) or 1.0
    c = np.correlate(b, a, mode="full") / den
    lags = np.arange(-(n - 1), n)
    keep = np.abs(lags) <= max_lag
    i = int(np.argmax(np.abs(c[keep])))
    return int(lags[keep][i]), float(np.abs(c[keep][i]))


def infer(sess, ri: np.ndarray, state: np.ndarray):
    return sess.run(["spec_e", "state_out"], {"spec": ri, "state_in": state})


# ---------------------------------------------------------------------------
# C++ 実装 (src/VorbisStft.cpp + PluginProcessor.cpp) の忠実な再現
# ---------------------------------------------------------------------------
def sim_cpp(x: np.ndarray, sess, win_len: int, hop: int, skip_steps: int = 2):
    """戻り値: (sample-loop の出力, 放出 hop を連結したストリーム)

    analyse:   inBuf を hop 左シフトし新しい hop を末尾に足す ->
               フレーム s は入力 [(s-1)*hop, (s+1)*hop) を覆う
    synthesise: acc を hop ずらして窓掛け波形を加算。初回は emit しない
    runHop:    step < skip_steps は合成しない (モデル出力がプリヒストリのため)
    """
    freq_bins = win_len // 2 + 1
    window = vorbis_window(win_len)

    n_hops = int(np.ceil(len(x) / hop))
    total = (n_hops + 4) * hop
    out = np.zeros(total, np.float32)          # sample-loop 出力
    emissions: list[np.ndarray] = []

    inbuf = np.zeros(win_len, np.float32)
    acc = np.zeros(win_len + hop, np.float32)
    state = load_init_state(sess)

    first_synth = True
    for s in range(n_hops):
        hopin = x[s * hop:(s + 1) * hop]
        if len(hopin) < hop:
            hopin = np.concatenate([hopin, np.zeros(hop - len(hopin), np.float32)])

        inbuf = np.concatenate([inbuf[hop:], hopin]).astype(np.float32)
        frame = np.fft.rfft(inbuf * window, n=win_len).astype(np.complex64)

        ri = np.zeros((1, 1, freq_bins, 2), np.float32)
        ri[0, 0, :, 0] = frame.real
        ri[0, 0, :, 1] = frame.imag
        spec_e, state = infer(sess, ri, state)

        if s >= skip_steps:
            t = np.fft.irfft(spec_e[0, 0, :, 0] + 1j * spec_e[0, 0, :, 1],
                             n=win_len).astype(np.float32) * window

            emit = not first_synth
            if first_synth:
                acc[:] = 0.0
                first_synth = False
            else:
                acc = np.concatenate([acc[hop:], np.zeros(hop, np.float32)])

            acc[:win_len] += t

            if emit:
                hop_out = acc[:hop].copy()
                emissions.append(hop_out)
                # processBlock: hop 完成サンプル (s+1)*hop-1 を読み終えた直後から書く
                start = (s + 1) * hop
                if start + hop <= total:
                    out[start:start + hop] += hop_out

    em = np.concatenate(emissions) if emissions else np.zeros(0, np.float32)
    return out[:len(x)], em


def sim_fifo(em: np.ndarray, prime_hops: int, hop: int, n: int) -> np.ndarray:
    """FIFO モデル: 先頭に prime hop の無音、そのあと放出 hop を順に並べる。"""
    y = np.concatenate([np.zeros(prime_hops * hop, np.float32), em])
    if len(y) < n:
        y = np.concatenate([y, np.zeros(n - len(y), np.float32)])
    return y[:n]


def sim_official(x: np.ndarray, sess, win_len: int, hop: int) -> np.ndarray:
    """公式 StreamEnhancer (causal, center=False) を sounddevice と同じ規約で再現。"""
    freq_bins = win_len // 2 + 1
    window = vorbis_window(win_len)

    n_hops = int(np.ceil(len(x) / hop))
    total = (n_hops + 4) * hop
    out = np.zeros(total, np.float32)

    in_buf = np.zeros(0, np.float32)
    acc = np.zeros(win_len, np.float32)
    state = load_init_state(sess)

    for b in range(n_hops):
        chunk = x[b * hop:(b + 1) * hop]
        if len(chunk) < hop:
            chunk = np.concatenate([chunk, np.zeros(hop - len(chunk), np.float32)])
        in_buf = np.concatenate([in_buf, chunk])

        while len(in_buf) >= win_len:
            seg = in_buf[:win_len] * window
            frame = np.fft.rfft(seg, n=win_len).astype(np.complex64)

            ri = np.zeros((1, 1, freq_bins, 2), np.float32)
            ri[0, 0, :, 0] = frame.real
            ri[0, 0, :, 1] = frame.imag
            spec_e, state = infer(sess, ri, state)

            t = np.fft.irfft(spec_e[0, 0, :, 0] + 1j * spec_e[0, 0, :, 1],
                             n=win_len).astype(np.float32) * window
            acc[:win_len] += t

            emitted = acc[:hop].copy()
            acc = np.concatenate([acc[hop:], np.zeros(hop, np.float32)])
            in_buf = in_buf[hop:]

            # process(chunk) の戻り値は今の chunk と同じ時間範囲の出力
            start = b * hop
            if start + hop <= total:
                out[start:start + hop] += emitted

    return out[:len(x)]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", type=Path, default=Path("models/dpdfnet2_48khz_hr.onnx"))
    ap.add_argument("--wav", type=Path, default=Path("test_signals/speech_with_noise.wav"))
    ap.add_argument("--seconds", type=float, default=4.0)
    args = ap.parse_args()

    SR, hop, win_len = 48000, 480, 960

    if args.wav is not None and args.wav.is_file():
        raw = read_wav_mono(args.wav, SR)
        x = np.concatenate([np.zeros(int(0.2 * SR), np.float32), raw])
        if len(x) > args.seconds * SR:
            x = x[: int(args.seconds * SR)]
        desc = f"{args.wav.name} -> {SR} Hz"
    else:
        x = synth_test_signal(SR, args.seconds)
        desc = "synthetic"

    print(f"[IN ] {desc}: {len(x)} samples  rms={rms(x):.6f}")
    sess = make_session(args.model)

    # ---- 参照: 公式オフライン (4 hop 補正込み) ----
    try:
        import dpdfnet
        y_full = dpdfnet.enhance(x, SR, model="dpdfnet2_48khz_hr", onnx_path=str(args.model))
        l4, c4 = lag_samples(x, y_full)
        print(f"[REF] dpdfnet.enhance (offline, 先頭 4 hop 補正): L={l4:6d} "
              f"({l4/hop:4.2f} hop) corr={c4:.3f}   <- 0 になるはず (ツールの健全性)")
    except ImportError:
        print("[REF] dpdfnet 未導入のためスキップ")

    # ---- 公式リアルタイム経路 ----
    y_off = sim_official(x, sess, win_len, hop)
    lo, co = lag_samples(x, y_off)
    print(f"[OFF] official StreamEnhancer (忠実な規約): L={lo:6d} "
          f"= {lo/hop:4.2f} hop = {lo/SR*1000:5.1f} ms  corr={co:.3f}")

    # ---- C++ ----
    print("\n[C++] C++ 実装の内容遅延")
    for sk in (0, 1, 2):
        y_loop, em = sim_cpp(x, sess, win_len, hop, skip_steps=sk)
        ll, cl = lag_samples(x, y_loop)
        rl = rms(y_loop)
        print(f"  skip={sk}")
        print(f"    (a) sample-loop (現行実装)          : L={ll:6d} = {ll/hop:4.2f} hop"
              f" = {ll/SR*1000:5.1f} ms   rms={rl:.6f}  corr={cl:.3f}")
        ce, _ = lag_samples(x, em)
        print(f"        放出 hop を連結しただけ        : L={ce:6d} = {ce/hop:4.2f} hop")
        for pr in (0, 1, 2, 3, 4):
            yf = sim_fifo(em, pr, hop, len(x))
            lf, cf = lag_samples(x, yf)
            print(f"    (b) FIFO prime={pr} hop              : L={lf:6d} = {lf/hop:4.2f} hop"
                  f" = {lf/SR*1000:5.1f} ms   corr={cf:.3f}")

    print("\n[NOTE] PluginProcessor.h の kLatencyHops は採用した規約の L/hop と一致させる。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
