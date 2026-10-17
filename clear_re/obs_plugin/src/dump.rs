//! 実機検証用のダンプ（開発用）。
//!
//! `DPDFNET_DUMP=<prefix>` を設定して OBS を起動すると、フィルタに入ってきた音と
//! 出ていく音を `<prefix>_in.wav` / `<prefix>_out.wav` に書き出す。
//!
//! 目的は「OBS の中で本当に DSP が通っているか」「遅延が 2880 サンプルか」を
//! **録画や ffmpeg 無しで**数値確認すること:
//!
//! * `_in` と `_out` の相互相関 → 遅延サンプル数
//! * `_in` をオフライン参照実装（Python）に通した結果と `_out` を比較 → 一致度
//! * `_in` / `_out` の低活動区間の減衰量 → 抑圧効果
//!
//! 環境変数が無ければ `Dump` は作られないので、本番のオーディオスレッドには
//! 一切影響しない（ミューテックスもファイルも増えない）。書き出しは 32 bit
//! float の WAV（OBS 内部と同じ形式）なので、読み出し側もそのまま扱える。

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// 32 bit float WAV のヘッダ長。
const HEADER_LEN: u64 = 44;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

pub struct Dump {
    prefix: PathBuf,
    sample_rate: u32,
    in_w: Option<WavWriter>,
    out_w: Option<WavWriter>,
    path_in: PathBuf,
    path_out: PathBuf,
    channels: usize,
    frames: u64,
    /// インターリーブ用の作業バッファ（毎ブロック使い回す）。
    interleaved: Vec<f32>,
    /// 一度でも失敗したら止める。
    done: bool,
}

impl Dump {
    /// `DPDFNET_DUMP` が設定されていればダンプを用意する。
    ///
    /// ファイルは最初のブロックでチャンネル数が確定してから作る。
    pub fn from_env(sample_rate: u32) -> Option<Dump> {
        let v = std::env::var_os("DPDFNET_DUMP")?;
        if v.is_empty() {
            return None;
        }
        Some(Dump {
            prefix: PathBuf::from(v),
            sample_rate,
            in_w: None,
            out_w: None,
            path_in: PathBuf::new(),
            path_out: PathBuf::new(),
            channels: 0,
            frames: 0,
            interleaved: Vec::new(),
            done: false,
        })
    }

    fn path_for(&self, suffix: &str) -> PathBuf {
        let dir = self.prefix.parent().filter(|p| !p.as_os_str().is_empty());
        let name = match self.prefix.file_name() {
            Some(n) => format!("{}{suffix}.wav", n.to_string_lossy()),
            None => format!("dpdfnet{suffix}.wav"),
        };
        match dir {
            Some(d) => d.join(name),
            None => PathBuf::from(name),
        }
    }

    /// 最初のブロックでチャンネル数が確定したときに 1 度だけ呼ぶ。
    fn open(&mut self, channels: usize) -> bool {
        self.channels = channels;
        self.path_in = self.path_for("_in");
        self.path_out = self.path_for("_out");

        let a = WavWriter::create(&self.path_in, self.sample_rate, channels as u16);
        let b = WavWriter::create(&self.path_out, self.sample_rate, channels as u16);
        match (a, b) {
            (Ok(a), Ok(b)) => {
                self.in_w = Some(a);
                self.out_w = Some(b);
                crate::obs_log!(
                    crate::ffi::LOG_INFO,
                    "dump: {} / {} に書き出します ({} Hz, {} ch)",
                    self.path_in.display(),
                    self.path_out.display(),
                    self.sample_rate,
                    channels
                );
                true
            }
            (a, b) => {
                let e = a.err().or(b.err()).map(|e| e.to_string()).unwrap_or_default();
                crate::obs_log!(crate::ffi::LOG_WARNING, "dump: ファイルを作れません: {e}");
                self.done = true;
                false
            }
        }
    }

    /// プレーンをインターリーブして `self.interleaved` に詰める。
    /// 書き出したフレーム数を返す（0 なら何もしない）。
    fn gather(&mut self, planes: &[*mut f32], frames: usize) -> usize {
        if self.done || planes.is_empty() {
            return 0;
        }
        if self.channels == 0 && !self.open(planes.len()) {
            return 0;
        }
        if planes.len() != self.channels {
            // 途中でチャンネル数が変わった（OBS 側の構成変更）。追えないので止める。
            self.done = true;
            return 0;
        }
        let need = frames * self.channels;
        if self.interleaved.len() < need {
            self.interleaved.resize(need, 0.0);
        }
        for (ch, plane) in planes.iter().enumerate() {
            // SAFETY: OBS が渡してくるプレーンは frames サンプル有効。
            let src = unsafe { std::slice::from_raw_parts(*plane, frames) };
            for (i, v) in src.iter().enumerate() {
                self.interleaved[i * self.channels + ch] = *v;
            }
        }
        need
    }

