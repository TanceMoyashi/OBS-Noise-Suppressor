#!/usr/bin/env python3
"""dp2 / dp4 / dp8 を「学習に使っていない実ノイズ」で知覚指標も含めて比べる。

データ (どちらも学習に未使用):
  * クリーン音声: LibriSpeech **test-clean** (`test_speech_48000.*`)
    - 学習に使ったのは dev-clean だけ。test-clean は一度も学習に入れていない。
  * ノイズ: MUSAN の **eval split** (`noise_48000.json` の `eval_indices`、
    ファイル名の sha1 で決まる 20 %)。学習は train split だけを使う。

指標:
  * SI-SDR (dB)      … 尺度不変の SDR。高いほどよい。
  * STOI (0..1)      … 明瞭度。高いほどよい。
  * PESQ-WB (1..4.5) … 音質。高いほどよい (16 kHz に落として測定)。
  * segSNR (dB)      … 32 ms ごとの SNR (active frame のみ、[-10, 35] にクリップ)。
  * dp8 との一致 (dB) … 波形がどれだけ dp8 と同じか (参考)。

usage:
  python tools/dpdfnet_mid/perceptual_report.py \
      --models dp2=models/dpdfnet2_48khz_hr_2out.onnx \
               dp4=models/dpdfnet4_48khz_hr_2out.onnx \
               dp8=models/dpdfnet8_48khz_hr_2out.onnx \
      --utts 4 --snr -5 0 5 10 --write-wav
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np
from scipy.signal import resample_poly

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_onnx_model import MODEL_FRAME_DELAY, istft_frames, stft_frames  # noqa: E402
from quality_report import run_stream, session  # noqa: E402

SR = 48000


# --------------------------------------------------------------------------- #
# データ
# --------------------------------------------------------------------------- #
def load_speech(root: Path, n: int, seconds: float) -> list[tuple[int, np.ndarray]]:
    meta = json.loads((root / f"test_speech_{SR}.json").read_text(encoding="utf-8"))
    raw = np.memmap(root / meta["raw_file"], dtype=np.float32, mode="r")
    lengths = np.asarray(meta["lengths"], np.int64)
    offs = np.concatenate([[0], np.cumsum(lengths)])
    need = int(seconds * SR)
    out = []
    for i in np.argsort(-lengths):
        if lengths[i] >= need:
            out.append((int(i), np.asarray(raw[offs[i]:offs[i] + need], np.float32)))
        if len(out) >= n:
            break
    print(f"[data] test-clean {len(out)} 発話 x {seconds:.0f} s")
    return out


def load_noise(root: Path) -> tuple[np.memmap, np.ndarray, np.ndarray, list[str]]:
    meta = json.loads((root / f"noise_{SR}.json").read_text(encoding="utf-8"))
    raw = np.memmap(root / meta["raw_file"], dtype=np.float32, mode="r")
    lengths = np.asarray(meta["lengths"], np.int64)
    offs = np.concatenate([[0], np.cumsum(lengths)])
    ev = [int(i) for i in meta["eval_indices"]]
    names = [meta["names"][i] for i in ev]
    print(f"[data] MUSAN eval split {len(ev)} files / "
          f"{lengths[ev].sum()/SR/60:.1f} min (学習には未使用)")
    return raw, offs[ev], lengths[ev], names


def noise_window(raw, starts, lengths, k, n, rng) -> np.ndarray:
    s, L = int(starts[k]), int(lengths[k])
    if L <= n:
        x = np.asarray(raw[s:s + L], np.float32)
        x = np.pad(x, (0, n - L))
    else:
        o = int(rng.integers(0, L - n))
        x = np.asarray(raw[s + o:s + o + n], np.float32)
    x = x - float(x.mean())
    return (x / (np.sqrt(np.mean(x ** 2)) + 1e-12)).astype(np.float32)


# --------------------------------------------------------------------------- #
# 指標
# --------------------------------------------------------------------------- #
def si_sdr(ref: np.ndarray, est: np.ndarray) -> float:
    n = min(len(ref), len(est))
    r = ref[:n].astype(np.float64)
    e = est[:n].astype(np.float64)
    r = r - r.mean()
    e = e - e.mean()
    a = np.dot(e, r) / (np.dot(r, r) + 1e-30)
    t = a * r
    err = e - t
    return float(10 * np.log10((np.sum(t ** 2) + 1e-30) / (np.sum(err ** 2) + 1e-30)))


def seg_snr(ref: np.ndarray, est: np.ndarray, win: int = 1536, hop: int = 768,
            lo: float = -10.0, hi: float = 35.0) -> float:
    """32 ms 窓の SNR (active frame のみ)。"""
    n = min(len(ref), len(est))
    vals = []
    for i in range(0, n - win, hop):
        r = ref[i:i + win].astype(np.float64)
        e = est[i:i + win].astype(np.float64)
        if np.sum(r ** 2) < 1e-8 * win:      # 無音フレームは除く
            continue
        v = 10 * np.log10((np.sum(r ** 2) + 1e-30) / (np.sum((r - e) ** 2) + 1e-30))
        vals.append(min(hi, max(lo, v)))
    return float(np.mean(vals)) if vals else float("nan")


def stoi_16k(ref: np.ndarray, est: np.ndarray) -> float:
    from pystoi import stoi
    n = min(len(ref), len(est))
    r = resample_poly(ref[:n].astype(np.float32), 1, 3)
    e = resample_poly(est[:n].astype(np.float32), 1, 3)
    return float(stoi(r, e, 16000, extended=False))


def pesq_16k(ref: np.ndarray, est: np.ndarray) -> float:
    from pesq import pesq
    n = min(len(ref), len(est))
    r = resample_poly(ref[:n].astype(np.float32), 1, 3)
    e = resample_poly(est[:n].astype(np.float32), 1, 3)
    try:
        return float(pesq(16000, r, e, "wb"))
    except Exception as ex:                  # 無音などで失敗したら NaN
        print(f"       [warn] pesq: {ex}")
        return float("nan")


def snr_db(ref: np.ndarray, est: np.ndarray) -> float:
    n = min(len(ref), len(est))
    r = ref[:n].astype(np.float64)
    e = est[:n].astype(np.float64)
    return float(10 * np.log10((np.sum(r ** 2) + 1e-30) / (np.sum((r - e) ** 2) + 1e-30)))


# --------------------------------------------------------------------------- #
def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="+", required=True, help="name=path")
    ap.add_argument("--root", type=Path, default=Path("build/dpdfnet_train"))
    ap.add_argument("--utts", type=int, default=4)
    ap.add_argument("--seconds", type=float, default=8.0)
    ap.add_argument("--snr", type=float, nargs="+", default=[-5.0, 0.0, 5.0, 10.0])
    ap.add_argument("--out-dir", type=Path, default=Path("build/dpdfnet_train/listen_real"))
    ap.add_argument("--write-wav", action="store_true")
    ap.add_argument("--seed", type=int, default=7)
    args = ap.parse_args()

    models = []
    for spec in args.models:
        name, path = spec.split("=", 1)
        models.append((name, session(Path(path))))
        print(f"[model] {name}: {path}")
    names = [m[0] for m in models]

    utts = load_speech(args.root, args.utts, args.seconds)
    raw, nstarts, nlengths, nnames = load_noise(args.root)

    rng = np.random.default_rng(args.seed)
    rows = []
    for u_i, clean in utts:
        for target in args.snr:
            k = int(rng.integers(0, len(nstarts)))
            nz = noise_window(raw, nstarts, nlengths, k, len(clean), rng)
            g = np.sqrt(np.mean(clean ** 2)) / (np.sqrt(np.mean(nz ** 2)) + 1e-12)
            g *= 10 ** (-target / 20.0)
            noisy = (clean + g * nz).astype(np.float32)

            pad = np.zeros(4800, np.float32)
            spec_in = stft_frames(np.concatenate([pad, noisy]), 960, 480)
            outs = {}
            for name, sess in models:
                y = istft_frames(run_stream(sess, spec_in), 960, 480)
                y = y[MODEL_FRAME_DELAY * 480:]
                outs[name] = y[len(pad):len(pad) + len(clean)]

            ref8 = outs.get("dp8")
            row = {"utt": u_i, "noise": Path(nnames[k]).name, "snr": target,
                   "in_snr": snr_db(clean, noisy), "m": {}}
            for name in names:
                o = outs[name]
                row["m"][name] = {
                    "si_sdr": si_sdr(clean, o),
                    "stoi": stoi_16k(clean, o),
                    "pesq": pesq_16k(clean, o),
                    "seg_snr": seg_snr(clean, o),
                    "snr": snr_db(clean, o),
                    "agree": snr_db(ref8, o) if ref8 is not None else float("nan"),
                }
            rows.append(row)
            m = row["m"]
            print(f"  utt{u_i}/+{target:+.0f}dB/{Path(nnames[k]).stem[:18]:18s} "
                  + "  ".join(f"{n}: SI-SDR {m[n]['si_sdr']:6.2f} STOI {m[n]['stoi']:.3f} "
                              f"PESQ {m[n]['pesq']:4.2f}" for n in names), flush=True)

            if args.write_wav:
                import wave as wavelib

                def save(p: Path, y: np.ndarray) -> None:
                    y = np.clip(y, -1, 1)
                    with wavelib.open(str(p), "wb") as f:
                        f.setnchannels(1)
                        f.setsampwidth(2)
                        f.setframerate(SR)
                        f.writeframes((y * 32767).astype("<i2").tobytes())

                d = args.out_dir
                d.mkdir(parents=True, exist_ok=True)
                tag = f"u{u_i}_{target:+.0f}dB"
                save(d / f"{tag}_1_clean.wav", clean)
                save(d / f"{tag}_2_noisy.wav", noisy)
                for j, name in enumerate(names):
                    save(d / f"{tag}_{j + 3}_{name}.wav", outs[name])

    # 集計
    print("\n[平均] " + f"{len(rows)} 条件 (test-clean 発話 x MUSAN eval ノイズ)")
    hdr = f"{'モデル':8s}" + "".join(f"{k:>10s}" for k in
                                     ["SI-SDR", "STOI", "PESQ", "segSNR", "SNR", "dp8一致"])
    print(hdr)
    print("-" * len(hdr))
    summary = {}
    for name in names:
        vals = {k: np.nanmean([r["m"][name][k] for r in rows])
                for k in ["si_sdr", "stoi", "pesq", "seg_snr", "snr", "agree"]}
        summary[name] = vals
        print(f"{name:8s}" + f"{vals['si_sdr']:10.2f}{vals['stoi']:10.3f}{vals['pesq']:10.2f}"
              f"{vals['seg_snr']:10.2f}{vals['snr']:10.2f}{vals['agree']:10.2f}")

    # SNR ごとの内訳 (dp2 vs dp4 の差が条件で偏っていないか)
    print("\n[SNR 別 SI-SDR]")
    print(f"{'SNR':>6s}" + "".join(f"{n:>10s}" for n in names))
    for target in args.snr:
        vals = [r for r in rows if r["snr"] == target]
        print(f"{target:+6.0f}" + "".join(
            f"{np.nanmean([r['m'][n]['si_sdr'] for r in vals]):10.2f}" for n in names))

    out = Path(args.root) / "perceptual_report.json"
    out.write_text(json.dumps({"rows": [{k: v for k, v in r.items() if k != 'm'}
                                         | {"models": r["m"]} for r in rows],
                               "summary": summary}, ensure_ascii=False, indent=2),
                   encoding="utf-8")
    print(f"\n[ok  ] {out}")
    if args.write_wav:
        print(f"[ok  ] 比較 WAV: {args.out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
