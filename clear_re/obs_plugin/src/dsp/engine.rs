//! ホップスケジューラとチャンネルごとの DSP。
//!
//! 移植元: `PluginProcessor::processBlock` / `runHop` / `updateMeters`。
//!
//! 本体は 48 kHz 固定。`hop = 480`、内容遅延は 6 hop = 2880 サンプル (60 ms)。
//! この遅延量は移植元の実測値 (`ANALYSIS.md` §13) と同じで、変えてはいけない。
//!
//! OBS のレートが 48 kHz 以外のときは、`Engine::process` が入口で 48 kHz へ
//! 落とし、出口で元のレートへ戻す（`Rack`）。`process_48k` 以下の本体は
//! レートを知らないので、48 kHz のときの経路は今までと完全に同じ。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use super::net::DpdfNet;
use super::params;
use super::stft::VorbisStft;
use super::INTERNAL_SAMPLE_RATE;
use crate::paths::DeviceChoice;

/// 実測した内容遅延（ホップ数）。`ANALYSIS.md` §13.2 / tools/sim_stream.py。
pub const K_LATENCY_HOPS: usize = 6;

/// モデル出力 1 フレームが参照する入力のずれ。
pub const K_MODEL_LOOKAHEAD_HOPS: usize = 4;

/// UI が読むパラメータのスナップショット（アトミックから集めた値）。
#[derive(Clone, Copy, Debug)]
pub struct ParamSnapshot {
    pub ambience: f32,
    pub voice_db: f32,
    pub voice_reverb: f32,
    /// フェーダーごとのミュート / バイパス（つまみの値は上書きしない）。
    pub ambience_mode: params::FeatureMode,
    pub voice_mode: params::FeatureMode,
    pub voice_reverb_mode: params::FeatureMode,
}

impl Default for ParamSnapshot {
    fn default() -> Self {
        Self {
            ambience: params::AMBIENCE_DEFAULT as f32,
            voice_db: params::VOICE_DEFAULT as f32,
            voice_reverb: params::VOICE_REVERB_DEFAULT as f32,
            ambience_mode: params::FeatureMode::default(),
            voice_mode: params::FeatureMode::default(),
            voice_reverb_mode: params::FeatureMode::default(),
        }
    }
}

/// 1 ホップぶんの処理時間の累計（リアルタイム余裕の診断用）。
///
/// `hops` / `sum_us` は累計なので、監視側は 2 点の差分を取れば
/// 「直近 1 秒の平均」が出せる（累計どうしの引き算なので、
/// 途中でエンジンが組み直されても数値が飛ばない）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DspStats {
    /// 計測したホップ数。
    pub hops: u64,
    /// 予算（= 1 ホップの実時間）を超えたホップ数。
    pub over: u64,
    /// 処理時間の合計 [µs]。
    pub sum_us: u64,
    /// 単発の最大 [µs]。
    pub max_us: u32,
    /// 1 ホップの予算 [µs]。0 = 未設定（計測なし）。
    pub budget_us: u32,
}

/// レベルメータ。オーディオスレッドが書き、UI スレッドが読む。
///
/// すべてリニア RMS（フルスケール = 1.0）。UI 側で dB / VU / GR に変換する。
#[derive(Debug)]
pub struct Meters {
    /// 入力（加工前）の RMS [ch]。パネルのアナログ VU 用。
    in_rms: [AtomicU32; 2],
    /// 入力（加工前）のピーク [ch]（ブロック内最大 + 短い戻り）。パネルの
    /// ピークホールド表示用。
    in_peak: [AtomicU32; 2],
    /// 出力（加工後）の RMS [ch]。GR メーター用。
    out_rms: [AtomicU32; 2],
    /// 出力（加工後）のピーク [ch]。パネルのピークホールド表示用。
    out_peak: [AtomicU32; 2],
    /// 出力の 3 バンド（低 / 中 / 高）RMS [ch][band]。
    levels: [[AtomicU32; 3]; 2],
    /// 音声ブロックを処理するたびに増える通し番号。
    ///
    /// パネルはこれを見て「前回の描画から新しい音声が来たか」を判定し、
    /// 来ていなければ針を静止位置へ戻す（無音時に針が止まらないように）。
    seq: AtomicU64,

    /// リアルタイム余裕の診断（`DspStats`）。オーディオスレッドが 1 ホップ
    /// ごとに足し込み、監視スレッド（`filter::watchdog`）が読む。
    dsp_hops: AtomicU64,
    dsp_over: AtomicU64,
    dsp_sum_us: AtomicU64,
    dsp_max_us: AtomicU32,
    dsp_budget_us: AtomicU32,
}

