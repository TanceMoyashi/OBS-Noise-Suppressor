//! オフライン検証（OBS を起動せずに DSP を実モデルで回す）。
//!
//! 確認すること:
//!   1. 内容遅延がちょうど 6 hop = 2880 サンプル (60 ms) であること
//!      （`ANALYSIS.md` §13.2 の実測値。これを変えてはいけない）
//!   2. モノラル 1 ch とステレオ 2 ch で、同じ入力に対する出力が同一であること
//!   3. 決定的であること（2 回回して一致）
//!
//! 実行方法:
//! ```text
//! set DPDFNET_ORT=D:\...\x86_64-win        (dpdfnet_onnxruntime.dll のあるディレクトリ)
//! set DPDFNET_MODEL=D:\...\dpdfnet2_48khz_hr_2out.onnx
//! PATH に C:\Program Files\obs-studio\bin\64bit を通す（obs.dll の import 解決）
//! cargo test --lib offline -- --nocapture
//! ```
//!
//! `DPDFNET_ORT` / `DPDFNET_MODEL` が無いときは「スキップ」して成功扱いにする
//! （CI や他の環境でビルドだけ通したいときのため）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::engine;
use super::engine::Meters;

// --------------------------------------------------------------------------
// ヘルパ
// --------------------------------------------------------------------------

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

fn read_wav_mono(path: &Path) -> Option<(Vec<f32>, u32)> {
    let mut r = hound::WavReader::open(path).ok()?;
    let spec = r.spec();

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().filter_map(|s| s.ok()).collect(),
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .filter_map(|s| s.ok())
                .map(|v| v as f32 / max)
                .collect()
        }
    };

    let ch = spec.channels.max(1) as usize;
    let mono = if ch > 1 {
        samples
            .chunks(ch)
            .map(|c| c.iter().sum::<f32>() / ch as f32)
            .collect()
    } else {
        samples
    };

    Some((mono, spec.sample_rate))
}

fn rms(v: &[f32]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let s: f64 = v.iter().map(|x| (*x as f64) * (*x as f64)).sum();
    (s / v.len() as f64).sqrt()
}

/// `output` が `input` より何サンプル遅れているかを相互相関で推定する。
///
/// 粗探索（`step` 倍に間引き）→ 局所探索（等倍）の 2 段。O(n^2) を避ける。
/// 符号は `tools/sim_stream.py` に合わせ、ピークの絶対値を取る。
fn estimate_lag(input: &[f32], output: &[f32], max_lag: usize, step: usize) -> (usize, f64) {
    let n = input.len().min(output.len());
    if n == 0 {
        return (0, 0.0);
    }

    let mean_in = input[..n].iter().map(|v| *v as f64).sum::<f64>() / n as f64;
    let mean_out = output[..n].iter().map(|v| *v as f64).sum::<f64>() / n as f64;

    let a: Vec<f64> = input[..n].iter().map(|v| *v as f64 - mean_in).collect();
    let b: Vec<f64> = output[..n].iter().map(|v| *v as f64 - mean_out).collect();

    let step = step.max(1);
    let ad: Vec<f64> = a.iter().step_by(step).copied().collect();
    let bd: Vec<f64> = b.iter().step_by(step).copied().collect();
    let nd = ad.len().min(bd.len());
    let max_lag_d = (max_lag / step).min(nd.saturating_sub(1));

    let mut best_l = 0usize;
    let mut best = 0.0f64;

    for l in 0..=max_lag_d {
        let mut sum = 0.0f64;
        for i in l..nd {
            sum += bd[i] * ad[i - l];
        }
        if sum.abs() > best {
            best = sum.abs();
            best_l = l;
        }
    }

    // 等倍で ±数サンプルまで詰める。
    let center = best_l * step;
    let lo = center.saturating_sub(2 * step);
    let hi = (center + 3 * step).min(n.saturating_sub(1));

    let mut fine_l = center;
    let mut fine = 0.0f64;
    for l in lo..=hi {
        let mut sum = 0.0f64;
        for i in l..n {
            sum += b[i] * a[i - l];
        }
        if sum.abs() > fine {
            fine = sum.abs();
            fine_l = l;
        }
    }

    // 正規化相関係数
    let mut num = 0.0f64;
    let mut d1 = 0.0f64;
    let mut d2 = 0.0f64;
    for i in fine_l..n {
        let x = a[i - fine_l];
        let y = b[i];
        num += x * y;
        d1 += x * x;
        d2 += y * y;
    }
    let corr = if d1 <= 0.0 || d2 <= 0.0 {
        0.0
    } else {
        (num / (d1.sqrt() * d2.sqrt())).abs()
    };

    (fine_l, corr)
}

