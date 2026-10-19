#!/usr/bin/env python3
"""中間モデルの品質を「teacher (dp8) との一致度」で測る + 聴き比べ用 WAV を作る。

指標:
  * spec SNR   : teacher の spec_e を正解とした SNR (dB)。大きいほど dp8 に近い
  * wave SNR   : iSTFT 後の波形で same
  * dp2 を同時に測るので「dp2 は dp8 から何 dB 離れているか」が基準になる

usage:
  python tools/dpdfnet_mid/evaluate_mid.py \
      --student dp4,4,build/dpdfnet_train/ckpt_mid/init_dp4.pth \
      --student dp2,2,build/dpdfnet_train/hf/checkpoints/dpdfnet2_48khz_hr.pth \
      --n-eval 4 --out-dir build/dpdfnet_train/listen
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
import torch

from mid_model import (DEFAULT_UPSTREAM, PLUGIN, SpecModel, add_offline_paths, build_offline,
                       complex_snr_db, istft_wave, load_dp8_into_mid, snr_db, stft_spec)

SR = 48000


# ---------------------------------------------------------------------------
# 評価用のノイズ入り音声を作る (学習とは別 seed)
# ---------------------------------------------------------------------------
def load_speech_pool(root: Path) -> tuple[np.ndarray, list[int]]:
    meta_path = root / f"speech_{SR}.json"
    if not meta_path.is_file():
        print(f"[warn] {meta_path.name} が無い (prepare_data.py 未実行) -> test_signals だけで評価する")
        return np.zeros(0, np.float32), []
    meta = json.loads(meta_path.read_text(encoding="utf-8"))
    raw_path = root / meta["raw_file"]
    if not raw_path.is_file():
        return np.zeros(0, np.float32), []
    raw = np.memmap(raw_path, dtype=np.float32, mode="r")
    return raw, meta["lengths"]


def make_noise(kind: str, n: int, rng: np.random.Generator, pool, lengths) -> np.ndarray:
    if kind == "white":
        return rng.standard_normal(n).astype(np.float32)
    if kind == "pink":
        w = rng.standard_normal(n).astype(np.float32)
        X = np.fft.rfft(w)
        f = np.arange(len(X)) + 1.0
        X /= f ** 0.5
        return np.fft.irfft(X, n=n).astype(np.float32)
    if kind == "hum":
        t = np.arange(n) / SR
        y = 0.5 * np.sin(2 * np.pi * 50 * t) + 0.25 * np.sin(2 * np.pi * 100 * t)
        for k in (3, 5, 7):
            y += 0.1 * np.sin(2 * np.pi * 50 * k * t) * rng.uniform(0.5, 1.5)
        return (y + 0.05 * rng.standard_normal(n)).astype(np.float32)
    if kind == "babble":
        y = np.zeros(n, np.float32)
        for _ in range(rng.integers(3, 6)):
            i = int(rng.integers(0, len(lengths)))
            off = int(sum(lengths[:i]))
            L = lengths[i]
            take = min(n, L)
            s = np.asarray(pool[off:off + take], np.float32)
            y[:take] += s * rng.uniform(0.5, 1.0)
        return y
    raise ValueError(kind)


def eval_signals(root: Path, n_eval: int, seconds: float, seed: int):
    """(名前, ノイズ入り波形) のリスト。teacher が dp8、入力はノイズ入り音声。"""
    rng = np.random.default_rng(seed)
    pool, lengths = load_speech_pool(root)
    n = int(seconds * SR)
    out = []

    if len(lengths) > 0:
        for i in range(n_eval):
            idx = int(rng.integers(0, len(lengths)))
            off = int(sum(lengths[:idx]))
            take = min(n, lengths[idx])
            clean = np.asarray(pool[off:off + take], np.float32)
            if take < n:
                clean = np.pad(clean, (0, n - take))
            kind = ["white", "pink", "hum", "babble"][i % 4]
            noise = make_noise(kind, n, rng, pool, lengths)
            snr = rng.uniform(0.0, 15.0)
            g = np.sqrt(np.mean(clean ** 2)) / (np.sqrt(np.mean(noise ** 2)) + 1e-12)
            g *= 10 ** (-snr / 20.0)
            out.append((f"libri_{i}_{kind}_snr{snr:.0f}", (clean + g * noise).astype(np.float32)))

    # 手元の実音声も入れる
    wavs = [PLUGIN / "test_signals" / "speech_with_noise.wav",
            PLUGIN / "test_signals" / "speech_tail_reverb_48k.wav"]
    try:
        from check_onnx_model import naive_resample, read_wav

        for w in wavs:
            if w.is_file():
                x, sr = read_wav(w)
                x = naive_resample(x, sr, SR)[: n]
                if len(x) == n:
                    out.append((w.stem, x.astype(np.float32)))
    except Exception:
        pass
    return out


def spec_of(model: SpecModel, x: np.ndarray, device) -> np.ndarray:
    with torch.no_grad():
        t = torch.from_numpy(x)[None, :].to(device)
        spec_e, _ = model(t)
    se = spec_e
    while se.dim() > 3:          # [B, 1, T, F, 2] -> [T, F, 2]
        se = se[0]
    return se.cpu().numpy()      # [T, F, 2]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--upstream", type=Path, default=DEFAULT_UPSTREAM)
    ap.add_argument("--root", type=Path, default=PLUGIN / "build" / "dpdfnet_train")
    ap.add_argument("--teacher", type=Path,
                    default=PLUGIN / "build" / "dpdfnet_train" / "hf" / "checkpoints"
                    / "dpdfnet8_48khz_hr.pth")
    ap.add_argument("--student", action="append", default=[],
                    help="name,blocks,pth  (複数指定可)")
    ap.add_argument("--n-eval", type=int, default=4)
    ap.add_argument("--seconds", type=float, default=6.0)
    ap.add_argument("--out-dir", type=Path, default=None, help="聴き比べ WAV の出力先")
    args = ap.parse_args()

    add_offline_paths(args.upstream)
    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")

    teacher = SpecModel(build_offline(8, device))
    sd = torch.load(str(args.teacher), map_location="cpu", weights_only=True)
    teacher.base.load_state_dict(sd, strict=True)
    teacher.eval()
    print(f"[teacher] dp8 ({args.teacher.name})")

    students = []
    for spec in args.student:
        name, blocks, pth = spec.split(",", 2)
        m = SpecModel(build_offline(int(blocks), device))
        load_dp8_into_mid(m.base, Path(pth), verbose=False)
        m.eval()
        students.append((name, m))
        print(f"[student] {name}: DPRNN {blocks} ブロック ({pth})")

    signals = eval_signals(args.root, args.n_eval, args.seconds, seed=20261019)
    print(f"[eval] {len(signals)} 本 x {args.seconds:.1f} s")

    rows: list[dict] = []
    wave_out: dict[str, list[np.ndarray]] = {}
    for name, x in signals:
        ref = spec_of(teacher, x, device)
        ref_w = istft_wave(ref)
        row = {"signal": name}
        for sname, m in students:
            est = spec_of(m, x, device)
            est_w = istft_wave(est)
            row[sname + "_spec_snr"] = complex_snr_db(ref, est)
            row[sname + "_wave_snr"] = snr_db(ref_w, est_w)
            if args.out_dir is not None:
                wave_out.setdefault(sname, []).append(est_w)
        rows.append(row)

    # 集計
    names = [s[0] for s in students]
    print("\n[結果] teacher (dp8) を正解とした SNR (dB)")
    header = f"{'signal':28s}" + "".join(f"{n+'_spec':>12s}{n+'_wave':>12s}" for n in names)
    print(header)
    for row in rows:
        line = f"{row['signal']:28s}"
        for n in names:
            line += f"{row[n+'_spec_snr']:12.2f}{row[n+'_wave_snr']:12.2f}"
        print(line)
    print(f"{'平均':28s}" + "".join(
        f"{np.mean([r[n+'_spec_snr'] for r in rows]):12.2f}"
        f"{np.mean([r[n+'_wave_snr'] for r in rows]):12.2f}" for n in names))

    out_path = None
    if args.out_dir is not None:
        import wave as wavelib

        out_dir = Path(args.out_dir)
        out_dir.mkdir(parents=True, exist_ok=True)
        for n in names:
            y = np.concatenate(wave_out[n])
            y = np.clip(y, -1.0, 1.0)
            pcm = (y * 32767.0).astype("<i2")
            p = out_dir / f"{n}.wav"
            with wavelib.open(str(p), "wb") as w:
                w.setnchannels(1)
                w.setsampwidth(2)
                w.setframerate(SR)
                w.writeframes(pcm.tobytes())
            print(f"[wav] {p}")
        out_path = str(out_dir)
    if out_path:
        (Path(out_path) / "metrics.json").write_text(
            json.dumps(rows, ensure_ascii=False, indent=2), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