    /// フィルタ前の音を控える（`process` はその場で書き換えるため）。
    pub fn capture_input(&mut self, planes: &[*mut f32], frames: usize) {
        let need = self.gather(planes, frames);
        if need == 0 {
            return;
        }
        // 借用が衝突しないよう、一時的に取り出して書き戻す。
        let data = std::mem::take(&mut self.interleaved);
        let res = match self.in_w.as_mut() {
            Some(w) => w.write_f32(&data[..need]),
            None => Ok(()),
        };
        self.interleaved = data;
        if let Err(e) = res {
            crate::obs_log!(
                crate::ffi::LOG_WARNING,
                "dump: {} に書けません: {e}",
                self.path_in.display()
            );
            self.done = true;
        }
    }

    /// フィルタ後の音を書き出す。
    pub fn write_output(&mut self, planes: &[*mut f32], frames: usize) {
        if self.in_w.is_none() {
            return;
        }
        let need = self.gather(planes, frames);
        if need == 0 {
            return;
        }
        let data = std::mem::take(&mut self.interleaved);
        let res = match self.out_w.as_mut() {
            Some(w) => w.write_f32(&data[..need]),
            None => Ok(()),
        };
        self.interleaved = data;
        if let Err(e) = res {
            crate::obs_log!(
                crate::ffi::LOG_WARNING,
                "dump: {} に書けません: {e}",
                self.path_out.display()
            );
            self.done = true;
            return;
        }
        self.frames += frames as u64;
    }

    /// ヘッダを書き戻して閉じる（`destroy` から呼ぶ）。
    pub fn finish(&mut self) {
        if self.in_w.is_none() {
            return;
        }
        if let Some(w) = self.in_w.as_mut() {
            if let Err(e) = w.finish() {
                crate::obs_log!(crate::ffi::LOG_WARNING, "dump: in を閉じられません: {e}");
            }
        }
        if let Some(w) = self.out_w.as_mut() {
            if let Err(e) = w.finish() {
                crate::obs_log!(crate::ffi::LOG_WARNING, "dump: out を閉じられません: {e}");
            }
        }
        crate::obs_log!(
            crate::ffi::LOG_INFO,
            "dump: {} フレーム書き出しました ({})",
            self.frames,
            self.path_out.display()
        );
    }
}

/// 最小限の 32 bit float WAV ライタ（RIFF サイズは finish() で書き戻す）。
struct WavWriter {
    file: BufWriter<File>,
    bytes: Vec<u8>,
    sample_rate: u32,
    channels: u16,
    data_bytes: u64,
}

impl WavWriter {
    fn create(path: &Path, sample_rate: u32, channels: u16) -> std::io::Result<WavWriter> {
        // サイズ 0 のヘッダを**先に**書いて即座に流す。OBS を強制終了して
        // `finish()` が走らなくても、44 バイトのヘッダがゼロ埋めのまま残ることは
        // 無くなる（読む側はファイル長からデータ長を決めればよい）。
        let block_align = channels * 4;
        let mut file = File::create(path)?;
        file.write_all(&wav_header(
            36,
            0,
            sample_rate,
            channels,
            block_align,
            sample_rate * u32::from(block_align),
        ))?;
        file.flush()?;
        Ok(WavWriter {
            file: BufWriter::new(file),
            bytes: Vec::new(),
            sample_rate,
            channels,
            data_bytes: 0,
        })
    }

    fn write_f32(&mut self, data: &[f32]) -> std::io::Result<()> {
        self.bytes.clear();
        self.bytes.reserve(data.len() * 4);
        for v in data {
            self.bytes.extend_from_slice(&v.to_le_bytes());
        }
        self.file.write_all(&self.bytes)?;
        self.data_bytes += self.bytes.len() as u64;
        Ok(())
    }

    fn finish(&mut self) -> std::io::Result<()> {
        let data = self.data_bytes;
        let ch = self.channels;
        let block_align = ch * 4;
        let byte_rate = self.sample_rate * u32::from(block_align);
        self.file.flush()?;
        let f = self.file.get_mut();
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&wav_header(
            HEADER_LEN - 8 + data,
            data,
            self.sample_rate,
            ch,
            block_align,
            byte_rate,
        ))?;
        f.seek(SeekFrom::Start(HEADER_LEN + data))?;
        self.file.flush()
    }
}

/// WAV ヘッダを組み立てる（すべてリトルエンディアン）。
fn wav_header(
    riff_size: u64,
    data_size: u64,
    sample_rate: u32,
    channels: u16,
    block_align: u16,
    byte_rate: u32,
) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER_LEN as usize);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(riff_size as u32).to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&WAVE_FORMAT_IEEE_FLOAT.to_le_bytes());
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&block_align.to_le_bytes());
    h.extend_from_slice(&32u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&(data_size as u32).to_le_bytes());
    debug_assert_eq!(h.len() as u64, HEADER_LEN);
    h
}
