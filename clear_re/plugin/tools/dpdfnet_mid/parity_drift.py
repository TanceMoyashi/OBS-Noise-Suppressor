#!/usr/bin/env python3
"""parity_check の補助: ONNX と torch のフレームごとの差が「初期から」か「徐々にドリフト」かを調べる。"""
from __future__ import annotations

import sys
from pathlib import Path

import numpy as np
import torch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))          # tools/
sys.path.insert(0, str(HERE))

from parity_check import add_paths, load_stream_model, make_onnx_session, stft_frames  # noqa: E402
from check_onnx_model import read_wav, naive_resample  # noqa: E402

PLUGIN = HERE.parent.parent
UPSTREAM = PLUGIN / "build" / "dpdfnet_train" / "upstream"
CKPT = PLUGIN / "build" / "dpdfnet_train" / "hf" / "checkpoints" / "dpdfnet2_48khz_hr.pth"
ONNX = PLUGIN / "models" / "dpdfnet2_48khz_hr.onnx"
WAV = PLUGIN / "test_signals" / "speech_tail_48k.wav"


def main() -> int:
    add_paths(UPSTREAM)
    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    sess = make_onnx_session(ONNX)
    meta = sess.get_modelmeta().custom_metadata_map
    freq_bins = int(sess.get_inputs()[0].shape[-2])
    erb_size = int(meta["erb_norm_state_size"])
    spec_size = int(meta["spec_norm_state_size"])
    state_size = int(meta["state_size"])
    init = np.zeros(state_size, np.float32)
    init[:erb_size] = np.array([float(v) for v in meta["erb_norm_init"].split(",")], np.float32)
    init[erb_size:erb_size + spec_size] = np.array(
        [float(v) for v in meta["spec_norm_init"].split(",")], np.float32)

    model = load_stream_model(UPSTREAM, CKPT, 2, device)
    x, sr = read_wav(WAV)
    x = naive_resample(x, sr, 48000)[: int(48000 * 6.0)]
    spec_in = stft_frames(x, freq_bins * 2 - 2, 480).astype(np.complex64)
    print(f"frames={spec_in.shape[0]}")

    onnx_state = init.copy()
    torch_state = torch.from_numpy(init.copy())
    ri = np.zeros((1, 1, freq_bins, 2), np.float32)
    names = [o.name for o in sess.get_outputs()]
    rows = []
    with torch.no_grad():
        for t in range(spec_in.shape[0]):
            ri[0, 0, :, 0] = spec_in[t].real
            ri[0, 0, :, 1] = spec_in[t].imag
            got = sess.run(None, {"spec": ri, "state_in": onnx_state})
            o_spec, o_state = got[names.index("spec_e")], got[names.index("state_out")]
            t_spec, t_state = model(torch.from_numpy(ri).to(device), torch_state.to(device))
            t_spec = t_spec.cpu().numpy()
            t_state = t_state.cpu().numpy()
            d_spec = float(np.abs(o_spec - t_spec).max())
            d_state = float(np.abs(o_state - t_state).max())
            rows.append((t, d_spec, d_state))
            onnx_state, torch_state = o_state, torch.from_numpy(t_state)

    for t, ds, dst in rows:
        if t < 8 or t % 25 == 0 or (ds > 2e-4 and rows[t - 1][1] <= 2e-4):
            print(f"  t={t:5d}  d_spec={ds:.3e}  d_state={dst:.3e}")
    a = np.array([[r[1], r[2]] for r in rows])
    print(f"[summary] d_spec mean={a[:,0].mean():.3e} max={a[:,0].max():.3e} | "
          f"d_state mean={a[:,1].mean():.3e} max={a[:,1].max():.3e}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
