//! VST3 シェルのオフライン検証（DAW もホスト API も使わずに DSP 経路を確認する）。
//!
//! コア（`dpdfnet`）の中身は向こうのテストで検証済みなので、ここで見るのは
//! **シェルが音を壊していないか**:
//!
//!   1. ホストのブロックサイズを変えても出力がビット一致すること
//!      （DAW は 1 サンプルから数千サンプルまで好きな長さで呼んでくる）
//!   2. モノラルとステレオで、同じ入力のチャンネル出力が一致すること
//!   3. 遅延の報告が 48 kHz で 2880 サンプル、他レートで比例して増えること
//!   4. 「自動」モデルの起動時キャリブレーションが痕跡を残さないこと
//!      （落ちたときは軽量モデルの出力と一致し、落ちないときは品質モデルと一致する）
//!
//! 実行方法（モデルと ONNX Runtime を指定する。無ければスキップして成功扱い）:
//! ```text
//! set DPDFNET_ORT=..\obs_plugin\vendor
//! set DPDFNET_MODEL=..\plugin\models\dpdfnet2_48khz_hr_2out.onnx
//! cargo test --lib -- --nocapture
//! ```

use std::path::PathBuf;

use dpdfnet::engine::ParamSnapshot;
use dpdfnet::paths::{DeviceChoice, ModelChoice};

use crate::core::Core;

/// テスト信号（決定的）。正弦 2 本 + 小さな擬似乱数ノイズ。
fn test_signal(n: usize) -> Vec<f32> {
    let mut state: u32 = 0x1234_5678;
    (0..n)
        .map(|i| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = ((state >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.02;
            let t = i as f32 / 48_000.0;
            let s = 0.5 * (std::f32::consts::TAU * 220.0 * t).sin()
                + 0.3 * (std::f32::consts::TAU * 440.0 * t).sin();
            s * 0.5 + noise
        })
        .collect()
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// テストできる環境か（モデルと ONNX Runtime があるか）。
///
/// dpdfnet 側のオフライン検証と同じ流儀で、無ければスキップする。
fn ready() -> bool {
    if env_path("DPDFNET_MODEL").is_none() {
        eprintln!("skip: DPDFNET_MODEL が未設定");
        return false;
    }
    if env_path("DPDFNET_ORT").is_none() {
        eprintln!("skip: DPDFNET_ORT が未設定");
        return false;
    }
    true
}

/// 組み立てて 1 秒ぶん流す。
fn run(channels: usize, block: usize, model: ModelChoice) -> (Core, Vec<Vec<f32>>) {
    let mut core = Core::new();
    core.build(channels, 48_000.0, model, DeviceChoice::Cpu);
    if let Some(e) = core.error() {
        panic!("エンジンを組み立てられない: {e}");
    }

    let x = test_signal(48_000);
    let mut ch = vec![x; channels];
    core.process_all(&mut ch, block, ParamSnapshot::default());
    (core, ch)
}

#[test]
fn output_does_not_depend_on_the_host_block_size() {
    if !ready() {
        return;
    }

    let (core, a) = run(2, 1024, ModelChoice::Quality);
    eprintln!("built model = {}", core.model().as_str());

    for block in [1, 480, 960, 4096, 8192] {
        let (_, b) = run(2, block, ModelChoice::Quality);
        assert_eq!(a, b, "ブロックサイズ {block} で出力が変わった");
    }
    eprintln!("block size 1 / 480 / 960 / 1024 / 4096 / 8192 でビット一致");
}

#[test]
fn mono_and_stereo_produce_the_same_channels() {
    if !ready() {
        return;
    }

    let (_, stereo) = run(2, 1024, ModelChoice::Quality);
    let (_, mono) = run(1, 1024, ModelChoice::Quality);

    assert_eq!(stereo[0], mono[0], "ステレオ L とモノラルが違う");
    assert_eq!(stereo[1], mono[0], "ステレオ R とモノラルが違う");
    eprintln!("モノ vs ステレオ（同一入力）: ビット一致");
}

#[test]
fn latency_is_reported_in_host_samples() {
    if !ready() {
        return;
    }

    let build = |rate: f32| {
        let mut core = Core::new();
        core.build(1, rate, ModelChoice::Quality, DeviceChoice::Cpu);
        if let Some(e) = core.error() {
            panic!("エンジンを組み立てられない: {e}");
        }
        core
    };

    // 48 kHz: 内部 6 hop = 2880 サンプル = 60 ms がそのまま。
    let core48 = build(48_000.0);
    assert_eq!(core48.latency_samples(48_000.0), 2880);

    // 96 kHz: 内部は 48 kHz のままなので、報告値は 2 倍 + 変換段の遅延。
    let core96 = build(96_000.0);
    let l96 = core96.latency_samples(96_000.0);
    assert!(l96 > 5760, "96 kHz の遅延が {l96}（変換段ぶんが足りない）");
    eprintln!("latency: 48 kHz = 2880 / 96 kHz = {l96} samples");
}

#[test]
fn auto_model_calibration_picks_one_model_without_leaving_a_trace() {
    if !ready() {
        return;
    }

    // 品質 / 軽量それぞれの「正解」を出しておく。
    let (q_core, quality) = run(2, 1024, ModelChoice::Quality);
    assert_eq!(q_core.model(), ModelChoice::Quality);
    let (f_core, light) = run(2, 1024, ModelChoice::Fast);
    assert_eq!(f_core.model(), ModelChoice::Fast);

    // 「自動」は起動時に実測してどちらかを選ぶ（選んだ方の出力と一致するはず）。
    let (auto_core, auto) = run(2, 1024, ModelChoice::Auto);
    let chosen = auto_core.model();
    eprintln!("自動 → {}", chosen.as_str());

    let expected = match chosen {
        ModelChoice::Fast => &light,
        _ => &quality,
    };
    assert_eq!(&auto, expected, "自動の出力が選んだモデルの出力と違う");
}
