#!/usr/bin/env python3
"""state_out のどの区間に差が出るかを特定する (t=0 の 1 フレームだけで調べる)。"""
from __future__ import annotations

import sys
from pathlib import Path

import numpy as np
import torch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE))

from parity_check import add_paths, load_stream_model, make_onnx_session  # noqa: E402
from check_onnx_model import read_wav, naive_resample  # noqa: E402

PLUGIN = HERE.parent.parent
UPSTREAM = PLUGIN / "build" / "dpdfnet_train" / "upstream"
CKPT = PLUGIN / "build" / "dpdfnet_train" / "hf" / "checkpoints" / "dpdfnet2_48khz_hr.pth"
ONNX = PLUGIN / "models" / "dpdfnet2_48khz_hr.onnx"
WAV = PLUGIN / "test_signals" / "speech_tail_48k.wav"


def segments(mdl) -> list[tuple[str, int]]:
    """initial_state() と同じ順序で (名前, サイズ) を返す。"""
    out = []
    out.append(("erb_norm(MagNorm48)", mdl.erb_norm.state_size()))
    out.append(("spec_norm(SpecNorm48)", mdl.spec_norm.state_size()))
    enc = mdl.enc
    for name in ("erb_conv0_buffer", "dprnn_erb", "df_conv0_buffer", "dprnn_df", "emb_gru"):
        m = getattr(enc, name)
        if hasattr(m, "state_size"):
            out.append((f"enc.{name}", m.state_size()))
    if hasattr(mdl.erb_dec, "state_size"):
        out.append(("erb_dec.emb_gru", mdl.erb_dec.state_size()))
    out.append(("df_dec.df_gru", mdl.df_dec.df_gru.state_size()))
    out.append(("df_dec.df_convp_buffer", mdl.df_dec.df_convp_buffer.state_size()))
    out.append(("mask(MagnitudeMask)", mdl.mask.state_size()))
    out.append(("df_op", mdl.df_op.state_size()))
    return out


def main() -> int:
    add_paths(UPSTREAM)
    sess = make_onnx_session(ONNX)
    meta = sess.get_modelmeta().custom_metadata_map
    fb = int(sess.get_inputs()[0].shape[-2])
    erb, sp = int(meta["erb_norm_state_size"]), int(meta["spec_norm_state_size"])
    ss = int(meta["state_size"])
    init = np.zeros(ss, np.float32)
    init[:erb] = np.array([float(v) for v in meta["erb_norm_init"].split(",")], np.float32)
    init[erb:erb + sp] = np.array([float(v) for v in meta["spec_norm_init"].split(",")], np.float32)

    model = load_stream_model(UPSTREAM, CKPT, 2, torch.device("cpu"))
    mdl = model.model
    segs = segments(mdl)
    print(f"state_size={ss}  合計={sum(s for _, s in segs)}")
    lo = 0
    for name, s in segs:
        print(f"  {lo:6d}..{lo+s:6d} ({s:5d})  {name}")
        lo += s

    x, sr = read_wav(WAV)
    from check_onnx_model import stft_frames
    x = naive_resample(x, sr, 48000)[:48000]
    spec = stft_frames(x, (fb - 1) * 2, 480)
    ri = np.zeros((1, 1, fb, 2), np.float32)
    ri[0, 0, :, 0] = spec[0].real
    ri[0, 0, :, 1] = spec[0].imag
    names = [o.name for o in sess.get_outputs()]
    got = sess.run(None, {"spec": ri, "state_in": init.copy()})
    o_spec, o_state = got[names.index("spec_e")], got[names.index("state_out")]
    with torch.no_grad():
        t_spec, t_state = model(torch.from_numpy(ri), torch.from_numpy(init.copy()))
    t_spec, t_state = t_spec.numpy(), t_state.numpy()
    print(f"spec_e 最大差={np.abs(o_spec - t_spec).max():.3e}")
    d = np.abs(o_state - t_state)
    lo = 0
    for name, s in segs:
        seg = d[lo:lo + s]
        print(f"  {name:26s} 最大差={seg.max():.3e} 平均={seg.mean():.3e} 非ゼロ={int((seg>1e-9).sum())}")
        lo += s
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
