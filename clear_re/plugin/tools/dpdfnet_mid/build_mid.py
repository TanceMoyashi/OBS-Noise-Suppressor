#!/usr/bin/env python3
"""中間モデル (dp4) の初期値を作る = dp8 の重みから DPRNN ブロック 0〜3 を引き継ぐ。

usage:
  python tools/dpdfnet_mid/build_mid.py \
      --src build/dpdfnet_train/hf/checkpoints/dpdfnet8_48khz_hr.pth \
      --blocks 4 --out build/dpdfnet_train/ckpt_mid/init_dp4.pth
"""

from __future__ import annotations

import argparse
from pathlib import Path

import torch

from mid_model import DEFAULT_UPSTREAM, PLUGIN, add_offline_paths, build_offline, load_dp8_into_mid


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--upstream", type=Path, default=DEFAULT_UPSTREAM)
    ap.add_argument("--src", type=Path,
                    default=PLUGIN / "build" / "dpdfnet_train" / "hf" / "checkpoints"
                    / "dpdfnet8_48khz_hr.pth")
    ap.add_argument("--blocks", type=int, default=4)
    ap.add_argument("--out", type=Path,
                    default=PLUGIN / "build" / "dpdfnet_train" / "ckpt_mid" / "init_dp4.pth")
    args = ap.parse_args()

    add_offline_paths(args.upstream)
    model = build_offline(args.blocks, "cpu")
    n_par = sum(p.numel() for p in model.parameters())
    print(f"[mid] DPRNN {args.blocks} ブロック / {n_par/1e6:.3f}M パラメータ")
    info = load_dp8_into_mid(model, args.src)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    torch.save(model.state_dict(), str(args.out))
    print(f"[ok ] {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
