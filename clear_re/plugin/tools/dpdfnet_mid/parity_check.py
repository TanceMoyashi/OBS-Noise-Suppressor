#!/usr/bin/env python3
"""上流 DPDFNet (.pth) を torch で再現し、公式 ONNX と数値一致することを確かめる。

なぜ必要か:
  中間モデル (dp4) を作るには、上流の重みを torch 側で読み込んで学習・書き出しする
  必要がある。窓・正規化・state の並びの解釈が 1 つでも違うと学習が無意味になるので、
  まず「torch で公式 ONNX と同じ出力が出る」ことを確定させる (このツールがその関門)。

やること:
  1. 上流のストリーミング実装 (onnx_model/dpdfnet_48khz_hr.py) を .pth から作る
  2. 公式 ONNX と、同じ spec 列を 1 フレームずつ流す
  3. spec_e と state_out を比較し、最大誤差を出す
  4. メタデータの state レイアウト (erb_norm_init の先頭値など) も突き合わせる

usage:
  python tools/dpdfnet_mid/parity_check.py \
      --ckpt build/dpdfnet_train/hf/checkpoints/dpdfnet2_48khz_hr.pth \
      --onnx models/dpdfnet2_48khz_hr.onnx \
      --upstream build/dpdfnet_train/upstream

依存: torch, onnxruntime, numpy (soundfile 任意)
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np
import onnxruntime as ort
import torch

HERE = Path(__file__).resolve().parent
TOOLS = HERE.parent
REPO_PLUGIN = TOOLS.parent


def add_paths(upstream: Path) -> None:
    for p in (str(upstream), str(TOOLS)):
        if p not in sys.path:
            sys.path.insert(0, p)
    if not (upstream / "onnx_model" / "dpdfnet_48khz_hr.py").is_file():
        raise SystemExit(
            f"ERROR: 上流コードが見つからない: {upstream}\n"
            "  git clone --depth 1 https://github.com/ceva-ip/DPDFNet.git <upstream>"
        )


def make_onnx_session(model: Path) -> ort.InferenceSession:
    so = ort.SessionOptions()
    so.intra_op_num_threads = 1
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    return ort.InferenceSession(str(model), sess_options=so, providers=["CPUExecutionProvider"])


def vorbis_window(win_len: int) -> np.ndarray:
    from check_onnx_model import vorbis_window as vw  # tools/ 側の実装を再利用

    return vw(win_len)


def stft_frames(x: np.ndarray, win_len: int, hop: int) -> np.ndarray:
    from check_onnx_model import stft_frames as sf

    return sf(x, win_len, hop)


def load_stream_model(upstream: Path, ckpt: Path, blocks: int, device: torch.device):
    """上流のストリーミング実装を .pth から構築する (公式エクスポートと同じ経路)。"""
    from onnx_model.dpdfnet_48khz_hr import DPDFNet48HR, correct_state_dict
    from onnx_model.export_dpdfnet_48khz_hr_to_onnx import DPDFNet48HROnnxWrapper

    model = DPDFNet48HR(
        conv_kernel_inp=(3, 3),
        conv_ch=64,
        enc_gru_dim=256,
        erb_dec_gru_dim=256,
        df_dec_gru_dim=256,
        enc_lin_groups=32,
        lin_groups=16,
        upsample_conv_type="subpixel",
        group_linear_type="loop",
        point_wise_type="cnn",
        separable_first_conv=True,
        dprnn_num_blocks=blocks,
    )
    sd = torch.load(str(ckpt), map_location="cpu", weights_only=True)
    sd = correct_state_dict(sd)
    missing, unexpected = model.load_state_dict(sd, strict=False)
    if missing or unexpected:
        raise SystemExit(f"ERROR: state_dict 不一致\n  missing={missing}\n  unexpected={unexpected}")
    model.eval()
    # ONNX の入力 spec は「wnorm を掛ける前」の生スペクトル。公式エクスポートは
    # ラッパー (spec*wnorm -> model -> *inv_wnorm) を通すので、同じ経路で比較する。
    wrapper = DPDFNet48HROnnxWrapper(model).eval()
    setattr(wrapper, "state_size", model.state_size)
    return wrapper.to(device)


def main() -> int:
    ap = argparse.ArgumentParser(description="torch vs 公式 ONNX の数値一致チェック")
    ap.add_argument("--upstream", type=Path,
                    default=REPO_PLUGIN / "build" / "dpdfnet_train" / "upstream")
    ap.add_argument("--ckpt", type=Path, required=True)
    ap.add_argument("--onnx", type=Path, required=True)
    ap.add_argument("--blocks", type=int, default=2)
    ap.add_argument("--wav", type=Path, nargs="*", default=None,
                    help="実音声での比較に使う WAV (48 kHz 推奨)。省略時は test_signals の音声 2 本")
    ap.add_argument("--frames", type=int, default=40, help="ランダム spec で比較するフレーム数")
    ap.add_argument("--snr-db", type=float, default=60.0,
                    help="ONNX と torch の全体 SNR の下限 (dB)")
    args = ap.parse_args()

    add_paths(args.upstream)
    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")

    sess = make_onnx_session(args.onnx)
    meta = sess.get_modelmeta().custom_metadata_map
    ins = sess.get_inputs()
    freq_bins = int(ins[0].shape[-2])
    state_size = int(ins[1].shape[0])
    print(f"[ONNX] {args.onnx.name}: freq_bins={freq_bins} state_size={state_size} "
          f"profile={meta.get('profile')}")

    model = load_stream_model(args.upstream, args.ckpt, args.blocks, device)
    mdl = model.model  # 実体 (wrapper は wnorm のスケーリングだけを足す)
    print(f"[TORCH] {mdl.__class__.__name__} blocks={args.blocks} params="
          f"{sum(p.numel() for p in mdl.parameters()) / 1e6:.3f}M")
    print(f"[TORCH] state_size = {mdl.state_size()}  (ONNX: {state_size})")

    # ---- state の並び (先頭は erb/spec 正規化の初期値) ----
    erb_size = int(meta["erb_norm_state_size"])
    spec_size = int(meta["spec_norm_state_size"])
    onnx_init = np.zeros(state_size, np.float32)
    onnx_init[:erb_size] = np.array([float(v) for v in meta["erb_norm_init"].split(",")], np.float32)
    onnx_init[erb_size:erb_size + spec_size] = np.array(
        [float(v) for v in meta["spec_norm_init"].split(",")], np.float32)
    # 初期 state は CPU 上で作る (一部モジュールが登録済み buffer を device 無視で使うため)
    model_cpu = model.cpu()
    torch_init = model_cpu.model.initial_state(
        device=torch.device("cpu"), dtype=torch.float32).numpy()
    model.to(device)
    d = np.abs(onnx_init - torch_init)
    print(f"[STATE] init 最大差 = {d.max():.3e} (非ゼロ要素 {int((d > 1e-9).sum())} / {state_size})")
    if d.max() > 1e-6:
        bad = np.nonzero(d > 1e-6)[0][:10]
        print(f"[STATE][WARN] 差のある位置: {bad.tolist()}")

    # ---- 比較用の spec 列を作る (実音声のみ。無音に抑圧される合成信号は相対指標が使えない) ----
    from check_onnx_model import read_wav, naive_resample

    wavs = args.wav
    if not wavs:
        wavs = [REPO_PLUGIN / "test_signals" / "speech_tail_48k.wav",
                REPO_PLUGIN / "test_signals" / "speech_tail_reverb_48k.wav"]
    frames: list[tuple[str, np.ndarray]] = []
    for w in wavs:
        w = Path(w)
        if not w.is_file():
            raise SystemExit(f"ERROR: wav not found: {w}")
        x, sr = read_wav(w)
        x = naive_resample(x, sr, 48000)
        spec = stft_frames(x, freq_bins * 2 - 2, 480).astype(np.complex64)
        frames.append((w.stem, spec))
        print(f"[IN] {w.name}: {len(x)} samples @48000 -> {spec.shape[0]} frames")

    ok = True
    for label, spec_in in frames:
        onnx_state = onnx_init.copy()
        torch_state = torch.from_numpy(torch_init.copy())
        ri = np.zeros((1, 1, freq_bins, 2), np.float32)
        max_se = 0.0
        max_st = 0.0
        max_se_t0 = 0.0
        sig_pow = 0.0
        err_pow = 0.0
        with torch.no_grad():
            for t in range(spec_in.shape[0]):
                ri[0, 0, :, 0] = spec_in[t].real
                ri[0, 0, :, 1] = spec_in[t].imag
                got = sess.run(None, {"spec": ri, "state_in": onnx_state})
                names = [o.name for o in sess.get_outputs()]
                o_spec = got[names.index("spec_e")]
                o_state = got[names.index("state_out")]

                t_in = torch.from_numpy(ri).to(device)
                t_spec, t_state = model(t_in, torch_state.to(device))
                t_spec = t_spec.cpu().numpy()
                t_state = t_state.cpu().numpy()

                d = float(np.abs(o_spec - t_spec).max())
                max_se = max(max_se, d)
                if t == 0:
                    max_se_t0 = d
                max_st = max(max_st, float(np.abs(o_state - t_state).max()))
                sig_pow += float(np.sum(o_spec.astype(np.float64) ** 2))
                err_pow += float(np.sum((o_spec - t_spec).astype(np.float64) ** 2))
                onnx_state, torch_state = o_state, torch.from_numpy(t_state)
        snr = 10.0 * np.log10((sig_pow + 1e-30) / (err_pow + 1e-30))
        print(f"[{label}] frames={spec_in.shape[0]}  t=0 の spec_e 最大差={max_se_t0:.3e}  "
              f"全体 最大差={max_se:.3e}")
        print(f"         state_out 最大差={max_st:.3e}  全体 SNR={snr:.1f} dB "
              f"(浮動小数の丸めだけで説明できる差か)")
        # 判定: 実音声では 16 秒流すと GRU 隠れ状態の丸め差が蓄積するため、
        # 絶対値ではなく「全体 SNR」で 60 dB 以上を要求する。
        gate = snr >= args.snr_db
        ok = ok and gate
        print(f"         -> {'一致 (合格)' if gate else '不一致 (不合格)'}  snr_min={args.snr_db:.0f} dB")

    print("\n[結論] " + ("数値一致の関門 通過。" if ok else "数値一致せず。窓/正規化/state の解釈を見直すこと。"))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
