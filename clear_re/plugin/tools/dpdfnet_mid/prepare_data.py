#!/usr/bin/env python3
"""中間モデル (dp4) の学習データを用意する。

やること:
  1. LibriSpeech dev-clean (OpenSLR SLR12, CC BY 4.0) をダウンロード
  2. 48 kHz / mono / float32 にデコードして 1 本の raw ファイルに連結
     (学習時は memmap で読み、ランダム窓を切り出す)
  3. ライセンス情報をファイルに残す

ノイズはここでは用意しない。学習側 (distill.py) が合成する
(白色 / ピンク / 低周波ハム / バブル / クリック)。実ノイズ (MUSAN, DNS) は
大きいので今回は入れていない。理由と追加手順は MODEL_TRAINING.md を参照。

usage:
  python tools/dpdfnet_mid/prepare_data.py --root build/dpdfnet_train --hours 2.0
"""

from __future__ import annotations

import argparse
import json
import tarfile
import urllib.request
from pathlib import Path

import numpy as np
import soundfile as sf
from scipy.signal import resample_poly

URL = "https://www.openslr.org/resources/12/dev-clean.tar.gz"


def download(url: str, dst: Path) -> None:
    if dst.is_file() and dst.stat().st_size > 300_000_000:
        print(f"[skip] すでに取得済み: {dst} ({dst.stat().st_size} bytes)")
        return
    print(f"[get ] {url} -> {dst}")
    tmp = dst.with_suffix(dst.suffix + ".part")
    with urllib.request.urlopen(url) as r, open(tmp, "wb") as f:
        total = int(r.headers.get("Content-Length", 0))
        got = 0
        while True:
            chunk = r.read(1 << 20)
            if not chunk:
                break
            f.write(chunk)
            got += len(chunk)
            if total and got % (20 << 20) < (1 << 20):
                print(f"       {got/1e6:.0f}/{total/1e6:.0f} MB", flush=True)
    tmp.replace(dst)
    print(f"[done] {dst} ({dst.stat().st_size} bytes)")


def extract(tar: Path, out_dir: Path) -> Path:
    root = out_dir / "LibriSpeech" / "dev-clean"
    if root.is_dir():
        print(f"[skip] 展開済み: {root}")
        return root
    print(f"[tar ] {tar} -> {out_dir}")
    with tarfile.open(tar, "r:gz") as t:
        t.extractall(out_dir)
    return root


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", type=Path, default=Path("build/dpdfnet_train"))
    ap.add_argument("--hours", type=float, default=2.0,
                    help="キャッシュする音声の上限時間 (dev-clean 全体は約 5.4 時間)")
    ap.add_argument("--sr", type=int, default=48000)
    args = ap.parse_args()

    root = args.root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    tar = root / "dev-clean.tar.gz"
    download(URL, tar)
    base = extract(tar, root)

    flacs = sorted(base.rglob("*.flac"))
    print(f"[scan] {len(flacs)} utterances")
    if not flacs:
        raise SystemExit("ERROR: flac が見つからない")

    raw_path = root / f"speech_{args.sr}.f32"
    meta_path = root / f"speech_{args.sr}.json"
    limit = int(args.hours * 3600 * args.sr)
    total = 0
    lengths: list[int] = []
    speakers: list[str] = []
    with open(raw_path, "wb") as f:
        for i, p in enumerate(flacs):
            x, sr = sf.read(str(p), dtype="float32", always_2d=False)
            if x.ndim > 1:
                x = x.mean(axis=1)
            if sr != args.sr:
                if args.sr % sr == 0:
                    x = resample_poly(x, args.sr // sr, 1).astype(np.float32)
                else:
                    import librosa
                    x = librosa.resample(x, orig_sr=sr, target_sr=args.sr).astype(np.float32)
            # 直流除去とピーク正規化 (データ全体のレベルは学習側で揺らす)
            x = x - x.mean()
            peak = float(np.max(np.abs(x)) or 1.0)
            x = (x / peak * 0.9).astype(np.float32)
            f.write(x.tobytes())
            lengths.append(len(x))
            speakers.append(p.parent.name)
            total += len(x)
            if i % 200 == 0:
                print(f"       {i+1}/{len(flacs)}  {total/args.sr/3600:.2f} h", flush=True)
            if total >= limit:
                break

    meta = {
        "source": URL,
        "dataset": "LibriSpeech dev-clean (OpenSLR SLR12)",
        "license": "CC BY 4.0 (LibriSpeech, Panayotov et al. 2015)",
        "sample_rate": args.sr,
        "utterances": len(lengths),
        "seconds": total / args.sr,
        "lengths": lengths,
        "speakers": speakers,
        "raw_file": raw_path.name,
    }
    meta_path.write_text(json.dumps(meta, ensure_ascii=False), encoding="utf-8")
    print(f"[ok  ] {raw_path.name}: {len(lengths)} utterances, {total/args.sr/3600:.2f} h, "
          f"{raw_path.stat().st_size/1e9:.2f} GB")
    print(f"[ok  ] {meta_path.name} に index を保存")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