/// 32 bit float WAV をそのまま（プラナーのまま）読む。
///
/// OBS を強制終了してダンプを取ると RIFF サイズが 0 のまま残るので、
/// ヘッダのサイズは信用せず**実ファイル長**からデータ長を決める。
fn read_wav_planar_loose(path: &Path) -> Option<(Vec<Vec<f32>>, u32, usize)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }

    let u16at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let u32at = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);

    let mut pos = 12usize;
    let mut channels = 1usize;
    let mut rate = 48_000u32;
    let mut bits = 32u16;
    let mut format = 1u16;
    let mut data_off = 44usize;

    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32at(pos + 4) as usize;
        if id == b"fmt " && pos + 24 <= bytes.len() {
            format = u16at(pos + 8);
            channels = u16at(pos + 10).max(1) as usize;
            rate = u32at(pos + 12);
            bits = u16at(pos + 22);
        } else if id == b"data" {
            data_off = pos + 8;
            break;
        }
        pos += 8 + size + (size & 1);
    }

    assert_eq!(format, 3, "float WAV であること: {}", path.display());
    assert_eq!(bits, 32, "32 bit であること: {}", path.display());

    let data = &bytes[data_off.min(bytes.len())..];
    let nsamples = data.len() / 4 / channels;
    let mut planar = vec![Vec::with_capacity(nsamples); channels];
    for i in 0..nsamples {
        for (ch, plane) in planar.iter_mut().enumerate() {
            let o = (i * channels + ch) * 4;
            plane.push(f32::from_le_bytes([
                data[o],
                data[o + 1],
                data[o + 2],
                data[o + 3],
            ]));
        }
    }

    Some((planar, rate, channels))
}

fn find_test_wav() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(p) = env_path("DPDFNET_TEST_WAV") {
        candidates.push(p);
    }

    // <repo>/clear_re/plugin/test_signals/*.wav を探す
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = here.parent().and_then(|p| p.parent()).map(|p| p.to_path_buf());
    if let Some(repo) = repo {
        for name in [
            "speech_tail_48k.wav",
            "speech_tail_reverb_48k.wav",
            "impulse_48k.wav",
        ] {
            candidates.push(repo.join("clear_re/plugin/test_signals").join(name));
        }
        candidates.push(repo.join("clear_re/plugin/test_signals/speech_with_noise.wav"));
    }

    candidates.into_iter().find(|p| p.is_file())
}

/// 入力をブロック単位で流し切って出力を返す。
fn run_stream(eng: &mut engine::Engine, ch: &[Vec<f32>], block: usize, p: engine::ParamSnapshot) -> Vec<Vec<f32>> {
    let frames = ch[0].len();
    // `Engine::process` は OBS と同じ「バッファ内で入力を読み出力で上書き」する
    // in-place 動作なので、入力そのものを作業バッファとして渡す。
    let mut work: Vec<Vec<f32>> = ch.to_vec();

    let mut pos = 0usize;
    while pos < frames {
        let n = block.min(frames - pos);

        // OBS と同じく planar な生ポインタの配列を作る。
        let mut ptrs: Vec<*mut f32> = work.iter_mut().map(|v| v[pos..].as_mut_ptr()).collect();
        eng.process(&mut ptrs, n, p);

        pos += n;
    }

    work
}

// --------------------------------------------------------------------------
// テスト
// --------------------------------------------------------------------------

