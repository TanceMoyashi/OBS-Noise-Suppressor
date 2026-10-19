#!/usr/bin/env python3
"""中間モデル (dp4 = DPRNN 4 ブロック) の共通部品。

上流 DPDFNet の 48 kHz HR モデルは `dprnn_num_blocks` だけが dp2 / dp8 の違い:
  dp2 = DPRNN 2 ブロック, dp4 (公式は 16 kHz のみ) = 4, dp8 = 8。
つまり「中間モデル」= `dprnn_num_blocks=4` で、追加の部品は要らない。
重みは dp8 の DPRNN ブロック 0〜3 をそのまま初期値に使う (残差ブロックなので
後半 4 ブロックを落とした状態から微調整すると回復が速い)。

ここでは 2 つの実装を扱う:
  * オフライン実装 (upstream/model/dpdfnet_48khz_hr.py) … 学習に使う (波形をそのまま渡せる)
  * ストリーミング実装 (upstream/onnx_model/...)      … ONNX へ書き出すときに上流が使う
学習はオフライン実装で行い、書き出しは上流のエクスポート経路
(`model/*.pth` → `correct_state_dict` → ストリーミング実装 → ONNX) に載せる。
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

import numpy as np
import torch
from torch import nn

# 公式エクスポート (export_dpdfnet_48khz_hr_to_onnx.py の build_model) と同一の設定
MODEL_KWARGS: dict[str, Any] = dict(
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
)

HERE = Path(__file__).resolve().parent
PLUGIN = HERE.parent.parent
DEFAULT_UPSTREAM = PLUGIN / "build" / "dpdfnet_train" / "upstream"

# tools/ を import パスに入れる (check_onnx_model.py の窓/iSTFT を再利用するため)
if str(HERE.parent) not in sys.path:
    sys.path.insert(0, str(HERE.parent))


def add_offline_paths(upstream: Path = DEFAULT_UPSTREAM) -> None:
    """上流の *オフライン* 実装を import できるようにする。

    `upstream/model/*.py` は `from modules import ...` のようなトップレベル import を
    使うので、`upstream/model` を sys.path に入れる必要がある。
    """
    if not (upstream / "model" / "dpdfnet_48khz_hr.py").is_file():
        raise SystemExit(f"ERROR: 上流コードが見つからない: {upstream}")
    for p in (str(upstream), str(upstream / "model")):
        if p in sys.path:
            sys.path.remove(p)
    sys.path.insert(0, str(upstream))
    sys.path.insert(0, str(upstream / "model"))


def patch_norm_device() -> None:
    """上流の MagNorm48 / SpecNorm48 は初期値を CPU テンソルで作るため CUDA で落ちる。

    計算内容は変えず、初期値テンソルを入力と同じ device に移すだけの patch を当てる。
    (これらはパラメータを持たないので state_dict には影響しない)
    """
    import modules as M
    import torch as _t

    def mag_fwd(self, x):
        b, t, f = x.shape
        if self.mu is None or not self.training or not self.stateful:
            mu = self.init_vals.get_ampirical_mu_0(f).to(x.device)
            mu = mu[None, :].expand(b, f)
            var = _t.zeros_like(mu) + 40 ** 2
        else:
            mu, var = self.mu, self.var
        outs = []
        for i in range(t):
            mu = self.alpha * mu + (1 - self.alpha) * x[:, i]
            if self.dynamic_var:
                var = self.alpha * var + (1 - self.alpha) * ((x[:, i] - mu) ** 2)
            outs.append((x[:, i] - mu) / (var.sqrt() + self.eps))
        y = _t.stack(outs, dim=1)
        if self.mu is None or not self.training or not self.stateful:
            self.mu = None
            self.var = None
        else:
            self.mu = mu
            self.var = var
        return y

    def spec_fwd(self, x):
        b, t, f, _ = x.shape
        if self.s is None or not self.training or not self.stateful:
            s = self.init_vals.get_ampirical_s_0(f).to(x.device)
            s = s[None, :].expand(b, f)
        else:
            s = self.s
        x_abs = _t.sqrt(x[..., 0] ** 2 + x[..., 1] ** 2)
        rs, is_ = [], []
        for i in range(t):
            s = self.alpha * s + (1 - self.alpha) * x_abs[:, i]
            den = (s + self.eps).sqrt()
            rs.append(x[:, i, :, 0] / den)
            is_.append(x[:, i, :, 1] / den)
        y = _t.stack([_t.stack(rs, dim=1), _t.stack(is_, dim=1)], dim=-1)
        if self.s is None or not self.training or not self.stateful:
            self.s = None
        else:
            self.s = s
        return y

    M.MagNorm48.forward = mag_fwd
    M.SpecNorm48.forward = spec_fwd


def build_offline(blocks: int, device: torch.device | str = "cpu"):
    """オフライン実装 (波形 → 波形) を作る。"""
    import dpdfnet_48khz_hr as M  # upstream/model/dpdfnet_48khz_hr.py

    patch_norm_device()
    model = M.DPDFNet48HR(dprnn_num_blocks=blocks, **MODEL_KWARGS)
    return model.to(device)


class SpecModel(nn.Module):
    """オフライン実装をラップして `spec_e` (DF 後) と `spec_m` (マスクのみ) を返す。

    公式 `forward()` と同じ計算を、iSTFT の手前で止めて返すだけ。
    学習の損失はここに定義する。
    """

    def __init__(self, base):
        super().__init__()
        self.base = base

    def forward(self, waveform: torch.Tensor):
        b = self.base
        spec, feat_erb, feat_spec = b._feature_extraction(waveform)  # no_grad
        feat_spec = feat_spec.squeeze(1).permute(0, 3, 1, 2)
        feat_erb = b.pad_feat(feat_erb)
        feat_spec = b.pad_feat(feat_spec)
        e0, e1, e2, e3, emb, c0, _ = b.enc(feat_erb, feat_spec)
        m = b.erb_dec(emb, e3, e2, e1, e0)
        df_coefs = b.df_out_transform(b.df_dec(emb, c0))
        spec_m = spec * m.unsqueeze(-1)          # mask_method == 'before_df'
        spec_e = b.df_op(spec_m.clone(), df_coefs)
        return spec_e, spec_m


def load_dp8_into_mid(mid, src_ckpt: Path, verbose: bool = True) -> dict[str, int]:
    """dp8 の重みを dp4 (または任意 blocks) に「先頭ブロック優先」でコピーする。

    戻り値: {copied, kept_init, skipped}
    """
    sd = torch.load(str(src_ckpt), map_location="cpu", weights_only=True)
    if "state_dict" in sd and isinstance(sd["state_dict"], dict):
        sd = sd["state_dict"]
    tgt = mid.state_dict()
    new = dict(tgt)
    copied = kept = 0
    skipped: list[str] = []
    for k, v in tgt.items():
        if k in sd and tuple(sd[k].shape) == tuple(v.shape):
            new[k] = sd[k]
            copied += 1
        else:
            kept += 1
            skipped.append(k)
    mid.load_state_dict(new, strict=True)
    if verbose:
        print(f"[init] dp8 からコピーしたテンソル: {copied}, 初期値のまま: {kept}")
        for k in skipped[:10]:
            print(f"       (初期値) {k}")
    return {"copied": copied, "kept_init": kept}


# ---------------------------------------------------------------------------
# 指標
# ---------------------------------------------------------------------------
def snr_db(ref: np.ndarray, est: np.ndarray) -> float:
    """ref を正解とした est の SNR (dB)。"""
    ref = np.asarray(ref, np.float64)
    est = np.asarray(est, np.float64)
    n = min(ref.size, est.size)
    ref, est = ref.reshape(-1)[:n], est.reshape(-1)[:n]
    e = ref - est
    return 10.0 * np.log10((np.sum(ref * ref) + 1e-30) / (np.sum(e * e) + 1e-30))


def complex_snr_db(ref_ri: np.ndarray, est_ri: np.ndarray) -> float:
    """[..., 2] (実部/虚部) 同士の SNR (dB)。"""
    ref = np.asarray(ref_ri, np.float64)
    est = np.asarray(est_ri, np.float64)
    e = ref - est
    return 10.0 * np.log10((np.sum(ref * ref) + 1e-30) / (np.sum(e * e) + 1e-30))


def istft_wave(spec_ri: np.ndarray, win_len: int = 960, hop: int = 480) -> np.ndarray:
    """[T, F, 2] → 波形 (OBS プラグインと同じ重ね合わせ。sim_stream.py と同趣旨)。"""
    from importlib import import_module

    m = import_module("check_onnx_model")
    c = spec_ri[..., 0] + 1j * spec_ri[..., 1]
    return m.istft_frames(c.astype(np.complex64), win_len, hop)


def stft_spec(x: np.ndarray, win_len: int = 960, hop: int = 480) -> np.ndarray:
    from importlib import import_module

    m = import_module("check_onnx_model")
    return m.stft_frames(np.asarray(x, np.float32), win_len, hop)
