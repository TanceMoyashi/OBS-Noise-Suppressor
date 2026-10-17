#!/usr/bin/env python3
"""ONNX グラフの中間テンソルを「誰が作ったか / 誰が使うか」でたどる調査ツール。

DPDFNet のストリーミング出力 spec_e がどのように作られるかを追い、
「マスク適用のみ (DF 経路に入る前)」のテンソルを特定するために使う。

usage:
  python tools/inspect_graph.py --model models/dpdfnet2_48khz_hr.onnx
  python tools/inspect_graph.py --model models/dpdfnet2_48khz_hr.onnx --consumers-of spec_e
  python tools/inspect_graph.py --model models/dpdfnet2_48khz_hr.onnx --producer-of spec_e --depth 6
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import onnx


def shapes_of(graph: onnx.GraphProto) -> dict[str, list]:
    out: dict[str, list] = {}
    for vi in list(graph.input) + list(graph.output) + list(graph.value_info):
        t = vi.type.tensor_type
        if not t.HasField("shape"):
            continue
        dims: list = []
        for d in t.shape.dim:
            dims.append(d.dim_value if d.HasField("dim_value") else d.dim_param or "?")
        out[vi.name] = dims
    return out


def fmt(name: str, shapes: dict[str, list]) -> str:
    s = shapes.get(name)
    return f"{name}{s if s else ''}"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True, type=Path)
    ap.add_argument("--producer-of", default=None, help="このテンソルを作るノードから遡って表示")
    ap.add_argument("--consumers-of", default=None, help="このテンソルを使うノードを表示")
    ap.add_argument("--depth", type=int, default=4)
    ap.add_argument("--node-of", default=None,
                    help="このテンソルを作るノードの詳細 (inputs/attributes) を表示")
    ap.add_argument("--tensor-info", default=None,
                    help="このテンソルの shape/dtype を input/output/value_info/initializer から表示")
    ap.add_argument("--only-shape", default=None,
                    help="rank 4 でこの値のテンソルを作るノードだけ列挙 (例: 2)")
    args = ap.parse_args()

    model = onnx.load(str(args.model))
    graph = model.graph
    shapes = shapes_of(graph)

    if args.tensor_info:
        t = args.tensor_info
        print(f"[tensor-info] {t}")
        hit = False
        for vi in list(graph.input) + list(graph.output) + list(graph.value_info):
            if vi.name == t:
                print(f"  vi     : {vi.type}")
                hit = True
        for init in graph.initializer:
            if init.name == t:
                import numpy as np
                arr = onnx.numpy_helper.to_array(init)
                flat = arr.reshape(-1)
                print(f"  init   : dtype={arr.dtype} shape={list(arr.shape)} "
                      f"min={flat.min():.6g} max={flat.max():.6g}")
                if flat.size <= 12:
                    print(f"           values={flat.tolist()}")
                hit = True
        for n in graph.node:
            for o in (*n.output,):
                if o == t:
                    print(f"  producer: {n.op_type} inputs={list(n.input)}")
                    hit = True
        if not hit:
            print("  (見つからない)")
        return 0

    if args.node_of:
        t = args.node_of
        for n in graph.node:
            if t in n.output:
                print(f"[node-of {t}]")
                print(f"  op_type = {n.op_type}")
                print(f"  name    = {n.name}")
                print(f"  inputs  = {list(n.input)}")
                print(f"  outputs = {list(n.output)}")
                for a in n.attribute:
                    print(f"  attr    = {a.name}")
                    print(f"            {onnx.helper.printable_attribute(a)}")
                break
        else:
            print(f"[node-of {t}] (見つからない)")
        print(f"[opset] {[(o.domain or 'ai.onnx', o.version) for o in model.opset_import]}")
        return 0

    producer = {}
    consumers: dict[str, list[tuple[str, str]]] = {}
    for n in graph.node:
        for o in n.output:
            producer[o] = n
        for i in n.input:
            consumers.setdefault(i, []).append((n.op_type, n.name))

    if args.only_shape is not None:
        want = int(args.only_shape)
        print(f"[rank4, 末尾={want}] のテンソルを作るノード:")
        for n in graph.node:
            for o in n.output:
                d = shapes.get(o)
                if d and len(d) == 4 and d[-1] == want:
                    print(f"  {n.op_type:8s} out={fmt(o, shapes)}")
                    for i in n.input:
                        print(f"      in  {fmt(i, shapes)}")
                    break
        return 0

    if args.consumers_of:
        t = args.consumers_of
        print(f"[consumers of {fmt(t, shapes)}]")
        for op, name in consumers.get(t, []):
            print(f"  {op:8s} {name}")
        if not consumers.get(t):
            print("  (graph output か未使用)")
        return 0

    if args.producer_of:
        t = args.producer_of
        seen = set()

        def walk(name: str, depth: int, indent: str) -> None:
            if depth < 0 or name in seen:
                return
            seen.add(name)
            n = producer.get(name)
            if n is None:
                kind = "graph input" if name in {i.name for i in graph.input} else "constant"
                print(f"{indent}{fmt(name, shapes)}   <- {kind}")
                return
            print(f"{indent}{n.op_type}  ->  {fmt(name, shapes)}")
            for i in n.input:
                walk(i, depth - 1, indent + "  ")

        walk(t, args.depth, "")
        return 0

    print(f"[graph] nodes={len(graph.node)}  inputs={[i.name for i in graph.input]}")
    print(f"[graph] outputs={[o.name for o in graph.output]}")
    from collections import Counter
    c = Counter(n.op_type for n in graph.node)
    print("[graph] op histogram: " + ", ".join(f"{k}={v}" for k, v in c.most_common()))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