#[test]
fn debug_steps() {
    let Some(ort_dir) = env_path("DPDFNET_ORT") else {
        eprintln!("skip");
        return;
    };
    let Some(model) = env_path("DPDFNET_MODEL") else {
        eprintln!("skip");
        return;
    };

    eprintln!("A: loading ort");
    let api = super::ort::ensure_loaded(std::slice::from_ref(&ort_dir)).expect("ort");
    eprintln!("B: ort ok api_version={} {}", super::ort::api_version(), super::ort::description());
    let _ = api;

    eprintln!("C: loading model");
    let mut net = super::net::DpdfNet::load(&model, 1, std::slice::from_ref(&ort_dir)).expect("load");
    eprintln!(
        "D: load ok freq_bins={} state={} dereverb={} err={}",
        net.freq_bins(),
        net.state_size(),
        net.has_dereverb_output(),
        net.last_error()
    );

    let n = 2 * net.freq_bins();
    let spec = vec![0.0f32; n];
    let mut of = vec![0.0f32; n];
    let mut on = vec![0.0f32; n];
    for i in 0..3 {
        net.process_frame(&spec, &mut of, &mut on);
        eprintln!("E{i}: frame ok");
    }

    // STFT 経由で正弦波を通し、各段の RMS を見る。
    let mut stft = super::stft::VorbisStft::new();
    assert!(stft.prepare(960, 960, 480), "stft prepare");
    let mut frame = vec![0.0f32; n];
    let mut out = vec![0.0f32; 480];
    let mut phase = 0.0f32;

    for step in 0..10 {
        let hop_in: Vec<f32> = (0..480)
            .map(|_| {
                phase += 1.0;
                (phase * 0.057_6).sin() * 0.3
            })
            .collect();

        stft.analyse(&hop_in, &mut frame);
        let frame_rms = rms(&frame);
        net.process_frame(&frame, &mut of, &mut on);
        let got = stft.synthesise(&of, &mut out);
        eprintln!(
            "S{step}: in={:.6} frame={:.6} of={:.6} on={:.6} got={} out={:.6}",
            rms(&hop_in),
            frame_rms,
            rms(&of),
            rms(&on),
            got,
            rms(&out)
        );
    }

    eprintln!("F: dropping net");
    drop(net);
    eprintln!("G: dropped");
}

#[test]
fn debug_paths() {
    let Some(ort_dir) = env_path("DPDFNET_ORT") else {
        eprintln!("skip: DPDFNET_ORT");
        return;
    };
    let Some(model) = env_path("DPDFNET_MODEL") else {
        eprintln!("skip: DPDFNET_MODEL");
        return;
    };
    let Some(wav) = find_test_wav() else {
        eprintln!("skip: WAV");
        return;
    };

    let (mut x, sr) = read_wav_mono(&wav).expect("wav");
    let pad = (0.2 * sr as f64) as usize;
    x.truncate(4 * sr as usize);
    let mut padded = vec![0.0f32; pad];
    padded.extend_from_slice(&x);
    let x = padded;

    let mut eng = engine::build(1, &model, std::slice::from_ref(&ort_dir), Arc::new(Meters::new()))
        .expect("build");
    let hop = eng.hop();

    let mut out = x.clone();
    let mut pos = 0usize;
    let mut idx = 0usize;

    while pos < x.len() {
        let n = hop.min(x.len() - pos);
        // in-place: バッファが入力を保持し、出力で上書きされる。
        let mut ptrs = vec![out[pos..].as_mut_ptr()];
        eng.process(&mut ptrs, n, engine::ParamSnapshot::default());

        let p = eng.debug_probe();
        let out_rms = rms(&out[pos..pos + n]);
        if idx < 12 || (idx % 20 == 0 && idx < 300) {
            eprintln!(
                "hop {idx:3}: in={:.5} frame={:.3} e={:.6} ns={:.6} blend={:.3} ring0={:.3} alpha={:.4} step={} out={:.6}",
                p.0, p.1, p.2, p.3, p.4, p.5, p.6, p.7, out_rms
            );
        }

        pos += n;
        idx += 1;
    }

    eprintln!("total out rms = {:.6} (input {:.6})", rms(&out), rms(&x));
}

