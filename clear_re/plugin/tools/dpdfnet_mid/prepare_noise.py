#!/usr/bin/env python3
"""実ノイズ (MUSAN の noise 部分) を用意する。

やること:
  1. (--download) Hugging Face のミラー `FluidInference/musan` から `noise/**` だけ取得
     元コーパスは OpenSLR SLR17 (MUSAN, CC BY 4.0)。noise 部分は 932 ファイル / 約 6 時間。
  2. 16 kHz の wav を 48 kHz / mono / float32 に変換して 1 本の raw に連結
  3. 学習用と評価用に **ファイル単位で** 分割する (評価用は学習に一切使わない)
     - 分割はファイル名の sha1 で決める (再現可能)
  4. ライセンス・出典・分割を `noise_48000.json` に残す

なぜ MUSAN か:
  * ライセンスが CC BY 4.0 で、プラグイン (Apache-2.0) に同梱・派生しやすい。
  * ESC-50 は CC BY-NC 3.0 (非商用) なので、製品に載せる可能性がある今回は避けた。
  * ダウンロードは noise 部分だけ 585 MB で済む (全体は 11 GB)。

usage:
  python tools/dpdfnet_mid/prepare_noise.py --root build/dpdfnet_train --download
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
import soundfile as sf
from scipy.signal import resample_poly

SR = 48000
REPO = "FluidInference/musan"
EVAL_RATIO = 0.2


def download(root: Path) -> Path:
    local = root / "musan"
    try:
        from huggingface_hub import snapshot_download
    except ImportError:
        raise SystemExit("ERROR: huggingface_hub が無い。pip install huggingface_hub")
    print(f"[get ] {REPO} の noise/** を取得 (約 585 MB)")
    p = snapshot_download(repo_id=REPO, repo_type="dataset",
                          allow_patterns=["noise/**"], local_dir=str(local))
    print(f"[done] {p}")
    return Path(p)


def is_eval(name: str) -> bool:
    """ファイル名から決まる評価用フラグ (再現可能)。"""
    h = hashlib.sha1(name.encode("utf-8")).digest()
    return (h[0] / 255.0) < EVAL_RATIO


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", type=Path, default=Path("build/dpdfnet_train"))
    ap.add_argument("--download", action="store_true")
    ap.add_argument("--sr", type=int, default=SR)
    ap.add_argument("--hours", type=float, default=6.5, help="取り込む上限時間")
    args = ap.parse_args()

    root = args.root.resolve()
    if args.download:
        download(root)
    base = root / "musan" / "noise"
    if not base.is_dir():
        raise SystemExit(f"ERROR: {base} が無い (--download を付けるか手動で置く)")

    wavs = sorted(p for p in base.rglob("*.wav"))
    if not wavs:
        wavs = sorted(p for p in base.rglob("*.WAV"))
    print(f"[scan] {len(wavs)} files")

    raw_path = root / f"noise_{args.sr}.f32"
    meta_path = root / f"noise_{args.sr}.json"
    limit = int(args.hours * 3600 * args.sr)
    total = 0
    lengths: list[int] = []
    names: list[str] = []
    evals: list[int] = []
    with open(raw_path, "wb") as f:
        for i, p in enumerate(wavs):
            try:
                x, sr = sf.read(str(p), dtype="float32", always_2d=False)
            except Exception as e:      # 壊れたファイルは飛ばす
                print(f"[warn] skip {p.name}: {e}")
                continue
            if x.ndim > 1:
                x = x.mean(axis=1)
            if len(x) < sr // 2:        # 0.5 秒未満は使わない
                continue
            if sr != args.sr:
                if args.sr % sr == 0:
                    x = resample_poly(x, args.sr // sr, 1).astype(np.float32)
                else:
                    import librosa
                    x = librosa.resample(x, orig_sr=sr, target_sr=args.sr).astype(np.float32)
            x = x - float(x.mean())
            if float(np.sqrt(np.mean(x ** 2))) < 1e-5:
                continue                # 実質無音
            f.write(x.astype(np.float32).tobytes())
            idx = len(lengths)
            lengths.append(len(x))
            rel = str(p.relative_to(base)).replace("\\", "/")
            names.append(rel)
            if is_eval(rel):
                evals.append(idx)
            total += len(x)
            if i % 100 == 0:
                print(f"       {i+1}/{len(wavs)}  {total/args.sr/3600:.2f} h", flush=True)
            if total >= limit:
                break

    n_eval = sum(lengths[i] for i in evals)
    meta = {
        "source": f"https://huggingface.co/datasets/{REPO} (mirror of OpenSLR SLR17 musan.tar.gz)",
        "dataset": "MUSAN noise subset",
        "license": "CC BY 4.0 (MUSAN, Snyder et al. 2015, https://www.openslr.org/17/). "
                   "Individual clips carry their own attribution in noise/free-sound/ and "
                   "noise/sound-bible/ LICENSE files (Attribution 3.0 / Public Domain).",
        "sample_rate": args.sr,
        "files": len(lengths),
        "seconds": total / args.sr,
        "lengths": lengths,
        "names": names,
        "eval_indices": evals,
        "eval_seconds": n_eval / args.sr,
        "eval_ratio": EVAL_RATIO,
        "raw_file": raw_path.name,
    }
    meta_path.write_text(json.dumps(meta, ensure_ascii=False), encoding="utf-8")
    print(f"[ok  ] {raw_path.name}: {len(lengths)} files, {total/args.sr/3600:.2f} h, "
          f"{raw_path.stat().st_size/1e9:.2f} GB")
    print(f"[ok  ] 学習用 {len(lengths)-len(evals)} files / 評価用 {len(evals)} files "
          f"({n_eval/args.sr/60:.1f} min)")
    print(f"[ok  ] {meta_path.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
