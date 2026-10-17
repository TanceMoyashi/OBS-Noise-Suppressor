#!/usr/bin/env python3
"""公式 DPDFNet ONNX に「マスク経路のみ」の出力 (spec_ns) を足して 2 出力化する。

これにより Voice Reverb ノブが機能する:
    spec_ns  = マスク適用のみ  (ノイズ抑圧。DF 経路による残響抑圧が乗らない)
    spec_e   = マスク + DF     (ノイズ抑圧 + 残響抑圧)

再学習は不要。重みは一切変更せず、Identity / Squeeze / Mul の 3 種類だけを足す。

グラフの構造 (tools/inspect_graph.py で確認):
    /model/mask/Mul_output_0          = 遅延整列済みの noisy spec × mask   [1,1,1,481,2]
    /model/Squeeze_output_0           = DF 経路の出力                      [1,1,481,2]
    spec_e = Mul(/model/Squeeze_output_0, inv_wnorm)          inv_wnorm = 960.0 (スカラー)

つまり spec_ns は次で作れる:
    spec_ns = Mul(Squeeze(/model/mask/Mul_output_0, axis=2), inv_wnorm)

手順:
  1) 候補の列挙
     python tools/add_second_output.py --model models/dpdfnet2_48khz_hr.onnx --list
  2) 実際に出力を足す
     python tools/add_second_output.py --model models/dpdfnet2_48khz_hr.onnx \\
         --tensor-name /model/mask/Mul_output_0 \\
         --out models/dpdfnet2_48khz_hr_2out.onnx
  3) 検証 (spec_e が不変であること / spec_ns が別信号であること)
     python tools/verify_two_output.py --base models/dpdfnet2_48khz_hr.onnx \\
         --two models/dpdfnet2_48khz_hr_2out.onnx

rank 5 のテンソルは axis=2 を Squeeze して [1,1,F,2] にし、--scale-const (既定
inv_wnorm) があれば掛ける。C++ 側は spec_ns / spec_m という名前の出力を探す。

依存: onnx
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import onnx
from onnx import TensorProto, helper


def tensor_shapes(graph: onnx.GraphProto) -> dict[str, list]:
    shapes: dict[str, list] = {}
    for vi in list(graph.input) + list(graph.output) + list(graph.value_info):
        t = vi.type.tensor_type
        if not t.HasField("shape"):
            continue
        dims = []
        for d in t.shape.dim:
            dims.append(d.dim_value if d.HasField("dim_value") else d.dim_param or "?")
        shapes[vi.name] = dims
    return shapes


def is_spec_like(dims: list, freq_bins: int) -> bool:
    """[1,1,F,2] か [1,1,1,F,2] の形か。"""
    if not dims or dims[-1] != 2:
        return False
    if len(dims) == 4:
        return dims[-2] == freq_bins
    if len(dims) == 5:
        return dims[-2] == freq_bins and dims[2] == 1
    return False


def main() -> int:
    ap = argparse.ArgumentParser(description="Add a mask-path output (spec_ns) to a DPDFNet ONNX model.")
    ap.add_argument("--model", required=True, type=Path)
    ap.add_argument("--out", type=Path, default=None)
    ap.add_argument("--tensor-name", default=None, help="spec_ns の元にする中間テンソル")
    ap.add_argument("--output-name", default="spec_ns",
                    help="追加する出力名 (C++ は spec_ns / spec_m を認識する)")
    ap.add_argument("--scale-const", default="inv_wnorm",
                    help="最後に掛ける定数初期化子の名前。不要なら --no-scale")
    ap.add_argument("--no-scale", action="store_true")
    ap.add_argument("--squeeze-axis", type=int, default=2,
                    help="rank 5 のとき Squeeze する軸 (既定 2)")
    ap.add_argument("--from-buffer", default=None,
                    help="フレームバッファ (rank 5 [N,1,1,F,2]) の名前。--buffer-index で 1 フレーム取り出す")
    ap.add_argument("--buffer-index", type=int, default=None,
                    help="--from-buffer から取り出す index (0 = 最古)")
    ap.add_argument("--list", action="store_true", help="候補を列挙して終了")
    args = ap.parse_args()

    if not args.model.is_file():
        print(f"ERROR: model not found: {args.model}", file=sys.stderr)
        return 1

    model = onnx.load(str(args.model))
    graph = model.graph

    spec_input = next((i for i in graph.input if i.name == "spec"), None)
    if spec_input is None:
        print("ERROR: no 'spec' input; is this a DPDFNet streaming export?", file=sys.stderr)
        return 1

    spec_dims = [d.dim_value for d in spec_input.type.tensor_type.shape.dim]
    freq_bins = spec_dims[-2]
    print(f"[INFO] spec shape = {spec_dims}  (freq_bins={freq_bins})")

    shapes = tensor_shapes(graph)
    existing = {o.name for o in graph.output}

    consumers: dict[str, list[str]] = {}
    for node in graph.node:
        for inp in node.input:
            consumers.setdefault(inp, []).append(node.op_type)

    candidates = []
    for node in graph.node:
        for out_name in node.output:
            if out_name in existing:
                continue
            dims = shapes.get(out_name)
            if dims and is_spec_like(dims, freq_bins):
                candidates.append((node.op_type, out_name, dims, consumers.get(out_name, [])))
                break

    if args.list:
        print("\n[候補] spec と同じフレーム形 (rank 4/5, 末尾 2) の中間テンソル:")
        for op, name, dims, cons in candidates:
            print(f"  op={op:8s} name={name}  shape={dims}")
            print(f"           consumers: {cons}")
        print("\n[期待] '/model/mask/Mul_output_0' (op=Mul, [1,1,1,F,2])。"
              "DF 経路へ入る前の『マスク適用のみ』で、consumer に DF 系が並ぶ。")
        print("       rank 5 の場合は axis=2 を Squeeze して [1,1,F,2] にし、"
              f"inv_wnorm を掛ける (既定)。")
        if not candidates:
            print("  (見つからない場合は value_info が省略されている)")
        return 0

    if not args.tensor_name and args.from_buffer is None:
        print("ERROR: --tensor-name (または --from-buffer) が必要 (--list 参照)", file=sys.stderr)
        return 1

    src = args.tensor_name or args.from_buffer
    if src in existing:
        print(f"ERROR: {src} is already a graph output", file=sys.stderr)
        return 1

    if args.output_name in existing:
        print(f"ERROR: output name '{args.output_name}' already exists", file=sys.stderr)
        return 1

    dims = shapes.get(src)
    if dims is None:
        print(f"[WARN] {src} の shape が value_info に無い。spec と同じと仮定する。")
        dims = list(spec_dims)

    alias = args.output_name
    cur = src

    # ---- 0) フレームバッファから 1 フレーム取り出す (spec_e との内容整列のため) ----
    if args.from_buffer is not None:
        if args.buffer_index is None:
            print("ERROR: --from-buffer には --buffer-index が必要", file=sys.stderr)
            return 1
        k = args.buffer_index
        for suf, vals in (("starts", [k]), ("ends", [k + 1]), ("axes", [0])):
            graph.initializer.append(
                helper.make_tensor(f"{alias}__slice_{suf}", TensorProto.INT64, [1], vals))
        sliced = f"{alias}__sliced"
        graph.node.append(helper.make_node(
            "Slice", [args.from_buffer, f"{alias}__slice_starts",
                      f"{alias}__slice_ends", f"{alias}__slice_axes"],
            [sliced], name=f"dpdfnet_voice_slice_{alias}"))
        print(f"[OK] Slice(index={k})     : {args.from_buffer} -> {sliced}")
        cur = sliced
        dims = [1] + list(dims[1:])

    # ---- 1) rank 5 [1,1,1,F,2] なら Squeeze して [1,1,F,2] に ----
    if len(dims) == 5:
        axis = args.squeeze_axis
        axes_name = f"{alias}__squeeze_axes"
        graph.initializer.append(
            helper.make_tensor(axes_name, TensorProto.INT64, [1], [axis]))
        squeezed = f"{alias}__squeezed"
        graph.node.append(helper.make_node(
            "Squeeze", [cur, axes_name], [squeezed], name=f"dpdfnet_voice_squeeze_{alias}"))
        print(f"[OK] Squeeze(axis={axis}) : {cur}{dims} -> {squeezed}")
        cur = squeezed
        dims = [d for i, d in enumerate(dims) if i != axis]

    # ---- 2) inv_wnorm (モデル内部の正規化を戻すスカラー) を掛ける ----
    scale = None if args.no_scale else args.scale_const
    if scale is not None:
        inits = {init.name for init in graph.initializer}
        if scale not in inits:
            print(f"ERROR: scale const not found in initializers: {scale} "
                  f"(使わないなら --no-scale)", file=sys.stderr)
            return 1
        graph.node.append(helper.make_node(
            "Mul", [cur, scale], [alias], name=f"dpdfnet_voice_scale_{alias}"))
        print(f"[OK] Mul({scale})          : -> {alias}")
    else:
        graph.node.append(helper.make_node(
            "Identity", [cur], [alias], name=f"dpdfnet_voice_{alias}"))
        print(f"[OK] Identity              : {cur} -> {alias}")

    type_dims = [d if isinstance(d, int) else None for d in dims]
    graph.output.append(helper.make_tensor_value_info(alias, TensorProto.FLOAT, type_dims))
    print(f"[OK] output '{alias}' shape={dims}")

    out_path = args.out or args.model.with_name(args.model.stem + "_2out.onnx")
    onnx.checker.check_model(model)
    onnx.save(model, str(out_path))
    print(f"[OK] wrote {out_path}")
    print("[NEXT] tools/verify_two_output.py で spec_e が不変であることを確認する。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