impl Meters {
    pub fn new() -> Self {
        Self {
            in_rms: [AtomicU32::new(0), AtomicU32::new(0)],
            in_peak: [AtomicU32::new(0), AtomicU32::new(0)],
            out_rms: [AtomicU32::new(0), AtomicU32::new(0)],
            out_peak: [AtomicU32::new(0), AtomicU32::new(0)],
            levels: [
                [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)],
                [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)],
            ],
            seq: AtomicU64::new(0),
            dsp_hops: AtomicU64::new(0),
            dsp_over: AtomicU64::new(0),
            dsp_sum_us: AtomicU64::new(0),
            dsp_max_us: AtomicU32::new(0),
            dsp_budget_us: AtomicU32::new(0),
        }
    }

    #[inline]
    pub fn get(&self, ch: usize, band: usize) -> f32 {
        let ch = ch.min(1);
        let band = band.min(2);
        f32::from_bits(self.levels[ch][band].load(Ordering::Relaxed))
    }

    /// 入力（加工前）RMS [ch]。
    #[inline]
    pub fn in_level(&self, ch: usize) -> f32 {
        f32::from_bits(self.in_rms[ch.min(1)].load(Ordering::Relaxed))
    }

    /// 入力（加工前）ピーク [ch]。
    #[inline]
    pub fn in_peak(&self, ch: usize) -> f32 {
        f32::from_bits(self.in_peak[ch.min(1)].load(Ordering::Relaxed))
    }

    /// 出力（加工後）RMS [ch]。
    #[inline]
    pub fn out_level(&self, ch: usize) -> f32 {
        f32::from_bits(self.out_rms[ch.min(1)].load(Ordering::Relaxed))
    }

    /// 出力（加工後）ピーク [ch]。
    #[inline]
    pub fn out_peak(&self, ch: usize) -> f32 {
        f32::from_bits(self.out_peak[ch.min(1)].load(Ordering::Relaxed))
    }

    /// 処理した音声ブロックの通し番号。
    #[inline]
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    #[inline]
    pub(crate) fn bump_seq(&self) {
        self.seq.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn set(&self, ch: usize, band: usize, v: f32) {
        let ch = ch.min(1);
        let band = band.min(2);
        self.levels[ch][band].store(v.to_bits(), Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn set_in(&self, ch: usize, v: f32) {
        self.in_rms[ch.min(1)].store(v.to_bits(), Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn set_in_peak(&self, ch: usize, v: f32) {
        self.in_peak[ch.min(1)].store(v.to_bits(), Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn set_out(&self, ch: usize, v: f32) {
        self.out_rms[ch.min(1)].store(v.to_bits(), Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn set_out_peak(&self, ch: usize, v: f32) {
        self.out_peak[ch.min(1)].store(v.to_bits(), Ordering::Relaxed);
    }

    /// 1 ホップぶんの処理時間 [µs] を足し込む。
    ///
    /// オーディオスレッドから呼ぶ。ロックもアロケーションもしない
    /// （リラックスしたアトミックの読み書きだけ）。
    #[inline]
    pub(crate) fn record_dsp_time(&self, us: u32) {
        let budget = self.dsp_budget_us.load(Ordering::Relaxed);
        self.dsp_hops.fetch_add(1, Ordering::Relaxed);
        self.dsp_sum_us.fetch_add(us as u64, Ordering::Relaxed);
        if budget != 0 && us > budget {
            self.dsp_over.fetch_add(1, Ordering::Relaxed);
        }

        // 最大値は診断用なので、緩い競合は許容する。
        let mut cur = self.dsp_max_us.load(Ordering::Relaxed);
        while us > cur {
            match self.dsp_max_us.compare_exchange_weak(
                cur,
                us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(v) => cur = v,
            }
        }
    }

    /// 1 ホップの予算 [µs] を決める（エンジン構築時に 1 回）。
    pub(crate) fn set_dsp_budget_us(&self, us: u32) {
        self.dsp_budget_us.store(us, Ordering::Relaxed);
    }

    /// 負荷の累計。監視スレッドが読む。
    pub fn dsp_stats(&self) -> DspStats {
        DspStats {
            hops: self.dsp_hops.load(Ordering::Relaxed),
            over: self.dsp_over.load(Ordering::Relaxed),
            sum_us: self.dsp_sum_us.load(Ordering::Relaxed),
            max_us: self.dsp_max_us.load(Ordering::Relaxed),
            budget_us: self.dsp_budget_us.load(Ordering::Relaxed),
        }
    }
}

impl Default for Meters {
    fn default() -> Self {
        Self::new()
    }
}

/// 1 チャンネルぶんの DSP 状態。
struct Channel {
    stft: VorbisStft,
    engine: DpdfNet,

    /// 現在の解析フレーム（複素スペクトルを re,im 交互で保持）。
    frame: Vec<f32>,
    /// 直近 4 フレームの「整列入力」。モデル出力とのブレンドに使う。
    frame_ring: [Vec<f32>; K_MODEL_LOOKAHEAD_HOPS],
    out_full: Vec<f32>,
    out_ns: Vec<f32>,
    blended: Vec<f32>,
    hop_out: Vec<f32>,

    step: u64,
}

impl Channel {
    fn new(engine: DpdfNet, meta_hop: usize, freq_bins: usize) -> Self {
        let spec_count = 2 * freq_bins;
        Self {
            stft: VorbisStft::new(),
            engine,
            frame: vec![0.0; spec_count],
            frame_ring: std::array::from_fn(|_| vec![0.0; spec_count]),
            out_full: vec![0.0; spec_count],
            out_ns: vec![0.0; spec_count],
            blended: vec![0.0; spec_count],
            hop_out: vec![0.0; meta_hop],
            step: 0,
        }
    }
}

/// レート変換一式（OBS のレート ↔ 48 kHz）。48 kHz のときは作らないので、
/// 通常運用（48 kHz）の経路は今までと 1 サンプルも変わらない。
struct Rack {
    /// OBS レート → 48 kHz（チャンネルごと）。
    up: Vec<super::resample::Stream>,
    /// 48 kHz → OBS レート（チャンネルごと）。
    down: Vec<super::resample::Stream>,
    /// 48 kHz の作業領域。本体はここをその場で書き換える。
    in48: Vec<Vec<f32>>,
    /// 48 kHz → OBS レートの出力を受ける作業領域。
    tail: Vec<Vec<f32>>,
    /// 前のチャンクから持ち越した出力（入出力を 1:1 に保つための端数）。
    carry: Vec<Vec<f32>>,
    carry_len: usize,
    /// 出力が足りずゼロで埋めたサンプル数（診断用。起動直後以外は 0 のはず）。
    starved: u64,
    /// 変換で増える遅延 [ms]。
    added_ms: f32,
}

impl Rack {
    /// 変換段を作る。比が大きすぎるレート（想定外の値）は `None`。
    fn new(rate: u32, num_channels: usize) -> Option<Rack> {
        use super::resample::{self, Stream};
        let internal = INTERNAL_SAMPLE_RATE as u32;
        let up_c = resample::Coeffs::for_rates(rate, internal)?;
        let down_c = resample::Coeffs::for_rates(internal, rate)?;

        // 1 チャンク（入力）あたりの 48 kHz サンプル数と、その出力数。
        let n48 = up_c.max_out(resample::MAX_BLOCK).max(1);
        let nout = down_c.max_out(n48).max(1);
        let carry_cap = nout.min(4096).max(64);

        let mut up = Vec::with_capacity(num_channels);
        let mut down = Vec::with_capacity(num_channels);
        let mut in48 = Vec::with_capacity(num_channels);
        let mut tail = Vec::with_capacity(num_channels);
        let mut carry = Vec::with_capacity(num_channels);
        for _ in 0..num_channels {
            up.push(Stream::new(Arc::clone(&up_c)));
            down.push(Stream::new(Arc::clone(&down_c)));
            in48.push(vec![0.0; n48]);
            tail.push(vec![0.0; nout]);
            carry.push(vec![0.0; carry_cap]);
        }

        let added_ms = (up_c.delay_in_samples() / f64::from(rate)
            + down_c.delay_in_samples() / f64::from(internal))
            * 1000.0;

        Some(Rack {
            up,
            down,
            in48,
            tail,
            carry,
            carry_len: 0,
            starved: 0,
            added_ms: added_ms as f32,
        })
    }

    fn reset(&mut self) {
        for s in self.up.iter_mut() {
            s.reset();
        }
        for s in self.down.iter_mut() {
            s.reset();
        }
        self.carry_len = 0;
    }

    /// 1 チャンクぶん変換する。`buffers` は OBS レートの入出力（同じ領域、
    /// `offset` から `n` サンプル）。`core` は 48 kHz の本体処理（in-place）。
    fn step(
        &mut self,
        buffers: &[*mut f32],
        offset: usize,
        n: usize,
        num_ch: usize,
        core: &mut dyn FnMut(&[*mut f32], usize),
    ) {
        // 1) OBS レート → 48 kHz
        let mut k = usize::MAX;
        for ch in 0..num_ch {
            // SAFETY: 呼び出し側が `offset + n` サンプル以上の有効な領域を渡す。
            let src = unsafe { std::slice::from_raw_parts(buffers[ch].add(offset), n) };
            self.up[ch].push(src);
            let got = self.up[ch].pull(&mut self.in48[ch]);
            k = k.min(got);
        }
        let k = if k == usize::MAX { 0 } else { k };

        // 2) 48 kHz の本体（in-place）
        let mut ptrs = [std::ptr::null_mut::<f32>(); 2];
        for (ch, p) in ptrs.iter_mut().enumerate().take(num_ch) {
            *p = self.in48[ch].as_mut_ptr();
        }
        core(&ptrs[..num_ch], k);

        // 3) 48 kHz → OBS レート
        //    push/pull は `MAX_BLOCK` 単位で行う（履歴保持の前提を守るため）。
        let mut k_out = 0usize;
        let mut pos = 0usize;
        while pos < k {
            let m = (k - pos).min(super::resample::MAX_BLOCK);
            for ch in 0..num_ch {
                self.down[ch].push(&self.in48[ch][pos..pos + m]);
            }
            let mut got = 0usize;
            for ch in 0..num_ch {
                got = self.down[ch].pull(&mut self.tail[ch][k_out..]);
            }
            k_out += got;
            pos += m;
        }

        // 4) 入力と同じ数だけ書き出す（端数は持ち越す）。
        let avail = self.carry_len + k_out;
        let take = avail.min(n);
        let c1 = self.carry_len.min(take);
        let c2 = take - c1;
        let left = avail - take;
        let overflow = left > self.carry.first().map_or(0, Vec::len);
        for ch in 0..num_ch {
            // SAFETY: 1) と同じ前提。
            let out = unsafe { std::slice::from_raw_parts_mut(buffers[ch].add(offset), n) };
            out[..c1].copy_from_slice(&self.carry[ch][..c1]);
            out[c1..take].copy_from_slice(&self.tail[ch][..c2]);
            if take < n {
                out[take..].iter_mut().for_each(|v| *v = 0.0);
            }
            if !overflow && left > 0 {
                // 旧 carry の残りを先頭へ寄せ、そのうしろに tail の残りを繋ぐ。
                let rem = self.carry_len - c1;
                self.carry[ch].copy_within(c1..self.carry_len, 0);
                self.carry[ch][rem..left].copy_from_slice(&self.tail[ch][c2..c2 + left - rem]);
            }
        }
        if take < n {
            self.starved += (n - take) as u64;
        }
        self.carry_len = if overflow { 0 } else { left };
    }
}

pub struct Engine {
    channels: Vec<Channel>,

    hop: usize,
    freq_bins: usize,

    in_fifo: Vec<Vec<f32>>,
    out_fifo: Vec<Vec<f32>>,
    in_fill: usize,
    out_fill: usize,
    out_read: usize,

    smoothed_noisy_weight: f32,
    smoothed_reverb_blend: f32,
    voice_gain: f32,
    voice_hp_state: Vec<f32>,
    voice_hp_prev_x: Vec<f32>,
    voice_hp_coef: f32,

    meter_lp_a_coef: f32,
    meter_lp_b_coef: f32,
    meter_lp_a: [f32; 2],
    meter_lp_b: [f32; 2],
    meter_band_smoothed: [[f32; 3]; 2],
    /// 入力 RMS の平滑値（VU の針が暴れないように）。
    meter_in_smoothed: [f32; 2],
    /// 入力ピーク（ブロック内最大 + 短い戻り）。パネルのピークホールド用。
    meter_in_peak: [f32; 2],
    /// 出力ピーク（入力と同じ扱い）。OUT のピークホールド用。
    meter_out_peak: [f32; 2],
    /// 出力 RMS の平滑値（GR = 入力 - 出力 の材料）。
    meter_out_smoothed: [f32; 2],

    pub meters: Arc<Meters>,
    pub num_channels: usize,
    pub dereverb_available: bool,
    pub latency_samples: usize,
    /// 実際に使った推論デバイス（`Gpu` 指定で失敗したときは `Cpu` になる）。
    pub device: DeviceChoice,
    pub model_summary: String,
    /// OBS のレートが 48 kHz 以外のときだけ入るレート変換段。
    rack: Option<Box<Rack>>,
    /// レート変換で増える遅延 [ms]（48 kHz のときは 0）。
    pub added_latency_ms: f32,
}

/// 必要なチャンネル数ぶんの ONNX セッションを作る（1 チャンネル = 1 セッション）。
fn load_channels(
    num_channels: usize,
    model_path: &Path,
    ort_dirs: &[PathBuf],
    device: DeviceChoice,
) -> Result<Vec<DpdfNet>, String> {
    let mut channels = Vec::with_capacity(num_channels);
    for _ in 0..num_channels {
        channels.push(DpdfNet::load(model_path, 1, device, ort_dirs)?);
    }
    Ok(channels)
}

/// 音声スレッドで触る前に、UI スレッド（create / activate）で組み立てる。
///
/// `num_channels` は 1 (モノラル) か 2 (ステレオ)。OBS の音声設定に対応する。
/// `rate` は OBS が動いているサンプルレート。48 kHz 以外なら内部で 48 kHz へ
/// 変換してから処理する（モデルが 48 kHz 前提のため）。
///
/// `device` はユーザーの設定。`Gpu` で組み立てられないときは警告を出して
/// **CPU で組み直す**（音を止めるより、遅くても動く方を選ぶ）。
pub fn build(
    num_channels: usize,
    rate: u32,
    model_path: &Path,
    ort_dirs: &[PathBuf],
    device: DeviceChoice,
    meters: Arc<Meters>,
) -> Result<Engine, String> {
    let num_channels = num_channels.clamp(1, 2);

    // レート変換段（48 kHz 以外のみ）。作れないレートは呼び出し側で弾く。
    let rack = if rate == INTERNAL_SAMPLE_RATE as u32 {
        None
    } else {
        match Rack::new(rate, num_channels) {
            Some(r) => Some(Box::new(r)),
            None => return Err(format!("未対応のサンプルレート: {rate} Hz")),
        }
    };
    let added_latency_ms = rack.as_ref().map_or(0.0, |r| r.added_ms);

    let (channels, device_used) = match load_channels(num_channels, model_path, ort_dirs, device) {
        Ok(ch) => (ch, device),
        Err(e) if device == DeviceChoice::Gpu => {
            crate::obs_log!(
                crate::ffi::LOG_WARNING,
                "GPU (DirectML) でモデルを読み込めないため CPU に切り替えます: {}",
                e
            );
            (
                load_channels(num_channels, model_path, ort_dirs, DeviceChoice::Cpu)?,
                DeviceChoice::Cpu,
            )
        }
        Err(e) => return Err(e),
    };

    let meta = channels[0].meta.clone();
    let hop = meta.hop.max(0) as usize;
    let freq_bins = meta.freq_bins.max(0) as usize;

    if hop == 0 || freq_bins == 0 {
        return Err("モデルの hop / freq_bins が不正".to_string());
    }

    // リアルタイム余裕の診断: 予算 = 1 ホップぶんの実時間（480 @48k = 10 ms）。
    meters.set_dsp_budget_us((hop as u64 * 1_000_000 / INTERNAL_SAMPLE_RATE as u64).max(1) as u32);

    let mut chans: Vec<Channel> = Vec::with_capacity(num_channels);
    for engine in channels {
        let mut c = Channel::new(engine, hop, freq_bins);
        if !c.stft.prepare(meta.n_fft as usize, meta.win_len as usize, hop) {
            return Err(format!(
                "STFT 形状が DPDFNet の想定外 (n_fft={}, win_len={}, hop={})",
                meta.n_fft, meta.win_len, hop
            ));
        }
        chans.push(c);
    }

    let dereverb_available = chans[0].engine.has_dereverb_output();

    let two_pi = std::f32::consts::TAU;
    let sr = INTERNAL_SAMPLE_RATE;

    Ok(Engine {
        channels: chans,
        hop,
        freq_bins,
        in_fifo: vec![vec![0.0; hop]; num_channels],
        out_fifo: vec![vec![0.0; hop]; num_channels],
        in_fill: 0,
        out_fill: 0,
        out_read: 0,
        smoothed_noisy_weight: 0.0,
        smoothed_reverb_blend: 0.0,
        voice_gain: 1.0,
        voice_hp_state: vec![0.0; num_channels],
        voice_hp_prev_x: vec![0.0; num_channels],
        voice_hp_coef: (-two_pi * 1500.0 / sr).exp(),
        meter_lp_a_coef: (-two_pi * 300.0 / sr).exp(),
        meter_lp_b_coef: (-two_pi * 3000.0 / sr).exp(),
        meter_lp_a: [0.0; 2],
        meter_lp_b: [0.0; 2],
        meter_band_smoothed: [[0.0; 3]; 2],
        meter_in_smoothed: [0.0; 2],
        meter_in_peak: [0.0; 2],
        meter_out_peak: [0.0; 2],
        meter_out_smoothed: [0.0; 2],
        meters,
        num_channels,
        dereverb_available,
        latency_samples: K_LATENCY_HOPS * hop,
        device: device_used,
        rack,
        added_latency_ms,
        model_summary: format!(
            "{} | n_fft={} hop={} state={} outputs={}",
            model_path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            meta.n_fft,
            hop,
            meta.state_size,
            if dereverb_available { 2 } else { 1 }
        ),
    })
}

impl Engine {
    /// テスト用（オフライン参照の hop 数を取り出す）。
    #[cfg(test)]
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// タイムスタンプの不連続や再開時に呼ぶ。
    pub fn reset(&mut self) {
        for c in self.channels.iter_mut() {
            c.engine.reset_state();
            c.stft.reset();
            c.frame.iter_mut().for_each(|v| *v = 0.0);
            for r in c.frame_ring.iter_mut() {
                r.iter_mut().for_each(|v| *v = 0.0);
            }
            c.step = 0;
        }

        for f in self.in_fifo.iter_mut() {
            f.iter_mut().for_each(|v| *v = 0.0);
        }
        for f in self.out_fifo.iter_mut() {
            f.iter_mut().for_each(|v| *v = 0.0);
        }
        self.in_fill = 0;
        self.out_fill = 0;
        self.out_read = 0;

        self.voice_hp_state.iter_mut().for_each(|v| *v = 0.0);
        self.voice_hp_prev_x.iter_mut().for_each(|v| *v = 0.0);

        self.meter_lp_a = [0.0; 2];
        self.meter_lp_b = [0.0; 2];
        self.meter_band_smoothed = [[0.0; 3]; 2];
        self.meter_in_smoothed = [0.0; 2];
        self.meter_in_peak = [0.0; 2];
        self.meter_out_peak = [0.0; 2];
        self.meter_out_smoothed = [0.0; 2];

        if let Some(r) = self.rack.as_mut() {
            r.reset();
        }
    }

    /// その場で `buffers` を書き換える。長さは `num_channels` 以上、各要素は
    /// `frames` サンプル以上の有効な planar f32 バッファ。
    ///
    /// **アロケーションを行わない。**
    ///
    /// OBS のレートが 48 kHz のときは 1 サンプルも変換せずそのまま本体へ渡す
    /// （従来と同じ経路）。それ以外のときは 48 kHz へ変換 → 本体 → 元のレートへ
    /// 変換、という順に通す。出力サンプル数は入力と同じ。
    pub fn process(&mut self, buffers: &[*mut f32], frames: usize, p: ParamSnapshot) {
        if frames == 0 || self.channels.is_empty() || self.hop == 0 {
            return;
        }

        let num_ch = buffers.len().min(self.channels.len());
        if num_ch == 0 {
            return;
        }

        let Some(mut rack) = self.rack.take() else {
            self.process_48k(buffers, frames, p);
            return;
        };

        // ブロック長は OBS から任意に来るので、想定長で分割して処理する。
        // 途中でパニックしても変換段を必ず戻す（戻さないと、次のブロックから
        // 変換前の音を 48 kHz として扱ってしまう）。48 kHz のときは上で
        // return しているので、ここを通るのはレート変換を使うときだけ。
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let me: &mut Engine = &mut *self;
            let chunk = super::resample::MAX_BLOCK;
            let mut done = 0usize;
            while done < frames {
                let n = (frames - done).min(chunk);
                rack.step(buffers, done, n, num_ch, &mut |ptrs, k| {
                    me.process_48k(ptrs, k, p);
                });
                done += n;
            }
        }));

        self.rack = Some(rack);
        if let Err(e) = r {
            // 外側（`filter_audio`）がログを出してパススルーへ退避する。
            std::panic::resume_unwind(e);
        }
    }

    /// 48 kHz の本体処理（従来からの経路。ここは変換しない）。
    fn process_48k(&mut self, buffers: &[*mut f32], frames: usize, p: ParamSnapshot) {
        if frames == 0 || self.channels.is_empty() || self.hop == 0 {
            return;
        }

        let num_ch = buffers.len().min(self.channels.len());
        if num_ch == 0 {
            return;
        }

        let hop = self.hop;

        // 入力レベル（VU 用）は加工前の値で測る。この時点の `buffers` は
        // まだ入力のまま（出力は下のループで上書きしていく）。
        let mut in_sum = [0.0f64; 2];
        let mut in_abs = [0.0f32; 2];

        for i in 0..frames {
            for ch in 0..num_ch {
                // SAFETY: 呼び出し側が frames サンプル以上の有効なバッファを渡す。
                let x = unsafe { *buffers[ch].add(i) };
                if ch < 2 {
                    in_sum[ch] += (x as f64) * (x as f64);
                    in_abs[ch] = in_abs[ch].max(x.abs());
                }
                self.in_fifo[ch][self.in_fill] = x;
            }

            self.in_fill += 1;
            let hop_just_completed = self.in_fill == hop;

            if self.out_fill > 0 {
                for ch in 0..num_ch {
                    let v = self.out_fifo[ch][self.out_read];
                    // SAFETY: 同上。
                    unsafe {
                        *buffers[ch].add(i) = v;
                    }
                }

                self.out_fill -= 1;
                if self.out_fill == 0 {
                    self.out_read = 0;
                } else {
                    self.out_read += 1;
                }
            } else {
                for ch in 0..num_ch {
                    // SAFETY: 同上。
                    unsafe {
                        *buffers[ch].add(i) = 0.0;
                    }
                }
            }

            // run_hop() は out_fifo を上書きするので、必ず上の書き出しの後に呼ぶ。
            if hop_just_completed {
                // リアルタイムに間に合っているかを診断できるように、ホップの
                // 処理時間を控える（`Instant::now()` 2 回 = 数十 ns 程度）。
                let t0 = Instant::now();
                self.run_hop(p);
                self.meters
                    .record_dsp_time(t0.elapsed().as_micros().min(u32::MAX as u128) as u32);
                self.in_fill = 0;
                self.out_fill = hop;
                self.out_read = 0;
            }
        }

        let mut in_rms = [0.0f32; 2];
        if frames > 0 {
            for ch in 0..num_ch.min(2) {
                in_rms[ch] = (in_sum[ch] / frames as f64).sqrt() as f32;
            }
        }

        self.update_meters(buffers, frames, num_ch, in_rms, in_abs);
        self.meters.bump_seq();
    }

    fn run_hop(&mut self, p: ParamSnapshot) {
        // ---- パラメータの平滑化（1 ホップ = 10 ms ごと） ----
        // ミュート / バイパスは平滑化の**前**に効かせる。こうすれば切り替えた
        // ときも、つまみを回したときと同じ速さで滑らかに動く（クリックしない）。
        let ambience = p.ambience_mode.apply(
            p.ambience.clamp(0.0, 1.0),
            params::AMBIENCE_MIN as f32,
            params::AMBIENCE_NEUTRAL,
        );
        let reverb = p.voice_reverb_mode.apply(
            p.voice_reverb.clamp(0.0, 1.0),
            params::VOICE_REVERB_MIN as f32,
            params::VOICE_REVERB_NEUTRAL,
        );
        let voice_db = p
            .voice_mode
            .apply(p.voice_db, params::VOICE_MIN as f32, params::VOICE_NEUTRAL);

        let target_weight =
            params::attenuation_to_noisy_weight(params::ambience_to_attenuation_db(ambience));
        let k = params::SMOOTHING_K;
        self.smoothed_noisy_weight += k * (target_weight - self.smoothed_noisy_weight);
        self.smoothed_reverb_blend += k * (reverb - self.smoothed_reverb_blend);
        self.voice_gain = params::decibels_to_gain(voice_db);

        let alpha = self.smoothed_noisy_weight;
        let beta = self.smoothed_reverb_blend;
        let one_minus_beta = 1.0 - beta;

        let spec_count = 2 * self.freq_bins;
        let hop = self.hop;

        for ci in 0..self.channels.len() {
            let c: &mut Channel = &mut self.channels[ci];
            let slot = (c.step as usize) & (K_MODEL_LOOKAHEAD_HOPS - 1);

            // 1) 解析: hop 個の入力 -> 1 フレームの複素スペクトル
            c.stft.analyse(&self.in_fifo[ci], &mut c.frame);

            // 2) 推論（アロケーションなし）
            c.engine
                .process_frame(&c.frame, &mut c.out_full, &mut c.out_ns);

            // 3) スペクトル領域ブレンド
            //    beta : 残響を残す -> ns 経路（マスクのみ）
            //    alpha: 環境ノイズを残す -> 整列入力を混ぜる
            {
                let prev_noisy = &c.frame_ring[slot];
                for k in 0..spec_count {
                    let dere = one_minus_beta * c.out_ns[k] + beta * c.out_full[k];
                    c.blended[k] = (1.0 - alpha) * dere + alpha * prev_noisy[k];
                }
            }

            // 4) 合成。step 0/1 のモデル出力はプリヒストリ（入力前のゼロ）に
            //    対応するため合成しない（起動時の過渡を避けるため。遅延には効かない）。
            let got = if c.step >= 2 {
                c.stft.synthesise(&c.blended, &mut c.hop_out)
            } else {
                false
            };

            if got {
                let coef = self.voice_hp_coef;
                let voice_gain = self.voice_gain;
                let mut hp_state = self.voice_hp_state[ci];
                let mut hp_prev = self.voice_hp_prev_x[ci];

                {
                    let dst = &mut self.out_fifo[ci];
                    for m in 0..hop {
                        let x = c.hop_out[m];
                        let hp = coef * (hp_state + x - hp_prev);
                        hp_state = hp;
                        hp_prev = x;

                        // Voice = presence シェルフ（0 dB で完全なパススルー）
                        dst[m] = x + (voice_gain - 1.0) * hp;
                    }
                }

                self.voice_hp_state[ci] = hp_state;
                self.voice_hp_prev_x[ci] = hp_prev;
            } else {
                self.out_fifo[ci].iter_mut().for_each(|v| *v = 0.0);
            }

            // 5) 次ステップのために現在フレームをリングへ
            c.frame_ring[slot].copy_from_slice(&c.frame);
            c.step += 1;
        }
    }

    fn update_meters(
        &mut self,
        buffers: &[*mut f32],
        frames: usize,
        num_ch: usize,
        in_rms: [f32; 2],
        in_abs: [f32; 2],
    ) {
        if frames == 0 {
            return;
        }

        const DECAY: f32 = 0.35;
        // レベル（VU / GR）の戻り。1 ブロックごとに掛ける。
        const LEVEL_DECAY: f32 = 0.80;
        // 入力ピークの戻り。パネルのホールドと合わせて「ゆっくり落ちる」見た目に。
        const PEAK_DECAY: f32 = 0.85;

        let n = num_ch.min(2);

        for ch in 0..n {
            // 入力レベル（VU 用）: 加工前の値。立ち上がりは即時、戻りはゆっくり。
            let sm_in = &mut self.meter_in_smoothed[ch];
            *sm_in = in_rms[ch].max(*sm_in * LEVEL_DECAY);
            self.meters.set_in(ch, *sm_in);

            // 入力ピーク（パネルのピークホールド表示用）。
            let pk = &mut self.meter_in_peak[ch];
            *pk = in_abs[ch].max(*pk * PEAK_DECAY);
            self.meters.set_in_peak(ch, *pk);

            // SAFETY: process() と同じ前提。書き換え後の値（= 出力）を読む。
            let p = unsafe { std::slice::from_raw_parts(buffers[ch], frames) };

            let mut s_a = self.meter_lp_a[ch];
            let mut s_b = self.meter_lp_b[ch];
            let inv_a = 1.0 - self.meter_lp_a_coef;
            let inv_b = 1.0 - self.meter_lp_b_coef;

            let mut sum = [0.0f64; 3];
            let mut sum_out = 0.0f64;
            let mut abs_out = 0.0f32;

            for &x in p {
                s_a += inv_a * (x - s_a);
                s_b += inv_b * (x - s_b);

                let low = s_a;
                let mid = s_b - s_a;
                let high = x - s_b;

                sum[0] += (low as f64) * (low as f64);
                sum[1] += (mid as f64) * (mid as f64);
                sum[2] += (high as f64) * (high as f64);
                sum_out += (x as f64) * (x as f64);
                abs_out = abs_out.max(x.abs());
            }

            self.meter_lp_a[ch] = s_a;
            self.meter_lp_b[ch] = s_b;

            // 出力レベル（GR = 入力 - 出力 の材料）。
            let out_rms = (sum_out / frames as f64).sqrt() as f32;
            let sm_out = &mut self.meter_out_smoothed[ch];
            *sm_out = out_rms.max(*sm_out * LEVEL_DECAY);
            self.meters.set_out(ch, *sm_out);

            // 出力ピーク（OUT のピークホールド表示用）。入力と同じ戻り。
            let pk_out = &mut self.meter_out_peak[ch];
            *pk_out = abs_out.max(*pk_out * PEAK_DECAY);
            self.meters.set_out_peak(ch, *pk_out);

            for b in 0..3 {
                let rms = (sum[b] / frames as f64).sqrt() as f32;
                let sm = &mut self.meter_band_smoothed[ch][b];
                *sm = rms.max(*sm * (1.0 - DECAY));
                let v = *sm;
                self.meters.set(ch, b, v);
            }
        }
    }

    /// テスト専用の内部状態スナップショット。
    /// `(hop入力rms, frame rms, out_full rms, out_ns rms, blended rms, ring[0] rms, alpha, step)`
    #[cfg(test)]
    pub fn debug_probe(&self) -> (f32, f32, f32, f32, f32, f32, f32, u64) {
        fn r(v: &[f32]) -> f32 {
            if v.is_empty() {
                return 0.0;
            }
            let s: f64 = v.iter().map(|x| (*x as f64) * (*x as f64)).sum();
            (s / v.len() as f64).sqrt() as f32
        }

        let c = &self.channels[0];
        (
            r(&self.in_fifo[0]),
            r(&c.frame),
            r(&c.out_full),
            r(&c.out_ns),
            r(&c.blended),
            r(&c.frame_ring[0]),
            self.smoothed_noisy_weight,
            c.step,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 負荷の計測が「予算超過だけ」を数えること。
    #[test]
    fn meters_records_dsp_time() {
        let m = Meters::new();
        // 予算が未設定（= エンジン未構築）のうちは超過を数えない。
        m.record_dsp_time(999_999);
        assert_eq!(m.dsp_stats().over, 0);

        m.set_dsp_budget_us(10_000);
        m.record_dsp_time(2_000);
        m.record_dsp_time(12_000); // 予算超過
        m.record_dsp_time(3_000);

        let s = m.dsp_stats();
        assert_eq!(s.hops, 4);
        assert_eq!(s.over, 1);
        assert_eq!(s.sum_us, 999_999 + 2_000 + 12_000 + 3_000);
        assert_eq!(s.max_us, 999_999);
        assert_eq!(s.budget_us, 10_000);
    }

    /// レート変換段そのものの確認（モデルも ONNX Runtime も要らない。
    /// `core` は素通しのダミー）。
    ///
    /// 見ること:
    ///   * 出力サンプル数が入力とまったく同じ（OBS の要求）
    ///   * ゼロ埋め（starved）は起動直後だけで止まる
    ///   * 定常では直流がそのまま通る（振幅・遅延の辻褄）
    ///   * 左右のチャンネルが完全に一致する（チャンネル間の状態漏れなし）
    #[test]
    fn rack_keeps_sample_counts_and_level() {
        for rate in [22_050u32, 44_100, 88_200, 96_000, 192_000] {
            for block in [1usize, 7, 480, 1023, 1024, 4096] {
                let mut r = Rack::new(rate, 2).expect("変換段が作れない");
                let n_blocks = (120_000usize / block).max(8);
                let mut buf = [vec![0.0f32; block], vec![0.0f32; block]];
                let mut first_starved = 0u64;

                for i in 0..n_blocks {
                    for b in buf.iter_mut() {
                        b.iter_mut().for_each(|v| *v = 0.35);
                    }
                    let ptrs: [*mut f32; 2] = [buf[0].as_mut_ptr(), buf[1].as_mut_ptr()];
                    r.step(&ptrs, 0, block, 2, &mut |_, k| assert!(k < 200_000));
                    if i == 0 {
                        first_starved = r.starved;
                    } else {
                        assert_eq!(
                            r.starved, first_starved,
                            "rate={rate} block={block}: 途中でゼロ埋めが再発 (i={i})"
                        );
                    }
                }

                // 起動直後のゼロ埋めは高々 2 ブロックぶん。
                assert!(
                    first_starved <= 2 * block as u64,
                    "rate={rate} block={block}: 起動時のゼロ埋めが多すぎる ({first_starved})"
                );
                // 定常: 直流がそのまま出る（変換の誤差は 1% 未満）。
                for v in buf[0].iter() {
                    assert!(
                        (v - 0.35).abs() < 2e-3,
                        "rate={rate} block={block}: 定常値が違う ({v})"
                    );
                }
                assert_eq!(buf[0], buf[1], "rate={rate} block={block}: L/R が違う");
            }
        }
    }
}
