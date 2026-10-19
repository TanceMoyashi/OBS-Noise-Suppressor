#!/usr/bin/env python3
"""中間モデル (dp4) を dp8 からの蒸留で微調整する。

方針:
  * 生徒 = DPRNN 4 ブロック (dp8 の先頭 4 ブロックを初期値に使う)
  * 教師 = DPDFNet dp8 (同じオフライン実装)
  * 損失 = 教師の `spec_e` との一致 (振幅のべき乗圧縮 MSE + 複素 MSE)
    -> プラグインが出すのは結局 `spec_e` なので、そこを直接合わせる。

データ:
  * prepare_data.py が作った LibriSpeech (48 kHz) の memmap からランダム窓
  * ノイズは合成 (白色 / ピンク / ハム / バブル / 非定常)。実ノイズは使っていない理由を
    MODEL_TRAINING.md に記載。
  * レベルは test_signals/speech_tail_48k.wav の RMS に合わせる (モデルの正規化は
    入力レベルの想定があるため、学習と評価で同じ土俵に乗せる)。

45 分以内で区切って再開できるようにしてある:
  --minutes で制限を掛け、--out に定期保存。同じ引数で再実行すると続きから走る。

usage:
  python tools/dpdfnet_mid/distill.py --init build/dpdfnet_train/ckpt_mid/init_dp4.pth \
      --out build/dpdfnet_train/ckpt_mid/dp4.pth --minutes 40
"""

from __future__ import annotations

import argparse
import json
import time
from pathlib import Path

import numpy as np
import torch
import torch.nn.functional as F

from mid_model import (DEFAULT_UPSTREAM, PLUGIN, SpecModel, add_offline_paths, build_offline,
                       complex_snr_db, load_dp8_into_mid, snr_db)

SR = 48000


# --------------------------------------------------------------------------- #
# データ
# --------------------------------------------------------------------------- #
class SpeechPool:
    """prepare_data.py が作った raw float32 + index を memmap で読む。"""

    def __init__(self, root: Path):
        meta_path = root / f"speech_{SR}.json"
        if not meta_path.is_file():
            raise SystemExit(
                f"ERROR: {meta_path} が無い。先に tools/dpdfnet_mid/prepare_data.py を実行すること"
            )
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        self.raw = np.memmap(root / meta["raw_file"], dtype=np.float32, mode="r")
        self.lengths = np.asarray(meta["lengths"], dtype=np.int64)
        self.offsets = np.concatenate([[0], np.cumsum(self.lengths)])
        self.seconds = float(meta["seconds"])
        print(f"[data] {len(self.lengths)} 発話 / {self.seconds/3600:.2f} h "
              f"({meta['dataset']}, {meta['license']})")

    def get(self, i: int) -> np.ndarray:
        return np.asarray(self.raw[self.offsets[i]:self.offsets[i] + self.lengths[i]], np.float32)

    def any_longer(self, n: int) -> np.ndarray:
        idx = np.where(self.lengths >= n)[0]
        if len(idx) == 0:
            idx = np.arange(len(self.lengths))
        return idx


def synth_rir(rt60: float, n: int, rng: np.random.Generator) -> np.ndarray:
    ln = int(rt60 * SR)
    h = rng.standard_normal(ln) * np.exp(-6.9 * np.arange(ln) / ln)
    h[0] = 1.0
    h /= np.sqrt(np.sum(h ** 2))
    out = np.zeros(n, np.float32)
    take = min(n, ln)
    out[:take] = h[:take]
    return out


