#!/usr/bin/env python3
"""OBS 実機のダンプ 2 つを突き合わせる（CPU 実行 vs GPU 実行など）。

`run_obs_verify.ps1` は `-WorkDir` に `dump_in.wav` / `dump_out.wav` を残す。
同じ WAV を同じ設定で流していれば **入力はビット一致**するはずなので、まず
それを確かめ、そのうえで出力を比べる。

  * 入力が `max diff = 0e0` → 2 回の実行は同じ音を処理している
  * 出力の SNR が 55 dB 以上 → 聴感上は同じ（GPU は丸めが違うのでビット一致しない）

usage:
  python tools/compare_dumps.py <A の WorkDir> <B の WorkDir>
  python tools/compare_dumps.py live-cpu live-gpu --segment 5
"""

from __future__ import annotations

import argparse
import struct
from pathlib import Path

import numpy as np


def read_float_wav(path: Path) -> tuple[np.ndarray, int, int]:
    """32-bit float WAV を読んで (インターリーブ, rate, channels) を返す。

    `wave` は float WAV (format 3) を扱えないので RIFF を自前で歩く。OBS を
    強制終了すると data チャンクのサイズが 0 のまま残ることがあるため、
    サイズは信用せず実ファイル長も見る。
    """
    raw = path.read_bytes()
    if raw[:4] != b"RIFF" or raw[8:12] != b"WAVE":
        raise SystemExit(f"WAV ではない: {path}")

    pos, rate, channels = 12, 0, 0
    while pos + 8 <= len(raw):
        cid = raw[pos : pos + 4]
        size = struct.unpack_from("<I", raw, pos + 4)[0]
        body = raw[pos + 8 : pos + 8 + size]
        if cid == b"fmt ":
            _fmt, channels, rate, _bps, _align, bits = struct.unpack_from("<HHIIHH", body, 0)
            if bits != 32:
                raise SystemExit(f"32-bit float を想定: {bits} bit ({path})")
        elif cid == b"data":
            if size == 0 or pos + 8 + size > len(raw):
                body = raw[pos + 8 :]
            return np.frombuffer(body, "<f4").copy(), int(rate), int(channels)
        pos += 8 + size + (size & 1)
    raise SystemExit(f"data チャンクが無い: {path}")


def snr_db(ref: np.ndarray, test: np.ndarray) -> float:
    n = min(len(ref), len(test))
    err = float(np.sum((ref[:n] - test[:n]) ** 2))
    if err <= 0.0:
        return float("inf")
    return 10.0 * np.log10(float(np.sum(ref[:n] ** 2)) / err)


def rms(x: np.ndarray) -> float:
    if len(x) == 0:
        return 0.0
    return float(np.sqrt(np.mean(np.square(x, dtype=np.float64))))


def report(tag: str, xa: np.ndarray, xb: np.ndarray, ra: int, rb: int, ca: int, cb: int) -> None:
    n = min(len(xa), len(xb))
    print(
        f"{tag}: A {len(xa)} frames / B {len(xb)} frames "
        f"({ra} Hz {ca} ch / {rb} Hz {cb} ch)"
    )
    print(
        f"     A rms={rms(xa):.6f}  B rms={rms(xb):.6f}  "
        f"max|diff|={float(np.max(np.abs(xa[:n] - xb[:n]))):.3e}  "
        f"snr={snr_db(xa, xb):.1f} dB"
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("a", type=Path, help="A の WorkDir（例: live-cpu）")
    ap.add_argument("b", type=Path, help="B の WorkDir（例: live-gpu）")
    ap.add_argument("--segment", type=int, default=0, help="この秒数ごとの SNR も出す（0 で無効）")
    args = ap.parse_args()

    ia, ra, ca = read_float_wav(args.a / "dump_in.wav")
    ib, rb, cb = read_float_wav(args.b / "dump_in.wav")
    report("in ", ia, ib, ra, rb, ca, cb)
    oa, _, _ = read_float_wav(args.a / "dump_out.wav")
    ob, _, _ = read_float_wav(args.b / "dump_out.wav")
    report("out", oa, ob, ra, rb, ca, cb)

    if args.segment > 0:
        n = min(len(oa), len(ob))
        step = args.segment * ra
        for k in range(0, max(1, n // step)):
            s, e = k * step, min((k + 1) * step, n)
            if e - s < step // 2:
                break
            print(
                f"  {k * args.segment:4d}-{k * args.segment + args.segment:4d} s: "
                f"snr={snr_db(oa[s:e], ob[s:e]):6.1f} dB  "
                f"max|diff|={float(np.max(np.abs(oa[s:e] - ob[s:e]))):.3e}"
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
