#!/usr/bin/env python3
"""中間モデル (学習済みオフライン .pth) を ONNX に書き出し、2 出力化して検証する。

既存モデルと同じ契約 (入力 `spec` [1,1,481,2] / `state_in`、出力 `spec_e` + `state_out`)
になるよう、上流のエクスポート経路と clear_re/plugin 側の道具をそのまま使う。

手順:
  1. 上流 onnx_model/export_dpdfnet_48khz_hr_to_onnx.py
       (オフライン .pth -> correct_state_dict -> ストリーミング実装 -> ONNX)
  2. tools/add_second_output.py           (spec_ns を追加して 2 出力化)
  3. tools/check_onnx_model.py            (契約と動作の確認)
  4. tools/verify_two_output.py           (1 出力と 2 出力の一致確認)

usage:
  python tools/dpdfnet_mid/export_mid.py --ckpt build/dpdfnet_train/ckpt_mid/dp4.best.pth \
      --name dpdfnet4_48khz_hr --blocks 4 --out-dir build/dpdfnet_train/onnx
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

from mid_model import DEFAULT_UPSTREAM, PLUGIN


def run(cmd: list[str], env: dict[str, str] | None = None, cwd: Path | None = None) -> None:
    print("[run] " + " ".join(str(c) for c in cmd), flush=True)
    r = subprocess.run([str(c) for c in cmd], env=env, cwd=str(cwd or PLUGIN))
    if r.returncode != 0:
        raise SystemExit(f"ERROR: {' '.join(str(c) for c in cmd)} が失敗 ({r.returncode})")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--upstream", type=Path, default=DEFAULT_UPSTREAM)
    ap.add_argument("--ckpt", type=Path, required=True, help="オフライン .pth (state_dict)")
    ap.add_argument("--name", default="dpdfnet4_48khz_hr")
    ap.add_argument("--blocks", type=int, default=4)
    ap.add_argument("--out-dir", type=Path, default=None)
    ap.add_argument("--wav", type=Path, default=PLUGIN / "test_signals" / "speech_with_noise.wav")
    ap.add_argument("--tensor-name", default="/model/mask/Mul_output_0",
                    help="spec_ns に使う中間テンソル (tools/add_second_output.py --list で確認)")
    ap.add_argument("--skip-verify", action="store_true")
    args = ap.parse_args()

    out_dir = (args.out_dir or (PLUGIN / "build" / "dpdfnet_train" / "onnx")).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    onnx_1out = out_dir / f"{args.name}.onnx"
    onnx_2out = out_dir / f"{args.name}_2out.onnx"

    env = dict(os.environ)
    env["PYTHONPATH"] = str(args.upstream.resolve())
    env.setdefault("PYTHONIOENCODING", "utf-8")

    run([sys.executable, "-m", "onnx_model.export_dpdfnet_48khz_hr_to_onnx",
         "--output", onnx_1out, "--checkpoint", args.ckpt.resolve(),
         "--dprnn-num-blocks", args.blocks, "--exporter", "legacy",
         "--skip-validation"], env=env, cwd=args.upstream.parent)
    # 中間テンソルの shape 情報 (value_info) を入れておく。add_second_output.py は
    # `/model/mask/Mul_output_0` が rank 5 [1,1,1,F,2] であることを value_info から
    # 判定して Squeeze を挟むため、これが無いと spec_ns の次元が 1 つ多くなる。
    run([sys.executable, "-c",
         "import onnx;from onnx import shape_inference as si;p=r'" + str(onnx_1out) + "';"
         "m=onnx.load(p);m=si.infer_shapes(m,strict_mode=False,data_prop=True);"
         "onnx.save(m,p);print('[OK] shape_inference')"])
    run([sys.executable, "tools/add_second_output.py", "--model", onnx_1out,
         "--tensor-name", args.tensor_name, "--out", onnx_2out])

    if args.skip_verify:
        print(f"[ok ] {onnx_1out}\n[ok ] {onnx_2out}")
        return 0

    run([sys.executable, "tools/check_onnx_model.py", "--model", onnx_2out,
         "--wav", args.wav, "--out", out_dir / f"{args.name}_2out_check.wav"])
    run([sys.executable, "tools/verify_two_output.py", "--base", onnx_1out, "--two", onnx_2out,
         "--wav", args.wav])
    print(f"[ok ] {onnx_1out}\n[ok ] {onnx_2out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