def make_noise(kind: str, n: int, rng: np.random.Generator, pool: SpeechPool) -> np.ndarray:
    if kind == "white":
        return rng.standard_normal(n).astype(np.float32)
    if kind == "pink":
        X = np.fft.rfft(rng.standard_normal(n))
        X /= (np.arange(len(X)) + 1.0) ** 0.5
        return np.fft.irfft(X, n=n).astype(np.float32)
    if kind == "brown":
        X = np.fft.rfft(rng.standard_normal(n))
        X /= (np.arange(len(X)) + 1.0)
        return np.fft.irfft(X, n=n).astype(np.float32)
    if kind == "hum":
        t = np.arange(n) / SR
        f0 = float(rng.choice([50.0, 60.0]))
        y = 0.5 * np.sin(2 * np.pi * f0 * t)
        for k in (2, 3, 4, 5, 7):
            y += rng.uniform(0.03, 0.15) * np.sin(2 * np.pi * f0 * k * t + rng.uniform(0, 6))
        return (y + 0.03 * rng.standard_normal(n)).astype(np.float32)
    if kind == "babble":
        y = np.zeros(n, np.float32)
        for _ in range(int(rng.integers(2, 6))):
            i = int(rng.integers(0, len(pool.lengths)))
            s = pool.get(i)
            if len(s) < n:
                s = np.pad(s, (0, n - len(s)))
            off = int(rng.integers(0, max(1, len(s) - n)))
            y += s[off:off + n] * rng.uniform(0.4, 1.0)
        return y
    if kind == "street":            # 非定常 (ゆっくりした振幅変調 + 低域寄り)
        X = np.fft.rfft(rng.standard_normal(n))
        f = np.arange(len(X)) + 1.0
        X /= f ** 0.7
        base = np.fft.irfft(X, n=n).astype(np.float32)
        mod = 0.5 + 0.5 * np.abs(np.sin(2 * np.pi * rng.uniform(0.2, 2.0) * np.arange(n) / SR))
        return base * mod
    if kind == "click":             # 机を叩く程度の打撃音
        y = 0.02 * rng.standard_normal(n).astype(np.float32)
        for _ in range(int(rng.integers(1, 4))):
            p = int(rng.integers(0, max(1, n - 480)))
            ln = int(0.02 * SR)
            env = np.exp(-np.arange(ln) / (0.002 * SR))
            y[p:p + ln] += rng.uniform(0.2, 1.0) * env * rng.standard_normal(ln).astype(np.float32)
        return y
    raise ValueError(kind)


NOISE_KINDS = ["white", "pink", "brown", "hum", "babble", "street", "click"]


def build_batch(pool: SpeechPool, cache_len: int, batch: int, seg_len: int,
                level_rms: float, rng: np.random.Generator,
                rir_bank: list[np.ndarray]) -> tuple[np.ndarray, np.ndarray]:
    """(ノイズ入り入力, クリーン入力) のバッチを作る。どちらもレベルを揃える。"""
    n = seg_len * batch
    idx = pool.any_longer(seg_len)
    xs = np.zeros((batch, seg_len), np.float32)
    clean_db = []
    for b in range(batch):
        i = int(rng.choice(idx))
        s = pool.get(i)
        if len(s) > seg_len:
            off = int(rng.integers(0, len(s) - seg_len))
            s = s[off:off + seg_len]
        else:
            s = np.pad(s, (0, seg_len - len(s)))
        # 残響 (たまに)
        if rng.random() < 0.25:
            h = rir_bank[int(rng.integers(0, len(rir_bank)))]
            s = np.convolve(s, h, mode="full")[:seg_len].astype(np.float32)
        # レベルを目標 RMS に合わせる (学習と評価で同じ土俵)
        g = level_rms / (np.sqrt(np.mean(s ** 2)) + 1e-12)
        g *= 10 ** (rng.uniform(-4.0, 4.0) / 20.0)
        s = s * g
        kind = NOISE_KINDS[int(rng.integers(0, len(NOISE_KINDS)))]
        nz = make_noise(kind, seg_len, rng, pool)
        target_snr = rng.uniform(-5.0, 20.0)
        gn = np.sqrt(np.mean(s ** 2)) / (np.sqrt(np.mean(nz ** 2)) + 1e-12) * 10 ** (-target_snr / 20)
        x = s + gn * nz
        xs[b] = x
        clean_db.append(20 * np.log10(np.sqrt(np.mean(x ** 2)) + 1e-12))
    return xs, np.asarray(clean_db, np.float32)


# --------------------------------------------------------------------------- #
# 損失
# --------------------------------------------------------------------------- #
def spectral_loss(est: torch.Tensor, ref: torch.Tensor) -> torch.Tensor:
    """教師の spec_e に合わせる。振幅のべき乗圧縮 + 複素 MSE。"""
    e = est.float()
    r = ref.float()
    mag_e = (e[..., 0] ** 2 + e[..., 1] ** 2 + 1e-12).pow(0.15)   # |x|^0.3
    mag_r = (r[..., 0] ** 2 + r[..., 1] ** 2 + 1e-12).pow(0.15)
    l_mag = F.mse_loss(mag_e, mag_r)
    l_ri = F.mse_loss(e, r)
    return l_mag + 0.25 * l_ri