#[test]
fn offline_latency_and_channel_consistency() {
    let Some(ort_dir) = env_path("DPDFNET_ORT") else {
        eprintln!("skip: DPDFNET_ORT が未設定");
        return;
    };
    let Some(model) = env_path("DPDFNET_MODEL") else {
        eprintln!("skip: DPDFNET_MODEL が未設定");
        return;
    };
    let Some(wav) = find_test_wav() else {
        eprintln!("skip: テスト用 WAV が見つからない");
        return;
    };

    let (mut x, sr) = read_wav_mono(&wav).expect("WAV 読み込み失敗");
    assert_eq!(sr, 48_000, "テスト信号は 48 kHz であること: {}", wav.display());

    // sim_stream.py と同じく先頭に 0.2 秒の無音を入れてから 4 秒ぶん使う。
    let pad = (0.2 * sr as f64) as usize;
    let mut padded = vec![0.0f32; pad];
    x.truncate(4 * sr as usize);
    padded.extend_from_slice(&x);
    let x = padded;

    let params = engine::ParamSnapshot::default();
    let block = 1024usize;

    // ---- 1 ch ----
    let mut eng1 = engine::build(1, &model, std::slice::from_ref(&ort_dir), Arc::new(Meters::new()))
        .expect("エンジン構築に失敗");
    let hop = eng1.hop();
    assert_eq!(hop, 480, "hop は 480 であること");
    assert_eq!(
        eng1.latency_samples,
        engine::K_LATENCY_HOPS * hop,
        "報告する遅延は 6 hop であること"
    );

    let y1 = run_stream(&mut eng1, std::slice::from_ref(&x), block, params);
    eprintln!(
        "model={} wav={} samples={} in_rms={:.6} out_rms={:.6} dereverb={}",
        model.display(),
        wav.display(),
        x.len(),
        rms(&x),
        rms(&y1[0]),
        eng1.dereverb_available
    );

    assert!(rms(&y1[0]) > 1e-5, "出力が無音");

    let (lag, corr) = estimate_lag(&x, &y1[0], 24_000, 16);
    eprintln!("lag = {lag} samples ({:.3} hop, {:.1} ms) corr = {corr:.4}",
        lag as f64 / hop as f64, lag as f64 / sr as f64 * 1000.0);

    assert!(
        corr > 0.3,
        "入力と出力の相関が低すぎる ({corr})。ラグ推定が信用できない"
    );
    assert_eq!(
        lag,
        engine::K_LATENCY_HOPS * hop,
        "内容遅延が {} サンプルではない (実測 {lag})",
        engine::K_LATENCY_HOPS * hop
    );

    // ---- 2 ch (同一入力を 2 回) ----
    let mut eng2 = engine::build(2, &model, std::slice::from_ref(&ort_dir), Arc::new(Meters::new()))
        .expect("ステレオエンジン構築に失敗");
    let stereo_in = vec![x.clone(), x.clone()];
    let y2 = run_stream(&mut eng2, &stereo_in, block, params);

    assert_eq!(y2.len(), 2);
    let max_diff = y1[0]
        .iter()
        .zip(y2[0].iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    eprintln!("mono vs stereo[0] max diff = {max_diff:e}");
    assert!(
        max_diff <= 1e-6,
        "モノラルとステレオで結果が違う (max diff {max_diff})"
    );

    let max_diff_lr = y2[0]
        .iter()
        .zip(y2[1].iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(max_diff_lr <= 1e-6, "L/R が独立していない (max diff {max_diff_lr})");

    // ---- 決定性 ----
    let mut eng3 = engine::build(1, &model, std::slice::from_ref(&ort_dir), Arc::new(Meters::new()))
        .expect("エンジン再構築に失敗");
    let y3 = run_stream(&mut eng3, std::slice::from_ref(&x), block, params);
    let max_diff3 = y1[0]
        .iter()
        .zip(y3[0].iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(max_diff3 == 0.0, "決定的でない (max diff {max_diff3})");
}

/// OBS 実機のダンプ（`DPDFNET_DUMP`）を、同じ DSP に通した結果と突き合わせる。
///
/// OBS 側で書いた `_in.wav` を入力として、**同じエンジン**をオフラインで回し、
/// OBS が書いた `_out.wav` と比較する。両者は同じサンプル列を同じ順で処理するので
/// 一致するはずで、ずれていれば「OBS 経路のどこか（チャンネル、ブロック、
/// フォーマット変換）がおかしい」と分かる。
///
/// 実行:
/// ```text
/// set DPDFNET_LIVE_DUMP=C:\...\live\dump
/// cargo test --release --lib live_dump -- --nocapture
/// ```
#[test]
fn live_dump_matches_reference() {
    let Some(prefix) = env_path("DPDFNET_LIVE_DUMP") else {
        eprintln!("skip: DPDFNET_LIVE_DUMP が未設定");
        return;
    };
    let Some(ort_dir) = env_path("DPDFNET_ORT") else {
        eprintln!("skip: DPDFNET_ORT が未設定");
        return;
    };
    let Some(model) = env_path("DPDFNET_MODEL") else {
        eprintln!("skip: DPDFNET_MODEL が未設定");
        return;
    };

    let p_in = PathBuf::from(format!("{}_in.wav", prefix.display()));
    let p_out = PathBuf::from(format!("{}_out.wav", prefix.display()));
    let (x, sr, ch) = read_wav_planar_loose(&p_in).expect("ダンプ (_in.wav) を読めない");
    let (y, sr_out, ch_out) = read_wav_planar_loose(&p_out).expect("ダンプ (_out.wav) を読めない");

    assert_eq!(sr, 48_000, "48 kHz でない");
    assert_eq!(sr_out, 48_000, "48 kHz でない");
    assert_eq!(ch, ch_out, "入出力のチャンネル数が違う");

    let frames_in = x[0].len();
    let frames_out = y[0].len();
    eprintln!(
        "live dump: {} / {} ch / in {} frames / out {} frames",
        p_in.display(),
        ch,
        frames_in,
        frames_out
    );
    assert!(frames_in > 48_000, "ダンプが短すぎる（フィルタが回っていない可能性）");

    // ---- 1) 実機の遅延（in → out） ----
    let n = frames_in.min(frames_out);
    assert!(n > 48_000);
    let (lag, corr) = estimate_lag(&x[0][..n], &y[0][..n], 24_000, 16);
    eprintln!(
        "実機 lag = {lag} samples ({:.3} hop, {:.1} ms) corr = {corr:.4} (in_rms {:.6} out_rms {:.6})",
        lag as f64 / 480.0,
        lag as f64 / 48_000.0 * 1000.0,
        rms(&x[0][..n]),
        rms(&y[0][..n])
    );
    assert!(corr > 0.3, "相関が低すぎる ({corr})");
    assert_eq!(
        lag,
        engine::K_LATENCY_HOPS * 480,
        "実機の遅延が {} サンプルではない",
        engine::K_LATENCY_HOPS * 480
    );

    // ---- 2) 同じ入力をオフラインで回して _out と比較 ----
    let ch_used = ch.clamp(1, 2);
    let mut eng = engine::build(
        ch_used,
        &model,
        std::slice::from_ref(&ort_dir),
        Arc::new(Meters::new()),
    )
    .expect("エンジン構築に失敗");

    let input: Vec<Vec<f32>> = x[..ch_used].to_vec();
    let ys = run_stream(&mut eng, &input, 1024, engine::ParamSnapshot::default());

    let m = ys[0].len().min(frames_out);
    assert!(m > 48_000, "比較できる長さが足りない");
    let mut max_diff = 0.0f32;
    let mut sum_sq = 0.0f64;
    for ch_i in 0..ch_used {
        for i in 0..m {
            let d = (ys[ch_i][i] - y[ch_i][i]).abs();
            if d > max_diff {
                max_diff = d;
            }
            sum_sq += (d as f64) * (d as f64);
        }
    }
    let diff_rms = (sum_sq / (m * ch_used) as f64).sqrt();

    eprintln!(
        "参照 vs 実機: {m} frames x {ch_used} ch / max diff = {max_diff:e} / diff rms = {diff_rms:e}"
    );
    eprintln!(
        "  参照 out_rms = {:.6} / 実機 out_rms = {:.6}",
        rms(&ys[0][..m]),
        rms(&y[0][..m])
    );

    assert!(
        max_diff < 1e-4,
        "実機の出力がオフライン参照と一致しない (max diff {max_diff:e})"
    );
}
