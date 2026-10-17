//! ホップスケジューラとチャンネルごとの DSP。
//!
//! 移植元: `PluginProcessor::processBlock` / `runHop` / `updateMeters`。
//!
//! 48 kHz 固定。`hop = 480`、内容遅延は 6 hop = 2880 サンプル (60 ms)。
//! この遅延量は移植元の実測値 (`ANALYSIS.md` §13) と同じで、変えてはいけない。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use super::net::DpdfNet;
use super::params;
use super::stft::VorbisStft;
use super::INTERNAL_SAMPLE_RATE;

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
}

impl Default for ParamSnapshot {
    fn default() -> Self {
        Self {
            ambience: params::AMBIENCE_DEFAULT as f32,
            voice_db: params::VOICE_DEFAULT as f32,
            voice_reverb: params::VOICE_REVERB_DEFAULT as f32,
        }
    }
}

/// レベルメータ。オーディオスレッドが書き、UI スレッドが読む。
///
/// すべてリニア RMS（フルスケール = 1.0）。UI 側で dB / VU / GR に変換する。
#[derive(Debug)]
pub struct Meters {
    /// 入力（加工前）の RMS [ch]。パネルのアナログ VU 用。
    in_rms: [AtomicU32; 2],
    /// 出力（加工後）の RMS [ch]。GR メーター用。
    out_rms: [AtomicU32; 2],
    /// 出力の 3 バンド（低 / 中 / 高）RMS [ch][band]。
    levels: [[AtomicU32; 3]; 2],
}

impl Meters {
    pub fn new() -> Self {
        Self {
            in_rms: [AtomicU32::new(0), AtomicU32::new(0)],
            out_rms: [AtomicU32::new(0), AtomicU32::new(0)],
            levels: [
                [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)],
                [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)],
            ],
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

    /// 出力（加工後）RMS [ch]。
    #[inline]
    pub fn out_level(&self, ch: usize) -> f32 {
        f32::from_bits(self.out_rms[ch.min(1)].load(Ordering::Relaxed))
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
    pub(crate) fn set_out(&self, ch: usize, v: f32) {
        self.out_rms[ch.min(1)].store(v.to_bits(), Ordering::Relaxed);
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
    /// 出力 RMS の平滑値（GR = 入力 - 出力 の材料）。
    meter_out_smoothed: [f32; 2],

    pub meters: Arc<Meters>,
    pub num_channels: usize,
    pub dereverb_available: bool,
    pub latency_samples: usize,
    pub model_summary: String,
}

/// 音声スレッドで触る前に、UI スレッド（create / activate）で組み立てる。
///
/// `num_channels` は 1 (モノラル) か 2 (ステレオ)。OBS の音声設定に対応する。
pub fn build(
    num_channels: usize,
    model_path: &Path,
    ort_dirs: &[PathBuf],
    meters: Arc<Meters>,
) -> Result<Engine, String> {
    let num_channels = num_channels.clamp(1, 2);

    let mut channels = Vec::with_capacity(num_channels);
    for _ in 0..num_channels {
        let engine = DpdfNet::load(model_path, 1, ort_dirs)?;
        channels.push(engine);
    }

    let meta = channels[0].meta.clone();
    let hop = meta.hop.max(0) as usize;
    let freq_bins = meta.freq_bins.max(0) as usize;

    if hop == 0 || freq_bins == 0 {
        return Err("モデルの hop / freq_bins が不正".to_string());
    }

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
        meter_out_smoothed: [0.0; 2],
        meters,
        num_channels,
        dereverb_available,
        latency_samples: K_LATENCY_HOPS * hop,
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
        self.meter_out_smoothed = [0.0; 2];
    }

    /// その場で `buffers` を書き換える。長さは `num_channels` 以上、各要素は
    /// `frames` サンプル以上の有効な planar f32 バッファ。
    ///
    /// **アロケーションを行わない。**
    pub fn process(&mut self, buffers: &[*mut f32], frames: usize, p: ParamSnapshot) {
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

        for i in 0..frames {
            for ch in 0..num_ch {
                // SAFETY: 呼び出し側が frames サンプル以上の有効なバッファを渡す。
                let x = unsafe { *buffers[ch].add(i) };
                if ch < 2 {
                    in_sum[ch] += (x as f64) * (x as f64);
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
                self.run_hop(p);
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

        self.update_meters(buffers, frames, num_ch, in_rms);
    }

    fn run_hop(&mut self, p: ParamSnapshot) {
        // ---- パラメータの平滑化（1 ホップ = 10 ms ごと） ----
        let target_weight =
            params::attenuation_to_noisy_weight(params::ambience_to_attenuation_db(p.ambience));
        let k = params::SMOOTHING_K;
        self.smoothed_noisy_weight += k * (target_weight - self.smoothed_noisy_weight);
        self.smoothed_reverb_blend +=
            k * (p.voice_reverb.clamp(0.0, 1.0) - self.smoothed_reverb_blend);
        self.voice_gain = params::decibels_to_gain(p.voice_db);

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
    ) {
        if frames == 0 {
            return;
        }

        const DECAY: f32 = 0.35;
        // レベル（VU / GR）の戻り。1 ブロックごとに掛ける。
        const LEVEL_DECAY: f32 = 0.80;

        let n = num_ch.min(2);

        for ch in 0..n {
            // 入力レベル（VU 用）: 加工前の値。立ち上がりは即時、戻りはゆっくり。
            let sm_in = &mut self.meter_in_smoothed[ch];
            *sm_in = in_rms[ch].max(*sm_in * LEVEL_DECAY);
            self.meters.set_in(ch, *sm_in);

            // SAFETY: process() と同じ前提。書き換え後の値（= 出力）を読む。
            let p = unsafe { std::slice::from_raw_parts(buffers[ch], frames) };

            let mut s_a = self.meter_lp_a[ch];
            let mut s_b = self.meter_lp_b[ch];
            let inv_a = 1.0 - self.meter_lp_a_coef;
            let inv_b = 1.0 - self.meter_lp_b_coef;

            let mut sum = [0.0f64; 3];
            let mut sum_out = 0.0f64;

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
            }

            self.meter_lp_a[ch] = s_a;
            self.meter_lp_b[ch] = s_b;

            // 出力レベル（GR = 入力 - 出力 の材料）。
            let out_rms = (sum_out / frames as f64).sqrt() as f32;
            let sm_out = &mut self.meter_out_smoothed[ch];
            *sm_out = out_rms.max(*sm_out * LEVEL_DECAY);
            self.meters.set_out(ch, *sm_out);

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