# --------------------------------------------------------------------------- #
def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--upstream", type=Path, default=DEFAULT_UPSTREAM)
    ap.add_argument("--root", type=Path, default=PLUGIN / "build" / "dpdfnet_train")
    ap.add_argument("--teacher", type=Path,
                    default=PLUGIN / "build" / "dpdfnet_train" / "hf" / "checkpoints"
                    / "dpdfnet8_48khz_hr.pth")
    ap.add_argument("--init", type=Path,
                    default=PLUGIN / "build" / "dpdfnet_train" / "ckpt_mid" / "init_dp4.pth")
    ap.add_argument("--out", type=Path,
                    default=PLUGIN / "build" / "dpdfnet_train" / "ckpt_mid" / "dp4.pth")
    ap.add_argument("--blocks", type=int, default=4)
    ap.add_argument("--seg", type=float, default=4.0, help="1 サンプルの長さ (秒)")
    ap.add_argument("--batch", type=int, default=6)
    ap.add_argument("--lr", type=float, default=2e-4)
    ap.add_argument("--steps", type=int, default=100000, help="総ステップ数 (打ち切り上限)")
    ap.add_argument("--minutes", type=float, default=40.0, help="1 回の実行の上限 (分)")
    ap.add_argument("--save-every", type=int, default=200)
    ap.add_argument("--val-every", type=int, default=200)
    ap.add_argument("--val-seconds", type=float, default=10.0)
    ap.add_argument("--seed", type=int, default=1234)
    ap.add_argument("--resume", action="store_true", help="--out があれば続きから")
    ap.add_argument("--no-lr-decay", action="store_true")
    args = ap.parse_args()

    add_offline_paths(args.upstream)
    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    torch.manual_seed(args.seed)
    rng = np.random.default_rng(args.seed)

    # レベル基準: test_signals の音声の RMS に合わせる
    import importlib

    ck = importlib.import_module("check_onnx_model")
    ref_wav = PLUGIN / "test_signals" / "speech_tail_48k.wav"
    xr, sr = ck.read_wav(ref_wav)
    xr = ck.naive_resample(xr, sr, SR) if sr != SR else xr
    level_rms = float(np.sqrt(np.mean(xr ** 2)))
    print(f"[level] 基準 RMS = {level_rms:.4f} ({ref_wav.name}, {sr} Hz -> {SR} Hz)")

    pool = SpeechPool(args.root)
    seg_len = int(args.seg * SR)

    # RIR: 実測 (test_signals/rir_48k.wav) + 合成
    rir_bank: list[np.ndarray] = []
    rir_path = PLUGIN / "test_signals" / "rir_48k.wav"
    if rir_path.is_file():
        r, rsr = ck.read_wav(rir_path)
        if rsr != SR:
            r = ck.naive_resample(r, rsr, SR)
        rir_bank.append((r / (np.sqrt(np.mean(r ** 2)) + 1e-12)).astype(np.float32))
    for rt in (0.15, 0.3, 0.5):
        rir_bank.append(synth_rir(rt, 2 * SR, rng))
    print(f"[rir ] {len(rir_bank)} 本")

    # モデル
    teacher = SpecModel(build_offline(8, device))
    teacher.base.load_state_dict(
        torch.load(str(args.teacher), map_location="cpu", weights_only=True), strict=True)
    teacher.eval()
    for p in teacher.parameters():
        p.requires_grad_(False)

    student = SpecModel(build_offline(args.blocks, device))
    if args.resume and args.out.is_file():
        student.base.load_state_dict(torch.load(str(args.out), map_location="cpu",
                                                weights_only=True), strict=True)
        print(f"[resume] {args.out} から再開")
    else:
        load_dp8_into_mid(student.base, args.init, verbose=True)
    student.train()

    n_par = sum(p.numel() for p in student.parameters())
    print(f"[model] 生徒 {args.blocks} ブロック / {n_par/1e6:.3f}M, 教師 dp8 "
          f"/ {sum(p.numel() for p in teacher.parameters())/1e6:.3f}M, device={device}")

    opt = torch.optim.Adam(student.parameters(), lr=args.lr, betas=(0.9, 0.98), eps=1e-8)

    # 検証データ (固定)
    val_n = int(args.val_seconds * SR)
    v_idx = pool.any_longer(val_n)
    val_pool = SpeechPool.__new__(SpeechPool)   # 同じ pool を使う
    val_pool.raw, val_pool.lengths, val_pool.offsets = pool.raw, pool.lengths, pool.offsets
    vrng = np.random.default_rng(999)
    vx, _ = build_batch(val_pool, 0, 1, val_n, level_rms, vrng, rir_bank)
    vx = vx[0]
    with torch.no_grad():
        vt_spec, _ = teacher(torch.from_numpy(vx)[None].to(device))
        vt_spec = vt_spec.cpu().numpy()

    def validate() -> dict:
        student.eval()
        with torch.no_grad():
            vs, _ = student(torch.from_numpy(vx)[None].to(device))
            vs = vs.float().cpu().numpy()
        student.train()
        se = vs[0]
        while se.ndim > 3:
            se = se[0]
        tr = vt_spec[0]
        while tr.ndim > 3:
            tr = tr[0]
        return {"val_spec_snr": complex_snr_db(tr, se)}

    start = time.time()
    deadline = start + args.minutes * 60
    step = 0
    best = -1e9
    hist: list[dict] = []
    log_path = args.out.with_suffix(".log")
    args.out.parent.mkdir(parents=True, exist_ok=True)

    def log(msg: str) -> None:
        print(msg, flush=True)
        with open(log_path, "a", encoding="utf-8") as f:
            f.write(msg + "\n")

    log(f"=== start {time.strftime('%Y-%m-%d %H:%M:%S')} lr={args.lr} batch={args.batch} "
        f"seg={args.seg} minutes={args.minutes}")

    while step < args.steps and time.time() < deadline:
        xs, lv = build_batch(pool, 0, args.batch, seg_len, level_rms, rng, rir_bank)
        x = torch.from_numpy(xs).to(device)

        with torch.no_grad():
            t_spec, _ = teacher(x)
        s_spec, _ = student(x)
        loss = spectral_loss(s_spec, t_spec)

        opt.zero_grad(set_to_none=True)
        loss.backward()
        torch.nn.utils.clip_grad_norm_(student.parameters(), 1.0)
        opt.step()
        step += 1

        if step % 20 == 0:
            el = time.time() - start
            log(f"[step {step:5d}] loss={loss.item():.5f}  level={lv.mean():.1f} dB  "
                f"{el/60:.1f} min  {step/(el+1e-9):.2f} step/s")

        if args.val_every and step % args.val_every == 0:
            m = validate()
            log(f"[val   {step:5d}] teacher との spec SNR = {m['val_spec_snr']:.2f} dB")
            hist.append({"step": step, "val_spec_snr": m["val_spec_snr"],
                         "loss": loss.item()})
            if m["val_spec_snr"] > best:
                best = m["val_spec_snr"]
                torch.save(student.base.state_dict(), str(args.out.with_suffix(".best.pth")))
                log(f"[best ] {args.out.with_suffix('.best.pth')} (SNR {best:.2f} dB)")

        if args.save_every and step % args.save_every == 0:
            torch.save(student.base.state_dict(), str(args.out))
            log(f"[save ] {args.out} (step {step})")

    torch.save(student.base.state_dict(), str(args.out))
    meta = {"steps": step, "minutes": (time.time() - start) / 60, "best_val_spec_snr": best,
            "lr": args.lr, "batch": args.batch, "seg": args.seg, "blocks": args.blocks,
            "hist": hist}
    args.out.with_suffix(".state.json").write_text(
        json.dumps(meta, ensure_ascii=False, indent=2), encoding="utf-8")
    log(f"=== stop  {time.strftime('%Y-%m-%d %H:%M:%S')} step={step} "
        f"({(time.time()-start)/60:.1f} min) best={best:.2f} dB -> {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
