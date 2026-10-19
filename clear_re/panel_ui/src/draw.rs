//! パネルの描画（GDI + DIB）。
//!
//! 見た目は OBS 版から**数値も含めてそのまま**移設したもの: 黒アルマイトの
//! 面板 + アンバーに灯る VU 盤面 + 彫り込み文字の業務用オーディオ機器風。
//!
//!   * ヘッダ（タイトル + サブタイトル + メモリー 1〜8 + SAVE）
//!   * IN / OUT: 入力（加工前）と出力（加工後）のアナログ VU メーター
//!               （針 + VU 目盛り）+ 現在値 / ピークの数値窓。ステレオは
//!               L/R の指標を 1 本にまとめる（`crate::meter` 参照）
//!   * CONTROL : 縦フェーダー 3 本（AMBIENCE / VOICE / REVERB）+ dB / % の併記、
//!               各フェーダーの下に MUTE / BYP キー（つまみの位置は保つ）
//!   * EFFECT  : GR メーター（コンプの GR と同じく 0 から下へ振れる）+ 出力 3 バンド
//!   * ステータス LED + テキスト、有効 / 無効のトグル
//!
//! 質感の作り方（参考写真から拾った要素）:
//!
//!   * 面板 = 縦のヘアライン + ざらつき + 面取りの縁 + 四隅の十字ネジ
//!   * VU   = 上から暖色に灯る盤面（光源のにじみ）、太い黒の弧、0 〜 +3 の赤帯、
//!            2 本目の細い弧（50 / 100 %）、ガラスの斜めの反射、細い黒針
//!
//! 描画は「DIB セクションに描いて `BitBlt`」方式。グラデーション・質感は
//! ピクセルを直接書き、枠・文字・針は GDI に任せる。
//!
//! ## ホストによる違い
//!
//! OBS 版にはモデル / デバイスのキーが無い（プロパティ画面で選ぶ）。VST3 版は
//! プロパティ画面が無いので、フッタの右（BYPASS の左）に MODEL / DEV の
//! キーを出す。`Config.model_key` / `Config.device_key` で切り替える。
//! **OBS 版では矩形の計算だけして描かない**ので、レイアウトは 1 px も変わらない。

use core::ffi::c_void;
use core::ptr;

use dpdfnet::params as dsp;
use dpdfnet::paths::ModelChoice;

use crate::ffi::*;
use crate::meter::{self, Reading};
use crate::preset::SLOTS;

// --------------------------------------------------------------------------
// パレット: 黒アルマイト + アンバーに灯る VU + 彫り込み文字
// --------------------------------------------------------------------------

/// パネル全体（アルミの地）の上下端。
const PANEL_TOP: u32 = 0x23262a;
const PANEL_BOT: u32 = 0x16181b;
/// ウィンドウ外周の地（最外周 1 px）。
const BG: u32 = 0x0b0c0e;
/// 面取り（上左 = 光 / 下右 = 影）。
const BEVEL_HI: u32 = 0x3c4249;
const BEVEL_LO: u32 = 0x06070a;
/// 彫り込み文字の「影」（文字の下に入る暗い線）。
const ENGRAVE_HI: u32 = 0x07080a;
/// シルク印刷（静的なラベル）。
const SILK: u32 = 0xb6bcc3;
const SILK_DIM: u32 = 0x818a93;
const SILK_FAINT: u32 = 0x565d65;
/// 数値表示（動的な値）。
const VALUE: u32 = 0xdfe5ea;
const VALUE_DIM: u32 = 0x9aa3ac;
/// VU 盤面。実機と同じくランプで後ろから照らした「暖かいアイボリー」。
/// 上ほどアンバー、下へ行くほど淡いクリームになる。
const VU_FACE_TOP: u32 = 0xf6dda6;
const VU_FACE_BOT: u32 = 0xe4d9bd;
/// 盤面ににじむ光源（バックライトのムラ）。
const VU_HOT: u32 = 0xffc964;
/// 盤面のインク（目盛り・文字）。
const VU_INK: u32 = 0x1b1914;
const VU_INK_DIM: u32 = 0x6d6656;
/// 0 VU から上の赤（オーバーロード帯）。
const VU_RED: u32 = 0xb02c1f;
/// VU の針（実機は細い黒針。先端だけわずかに赤い）。
const NEEDLE: u32 = 0x14130f;
const NEEDLE_TIP: u32 = 0x8e2f26;
/// 赤（BYPASS / LED など）。
const RED: u32 = 0xa9362a;
/// アクセント（フェーダーの値 / 3 バンドの低域側）。
const ACCENT: u32 = 0x4bbfae;
const ACCENT_HI: u32 = 0x8ce9d8;
/// アンバー（GR / LED）。
const WARM: u32 = 0xe0a24b;
const WARM_HI: u32 = 0xf7d79b;
/// LED の色。
const LED_GREEN: u32 = 0x5fd07d;
const LED_RED: u32 = 0xd2453a;

// --------------------------------------------------------------------------
// レイアウト（論理ピクセル）
// --------------------------------------------------------------------------

/// クライアント領域の既定サイズ。単独ウィンドウはリサイズもできる（最小は下の 2 つ）。
pub(crate) const CLIENT_W: f32 = 780.0;
pub(crate) const CLIENT_H: f32 = 400.0;
pub(crate) const MIN_W: f32 = 700.0;
pub(crate) const MIN_H: f32 = 360.0;

const K_MARGIN: f32 = 24.0;
const K_GAP: f32 = 10.0;
const K_HEADER_H: f32 = 46.0;
const K_FOOTER_H: f32 = 34.0;
const K_SECTION_TITLE_H: f32 = 16.0;
const K_SECTION_GAP: f32 = 16.0;

/// 左（入力 VU）セクションと、真ん中（フェーダー）セクションの幅。
/// 残りはすべて EFFECT（GR + 3 バンド）に渡す。
const K_INPUT_W: f32 = 232.0;
const K_CONTROL_W: f32 = 224.0;

/// 入力 VU 2 本の縦間隔と、数値欄の幅。
const K_VU_GAP: f32 = 8.0;
const K_VU_NUM_W: f32 = 58.0;

/// フェーダー 1 本ぶん。
const K_FADER_LABEL_H: f32 = 15.0;
const K_FADER_VALUE_H: f32 = 15.0;
const K_FADER_TRACK_W: f32 = 9.0;
const K_FADER_HANDLE_W: f32 = 30.0;
pub(crate) const K_FADER_HANDLE_H: f32 = 14.0;
/// MUTE / BYP キーの高さ（フェーダーの上に 1 段）。
const K_FADER_BTN_H: f32 = 15.0;

/// GR メーター（縦・0 から下へ振れるコンプの GR 風）。
const K_GR_BAR_W: f32 = 20.0;
const K_GR_SCALE_W: f32 = 28.0;
const K_GR_NUM_W: f32 = 56.0;

/// 3 バンド（低 / 中 / 高）バー。
const K_BAND_W: f32 = 14.0;
const K_BAND_GAP: f32 = 8.0;
const K_BAND_LABEL_H: f32 = 14.0;

/// メモリー（スロット 8 個 + SAVE）。
const K_SLOT_W: f32 = 24.0;
const K_SLOT_H: f32 = 20.0;
const K_SLOT_GAP: f32 = 4.0;
const K_SAVE_W: f32 = 48.0;
const K_MEMORY_LABEL_W: f32 = 56.0;

const K_BUTTON_W: f32 = 96.0;
const K_BUTTON_H: f32 = 24.0;

/// フッタの MODEL / DEV キー 1 個ぶんの幅（VST3 版のみ）。
const K_FOOTER_KEY_W: f32 = 76.0;

/// ネジ（四隅）。
const K_SCREW_R: f32 = 5.0;
const K_SCREW_INSET: f32 = 12.0;

/// VU の目盛り: 0 VU を基準に -20 VU 〜 +3 VU を出す（基準値で平行移動）。
const VU_VU_AT_MIN: f32 = -20.0;
const VU_VU_AT_MAX: f32 = 3.0;
const VU_ANGLE_MIN: f32 = -55.0;
const VU_ANGLE_MAX: f32 = 55.0;
/// 針の支点を盤面の下端からどれだけ上げるか。
const VU_PIVOT_LIFT: f32 = 5.0;

/// GR メーターの振れ幅（0 〜 -30 dB）。
const GR_DB_RANGE: f32 = 30.0;

// --------------------------------------------------------------------------
// 小物
// --------------------------------------------------------------------------

/// 整数の矩形（レイアウト計算と描画で共用する）。
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct IR {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl IR {
    pub(crate) fn w(&self) -> i32 {
        self.r - self.l
    }
    pub(crate) fn h(&self) -> i32 {
        self.b - self.t
    }
    fn cx(&self) -> i32 {
        (self.l + self.r) / 2
    }
    fn cy(&self) -> i32 {
        (self.t + self.b) / 2
    }
    fn top(&self, h: i32) -> IR {
        IR { b: self.t + h, ..*self }
    }
    fn bottom(&self, h: i32) -> IR {
        IR { t: self.b - h, ..*self }
    }
    /// 左から `w` ぶん切り出す。
    fn left(&self, w: i32) -> IR {
        IR { r: self.l + w, ..*self }
    }
    /// 右から `w` ぶん切り出す。
    fn right(&self, w: i32) -> IR {
        IR { l: self.r - w, ..*self }
    }
    fn inset(&self, d: i32) -> IR {
        IR { l: self.l + d, t: self.t + d, r: self.r - d, b: self.b - d }
    }
}

#[inline]
pub(crate) fn hit(r: &IR, x: i32, y: i32) -> bool {
    x >= r.l && x < r.r && y >= r.t && y < r.b
}

#[inline]
fn lerp_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |shift: u32| -> u32 {
        let x = ((a >> shift) & 0xFF) as f32;
        let y = ((b >> shift) & 0xFF) as f32;
        ((x + (y - x) * t).round() as u32) & 0xFF
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// 無効状態を「面板の地に寄せる」ことで表現する（JUCE の withMultipliedAlpha 相当）。
#[inline]
fn dim(c: u32, amount: f32) -> u32 {
    lerp_rgb(PANEL_BOT, c, amount)
}

#[inline]
fn lighten(c: u32, t: f32) -> u32 {
    lerp_rgb(c, 0xFFFFFF, t)
}

#[inline]
fn colorref(c: u32) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | (c >> 16 & 0xFF)
}

/// チャンネルごとに符号付きのざらつきを足す（アルマイトの質感）。
#[inline]
fn grain(c: u32, n: i32) -> u32 {
    let ch = |shift: u32| -> u32 {
        let v = ((c >> shift) & 0xFF) as i32 + n;
        v.clamp(0, 255) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// 決定的な擬似ノイズ (-1..1)。同じ座標なら常に同じ値。
#[inline]
fn hash2(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x27d4_eb2d) ^ (y as u32).wrapping_mul(0x1656_67b1);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_f491);
    h ^= h >> 13;
    ((h & 0xFFFF) as f32 / 32767.5) - 1.0
}

/// `pos` が丸角長方形の内側か。`clamp` は min > max で panic するので注意。
fn inside_round_rect(x: i32, y: i32, r: IR, rad: i32) -> bool {
    if x < r.l || x >= r.r || y < r.t || y >= r.b {
        return false;
    }
    let max_rad = ((r.w().min(r.h()) - 1) / 2).max(0);
    let rad = rad.min(max_rad);
    if rad == 0 {
        return true;
    }
    let cx = x.clamp(r.l + rad, r.r - 1 - rad) as f32;
    let cy = y.clamp(r.t + rad, r.b - 1 - rad) as f32;
    let dx = x as f32 - cx;
    let dy = y as f32 - cy;
    dx * dx + dy * dy <= (rad as f32) * (rad as f32)
}

/// リニア RMS -> VU の針位置 0..1。
///
/// 0 VU = `vref` dBFS とし、-20 VU 〜 +3 VU を表示する。
/// 基準値を変えると目盛り全体（= 対応する dBFS の範囲）が平行移動する。
fn vu_to_pos(lin: f32, vref: f32) -> f32 {
    if !(lin > 0.0) {
        return 0.0;
    }
    let db = 20.0 * lin.log10();
    let lo = -(vref + -VU_VU_AT_MIN); // -20 VU に当たる dBFS
    let hi = -(vref - VU_VU_AT_MAX); // +3 VU に当たる dBFS
    ((db - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// dB 表示用の文字列。下限以下は `-∞`。
fn db_text(db: f32) -> String {
    if db <= meter::DB_FLOOR + 0.05 {
        "-∞".to_string()
    } else {
        format!("{db:.1}")
    }
}

// --------------------------------------------------------------------------
// フェーダー定義
// --------------------------------------------------------------------------

pub(crate) struct FaderSpec {
    name: &'static str,
    min: f32,
    max: f32,
    default: f32,
    /// 上が % 表示か（dB を上に出すか）。
    percent_first: bool,
}

pub(crate) const FADERS: [FaderSpec; 3] = [
    FaderSpec {
        name: "AMBIENCE",
        min: dsp::AMBIENCE_MIN as f32,
        max: dsp::AMBIENCE_MAX as f32,
        default: dsp::AMBIENCE_DEFAULT as f32,
        percent_first: false,
    },
    FaderSpec {
        name: "VOICE",
        min: dsp::VOICE_MIN as f32,
        max: dsp::VOICE_MAX as f32,
        default: dsp::VOICE_DEFAULT as f32,
        percent_first: false,
    },
    FaderSpec {
        name: "REVERB",
        min: dsp::VOICE_REVERB_MIN as f32,
        max: dsp::VOICE_REVERB_MAX as f32,
        default: dsp::VOICE_REVERB_DEFAULT as f32,
        percent_first: true,
    },
];

impl FaderSpec {
    pub(crate) fn min(&self) -> f32 {
        self.min
    }
    pub(crate) fn max(&self) -> f32 {
        self.max
    }
    pub(crate) fn default(&self) -> f32 {
        self.default
    }
}

/// フェーダーの表示文字列 `(上段, 下段)`。dB と % を併記する。
pub(crate) fn fader_text(i: usize, v: f32) -> (String, String) {
    let pc = |t: f32| format!("{} %", (t * 100.0).round() as i32);
    match i {
        // AMBIENCE: 上 = 抑圧の上限 [dB]（-48 〜 -6 dB）、下 = %.
        0 => {
            let db = -(dsp::ambience_to_attenuation_db(v) as f64);
            (format!("{} dB", db_text(db as f32)), pc(v))
        }
        // VOICE: 上 = dB、下 = -12..+12 dB を 0..100 % とみなした値。
        1 => {
            let t = (v - dsp::VOICE_MIN as f32) / (dsp::VOICE_MAX as f32 - dsp::VOICE_MIN as f32);
            (format!("{v:+.1} dB"), pc(t))
        }
        // REVERB: 上 = %、下 = 残響成分のレベル [dB]（0 % は -∞）。
        _ => {
            let db = if v > 0.0 { 20.0 * v.log10() } else { meter::DB_FLOOR };
            (pc(v), format!("{} dB", db_text(db)))
        }
    }
}

/// MUTE / BYP を押したときの一時メッセージ（何が起きたかを文章で出す）。
///
/// 効果はフェーダーごとに違う（`dsp::FeatureMode` の表のとおり）ので、
/// 押した直後にここで言葉にして出す。
pub(crate) fn mode_message(i: usize, m: dsp::FeatureMode) -> String {
    let name = FADERS[i].name;
    let body = match (i, m) {
        (_, dsp::FeatureMode::Active) => "有効（つまみの値）",
        (0, dsp::FeatureMode::Mute) => "ミュート — 暗騒音を残しません (0 %)",
        (0, dsp::FeatureMode::Bypass) => "バイパス — 暗騒音をそのまま (100 %)",
        (1, dsp::FeatureMode::Mute) => "ミュート — presence を最小 (-12 dB)",
        (1, dsp::FeatureMode::Bypass) => "バイパス — presence を掛けません (0 dB)",
        (2, dsp::FeatureMode::Mute) => "ミュート — 残響を残しません (0 %)",
        _ => "バイパス — 残響をそのまま (100 %)",
    };
    format!("{name}: {body}")
}

// --------------------------------------------------------------------------
// レイアウト
// --------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Layout {
    /// タイトル（左）とメモリー（右）を載せる帯。
    title: IR,
    /// メモリーの見出し / スロット 8 個 / SAVE。
    memory_label: IR,
    slot: [IR; SLOTS],
    save_btn: IR,
    /// フッタ（ステータス + 有効/無効）。
    status: IR,
    /// ステータスの文字を描く範囲（MODEL / DEV キーのぶんを除いた右端）。
    status_text: IR,
    button: IR,
    /// MODEL / DEV キー（VST3 版のみ描く。OBS 版は矩形だけ計算する）。
    model_key: IR,
    device_key: IR,

    /// セクション見出し（IN / OUT / CONTROL / EFFECT）を描く帯。
    section: [IR; 3],
    /// VU の盤面（上 = IN = 加工前 / 下 = OUT = 加工後）と、その右の数値窓。
    vu: [IR; 2],
    vu_num: [IR; 2],
    /// フェーダーの当たり判定（トラックの柱）。MUTE / BYP はこの外にある。
    fader: [IR; 3],
    /// フェーダーのトラック（つまみが動く範囲）。
    fader_track: [IR; 3],
    label: [IR; 3],
    value_db: [IR; 3],
    value_pc: [IR; 3],
    /// MUTE / BYP キー（フェーダーの上に 1 段）。
    mute: [IR; 3],
    byp: [IR; 3],
    /// GR バー本体 / 目盛り / 数値欄。
    gr: IR,
    gr_scale: IR,
    gr_num: IR,
    /// 3 バンドバー（低 / 中 / 高）とラベル。
    bands: IR,
    bands_label: IR,
}

impl Layout {
    pub(crate) fn slot_rect(&self, i: usize) -> IR {
        self.slot[i]
    }
    pub(crate) fn save_rect(&self) -> IR {
        self.save_btn
    }
    pub(crate) fn button_rect(&self) -> IR {
        self.button
    }
    pub(crate) fn model_key_rect(&self) -> IR {
        self.model_key
    }
    pub(crate) fn device_key_rect(&self) -> IR {
        self.device_key
    }
    pub(crate) fn fader_rect(&self, i: usize) -> IR {
        self.fader[i]
    }
    pub(crate) fn fader_track_rect(&self, i: usize) -> IR {
        self.fader_track[i]
    }
    pub(crate) fn mute_rect(&self, i: usize) -> IR {
        self.mute[i]
    }
    pub(crate) fn byp_rect(&self, i: usize) -> IR {
        self.byp[i]
    }
    pub(crate) fn vu_rect(&self, i: usize) -> IR {
        self.vu[i]
    }
    pub(crate) fn vu_num_rect(&self, i: usize) -> IR {
        self.vu_num[i]
    }
}

/// 与えられた大きさに対する配置を求める。
///
/// `model_key` / `device_key` は VST3 版だけ true。false のときは矩形を
/// 計算するが `status` の幅を削らない（= OBS 版と同じレイアウトになる）。
pub(crate) fn compute_layout(
    scale: f32,
    w: i32,
    h: i32,
    model_key: bool,
    device_key: bool,
) -> Layout {
    let sc = |v: f32| (v * scale).round() as i32;
    let mut out = Layout::default();
    let m = sc(K_MARGIN);
    let gap = sc(K_GAP);
    let mut area = IR { l: m, t: m, r: w - m, b: h - m };

    // ---- ヘッダ（左 = タイトル / 右 = メモリー） ----
    out.title = area.top(sc(K_HEADER_H));

    let slot_w = sc(K_SLOT_W);
    let slot_h = sc(K_SLOT_H);
    let slot_gap = sc(K_SLOT_GAP);
    let save_w = sc(K_SAVE_W);
    let label_w = sc(K_MEMORY_LABEL_W);
    let row_w = label_w + SLOTS as i32 * (slot_w + slot_gap) - slot_gap + gap + save_w;
    let row_h = slot_h.max(sc(K_BUTTON_H));
    let cy = out.title.cy();
    let mut x = out.title.r - row_w;
    out.memory_label = IR { l: x, t: cy - row_h / 2, r: x + label_w, b: cy + row_h / 2 };
    x += label_w;
    for i in 0..SLOTS {
        out.slot[i] = IR { l: x, t: cy - slot_h / 2, r: x + slot_w, b: cy + slot_h / 2 };
        x += slot_w + slot_gap;
    }
    x += gap - slot_gap;
    out.save_btn = IR { l: x, t: cy - slot_h / 2, r: x + save_w, b: cy + slot_h / 2 };

    area.t += sc(K_HEADER_H) + gap;

    // ---- フッタ ----
    let mut footer = area.bottom(sc(K_FOOTER_H));
    let bw = sc(K_BUTTON_W);
    let bh = sc(K_BUTTON_H);
    out.button = IR {
        l: footer.r - bw,
        t: footer.cy() - bh / 2,
        r: footer.r,
        b: footer.cy() + bh / 2,
    };
    footer.r -= bw + gap;

    // MODEL / DEV キー（BYPASS の左に並べる）。OBS 版では描かないが、矩形は
    // 常に有効な値を入れておく（レイアウトのテストで使う）。
    let key_w = sc(K_FOOTER_KEY_W);
    let key_gap = sc(6.0);
    let mut kx = footer.r;
    out.device_key = IR {
        l: kx - key_w,
        t: footer.cy() - bh / 2,
        r: kx,
        b: footer.cy() + bh / 2,
    };
    kx -= key_w + key_gap;
    out.model_key = IR {
        l: kx - key_w,
        t: footer.cy() - bh / 2,
        r: kx,
        b: footer.cy() + bh / 2,
    };

    // 描くキーのぶんだけステータスの右端を詰める。キーが 1 つだけのときも
    // 元の OBS 版と同じ 76 px を空ける（刻印と文字が重ならない）。
    let keys = model_key as i32 + device_key as i32;
    let reserved = sc(K_FOOTER_KEY_W) * keys.max(1);
    out.status = footer;
    out.status_text = IR { r: footer.r - reserved, ..footer };

    area.b -= sc(K_FOOTER_H) + gap;

    // ---- 横を 3 セクションに分ける（INPUT / CONTROL / EFFECT） ----
    let sgap = sc(K_SECTION_GAP);
    let input = IR { r: area.l + sc(K_INPUT_W), ..area };
    let control = IR {
        l: input.r + sgap,
        r: input.r + sgap + sc(K_CONTROL_W),
        ..area
    };
    let effect = IR { l: control.r + sgap, ..area };

    let title_h = sc(K_SECTION_TITLE_H);
    out.section[0] = input.top(title_h);
    out.section[1] = control.top(title_h);
    out.section[2] = effect.top(title_h);

    // ---- IN / OUT: VU 2 本（IN が上 / OUT が下）+ 数値窓 ----
    let vu_area = IR { t: input.t + title_h + sc(6.0), ..input };
    let vg = sc(K_VU_GAP);
    let row_h = (vu_area.h() - vg) / 2;
    let num_w = sc(K_VU_NUM_W);
    for ch in 0..2usize {
        let t = vu_area.t + ch as i32 * (row_h + vg);
        let row = IR { t, b: t + row_h, ..vu_area };
        out.vu[ch] = row.left(row.w() - num_w - sc(6.0));
        out.vu_num[ch] = row.right(num_w);
    }

    // ---- CONTROL: フェーダー 3 本（+ ミュート / バイパスキー） ----
    let ctrl_area = IR { t: control.t + title_h + sc(4.0), ..control };
    let cw = ctrl_area.w() / 3;
    for i in 0..3usize {
        let cell = IR {
            l: ctrl_area.l + cw * i as i32,
            t: ctrl_area.t,
            r: ctrl_area.l + cw * (i as i32 + 1),
            b: ctrl_area.b,
        };
        out.label[i] = cell.top(sc(K_FADER_LABEL_H));
        out.value_db[i] = IR {
            t: out.label[i].b,
            b: out.label[i].b + sc(K_FADER_VALUE_H),
            ..cell
        };
        out.value_pc[i] = IR {
            t: out.value_db[i].b,
            b: out.value_db[i].b + sc(K_FADER_VALUE_H),
            ..cell
        };
        // MUTE / BYP のキー（値表示の下、トラックの上に 1 段）。
        let by = out.value_pc[i].b + sc(2.0);
        let bh = sc(K_FADER_BTN_H);
        let bg = sc(2.0);
        let key_w = ((cell.w() - sc(8.0) - bg) / 2).max(sc(10.0));
        out.mute[i] = IR {
            l: cell.l + sc(4.0),
            t: by,
            r: cell.l + sc(4.0) + key_w,
            b: by + bh,
        };
        out.byp[i] = IR {
            l: out.mute[i].r + bg,
            t: by,
            r: cell.r - sc(4.0),
            b: by + bh,
        };
        // トラック = つまみが動ける範囲（つまみの張り出しを含む）。
        // つまみの中心が動けるのは [t + hh/2, b - hh/2]。
        out.fader_track[i] = IR {
            t: out.byp[i].b + sc(5.0),
            b: cell.b - sc(2.0),
            ..cell
        };
        // 当たり判定はトラックの柱ぜんぶ（ミキサーのストリップ風）。
        // キーの高さは含めない（キーを押しただけでフェーダーが飛ばない）。
        out.fader[i] = IR {
            t: out.fader_track[i].t,
            ..cell
        };
    }

    // ---- EFFECT: GR（縦・0 から下へ）+ 3 バンド ----
    let fx = IR { t: effect.t + title_h, ..effect };
    let gr_w = sc(K_GR_BAR_W);
    let gr_scale_w = sc(K_GR_SCALE_W);
    let gr_num_w = sc(K_GR_NUM_W);
    let band_w = sc(K_BAND_W);
    let band_gap = sc(K_BAND_GAP);
    let bands_w = band_w * 3 + band_gap * 2;
    let total = gr_w + gr_scale_w + gr_num_w + gap + bands_w;
    let x0 = fx.l + ((fx.w() - total) / 2).max(0);

    let top = fx.t + sc(14.0);
    let band_label_h = sc(K_BAND_LABEL_H);
    let bottom = fx.b - band_label_h;

    out.gr = IR { l: x0, t: top, r: x0 + gr_w, b: bottom };
    out.gr_scale = IR {
        l: out.gr.r,
        r: out.gr.r + gr_scale_w,
        ..out.gr
    };
    out.gr_num = IR {
        l: out.gr_scale.r,
        r: out.gr_scale.r + gr_num_w,
        ..out.gr
    };
    let band_l = out.gr_num.r + gap;
    out.bands = IR {
        l: band_l,
        t: top,
        r: band_l + bands_w,
        b: bottom,
    };
    out.bands_label = IR {
        l: out.bands.l,
        t: bottom,
        r: out.bands.r,
        b: fx.b,
    };

    out
}

// --------------------------------------------------------------------------
// GDI リソース
// --------------------------------------------------------------------------

pub(crate) struct Fonts {
    pub(crate) scale: f32,
    title: HFONT,
    subtitle: HFONT,
    silk: HFONT,
    silk_small: HFONT,
    digital: HFONT,
    digital_small: HFONT,
    vu: HFONT,
    vu_big: HFONT,
    vu_tiny: HFONT,
    button: HFONT,
    status: HFONT,
}

impl Fonts {
    pub(crate) unsafe fn new(scale: f32) -> Self {
        Self {
            scale,
            title: make_font("Segoe UI", 17.0 * scale, 600),
            subtitle: make_font("Segoe UI", 9.0 * scale, 400),
            silk: make_font("Tahoma", 9.5 * scale, 700),
            silk_small: make_font("Tahoma", 8.0 * scale, 400),
            digital: make_font("Consolas", 13.0 * scale, 700),
            digital_small: make_font("Consolas", 10.0 * scale, 700),
            vu: make_font("Arial Narrow", 10.5 * scale, 700),
            vu_big: make_font("Arial Narrow", 15.0 * scale, 700),
            vu_tiny: make_font("Arial Narrow", 7.0 * scale, 700),
            button: make_font("Tahoma", 10.0 * scale, 700),
            status: make_font("Segoe UI", 10.0 * scale, 400),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [
            self.title,
            self.subtitle,
            self.silk,
            self.silk_small,
            self.digital,
            self.digital_small,
            self.vu,
            self.vu_big,
            self.vu_tiny,
            self.button,
            self.status,
        ] {
            if !f.0.is_null() {
                unsafe {
                    let _ = DeleteObject(f);
                }
            }
        }
    }
}

/// 32 bit top-down の DIB セクション + メモリ DC。
pub(crate) struct Dib {
    pub(crate) dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    pub(crate) w: i32,
    pub(crate) h: i32,
}

impl Dib {
    pub(crate) unsafe fn new(w: i32, h: i32) -> Result<Self, String> {
        if w <= 0 || h <= 0 {
            return Err(format!("DIB の大きさが不正: {w}x{h}"));
        }
        let dc = CreateCompatibleDC(HDC::default());
        if dc.0.is_null() {
            return Err("CreateCompatibleDC に失敗".into());
        }

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        };

        let mut bits: *mut c_void = ptr::null_mut();
        let bmp = CreateDIBSection(
            dc,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            HBITMAP::default(),
            0,
        );
        if bmp.0.is_null() {
            let _ = DeleteDC(dc);
            return Err("CreateDIBSection に失敗".into());
        }
        if bits.is_null() {
            let _ = DeleteObject(bmp);
            let _ = DeleteDC(dc);
            return Err("CreateDIBSection: 画素が取れない".into());
        }

        let old = SelectObject(dc, bmp);
        Ok(Dib { dc, bmp, old, bits: bits as *mut u32, w, h })
    }

    #[inline]
    fn put(&mut self, x: i32, y: i32, c: u32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        unsafe {
            *self.bits.add((y * self.w + x) as usize) = c & 0x00FF_FFFF;
        }
    }

    #[inline]
    fn get(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return 0;
        }
        unsafe { *self.bits.add((y * self.w + x) as usize) & 0x00FF_FFFF }
    }

    fn fill_rect(&mut self, r: IR, c: u32) {
        for y in r.t.max(0)..r.b.min(self.h) {
            for x in r.l.max(0)..r.r.min(self.w) {
                self.put(x, y, c);
            }
        }
    }

    /// 上 `top` → 下 `bottom` の縦グラデーション。
    fn fill_vgrad(&mut self, r: IR, top: u32, bottom: u32) {
        let span = (r.h() - 1).max(1) as f32;
        for y in r.t.max(0)..r.b.min(self.h) {
            let t = (y - r.t) as f32 / span;
            let c = lerp_rgb(top, bottom, t);
            for x in r.l.max(0)..r.r.min(self.w) {
                self.put(x, y, c);
            }
        }
    }

    /// 角丸の塗り。
    fn fill_round_rect(&mut self, r: IR, rad: i32, c: u32) {
        for y in r.t.max(0)..r.b.min(self.h) {
            for x in r.l.max(0)..r.r.min(self.w) {
                if inside_round_rect(x, y, r, rad) {
                    self.put(x, y, c);
                }
            }
        }
    }

    /// 縦バー用: 上 `top` → 下 `bottom` の縦グラデ（角丸）。
    fn fill_round_rect_vgrad(&mut self, r: IR, rad: i32, top: u32, bottom: u32) {
        let span = (r.h() - 1).max(1) as f32;
        for y in r.t.max(0)..r.b.min(self.h) {
            let t = (y - r.t) as f32 / span;
            let c = lerp_rgb(top, bottom, t);
            for x in r.l.max(0)..r.r.min(self.w) {
                if inside_round_rect(x, y, r, rad) {
                    self.put(x, y, c);
                }
            }
        }
    }

    fn fill_ellipse(&mut self, cx: i32, cy: i32, r: i32, c: u32) {
        let r2 = (r * r) as f32;
        for y in (cy - r).max(0)..(cy + r + 1).min(self.h) {
            for x in (cx - r).max(0)..(cx + r + 1).min(self.w) {
                let dx = (x - cx) as f32;
                let dy = (y - cy) as f32;
                if dx * dx + dy * dy <= r2 {
                    self.put(x, y, c);
                }
            }
        }
    }

    /// 縦グラデの円（LED / ネジ）。
    fn fill_ellipse_vgrad(&mut self, cx: i32, cy: i32, r: i32, top: u32, bottom: u32) {
        let r2 = (r * r) as f32;
        let span = (2 * r).max(1) as f32;
        for y in (cy - r).max(0)..(cy + r + 1).min(self.h) {
            let t = (y - (cy - r)) as f32 / span;
            let c = lerp_rgb(top, bottom, t);
            for x in (cx - r).max(0)..(cx + r + 1).min(self.w) {
                let dx = (x - cx) as f32;
                let dy = (y - cy) as f32;
                if dx * dx + dy * dy <= r2 {
                    self.put(x, y, c);
                }
            }
        }
    }

    /// 内側の影（枠から内側へ `depth` px ぶん暗くする）。彫り込み / ガラスの奥行き。
    fn inner_shadow(&mut self, r: IR, rad: i32, color: u32, depth: i32, strength: f32) {
        if depth <= 0 {
            return;
        }
        let d = depth as f32;
        for y in r.t.max(0)..r.b.min(self.h) {
            for x in r.l.max(0)..r.r.min(self.w) {
                if !inside_round_rect(x, y, r, rad) {
                    continue;
                }
                let edge = (x - r.l).min(r.r - 1 - x).min(y - r.t).min(r.b - 1 - y) as f32;
                if edge >= d {
                    continue;
                }
                let t = 1.0 - (edge + 0.5) / d;
                let bg = self.get(x, y);
                self.put(x, y, lerp_rgb(bg, color, strength * t * t));
            }
        }
    }

    /// 上端から `depth` px ぶん明るくする（金属の照り）。
    fn top_sheen(&mut self, r: IR, color: u32, depth: i32, strength: f32) {
        if depth <= 0 {
            return;
        }
        let d = depth as f32;
        for y in r.t.max(0)..(r.t + depth).min(self.h) {
            let t = 1.0 - (y - r.t) as f32 / d;
            for x in r.l.max(0)..r.r.min(self.w) {
                let bg = self.get(x, y);
                self.put(x, y, lerp_rgb(bg, color, strength * t));
            }
        }
    }

    /// 光源のにじみ（中心から遠ざかるほど弱く混ぜる）。VU 盤面のバックライト用。
    fn glow(&mut self, mask: IR, rad: i32, cx: i32, cy: i32, radius: i32, color: u32, strength: f32) {
        if radius <= 0 || strength <= 0.0 {
            return;
        }
        let r2 = (radius * radius) as f32;
        for y in mask.t.max(0)..mask.b.min(self.h) {
            for x in mask.l.max(0)..mask.r.min(self.w) {
                if !inside_round_rect(x, y, mask, rad) {
                    continue;
                }
                let dx = (x - cx) as f32;
                let dy = (y - cy) as f32;
                let d2 = dx * dx + dy * dy;
                if d2 >= r2 {
                    continue;
                }
                // sqrt を使わず 2 乗のまま減衰させる。
                let t = 1.0 - d2 / r2;
                let bg = self.get(x, y);
                self.put(x, y, lerp_rgb(bg, color, strength * t * t));
            }
        }
    }

    /// ガラスの反射。`x0` = 上端での帯の左位置、`slope` = 1 px 下がるごとの右への
    /// ずれ、`width` = 帯の幅。
    fn glass_streak(
        &mut self,
        mask: IR,
        rad: i32,
        x0: f32,
        slope: f32,
        width: i32,
        color: u32,
        strength: f32,
    ) {
        if width <= 0 || strength <= 0.0 {
            return;
        }
        let w = width as f32;
        for y in mask.t.max(0)..mask.b.min(self.h) {
            let sx = x0 + (y - mask.t) as f32 * slope;
            for x in mask.l.max(0)..mask.r.min(self.w) {
                if !inside_round_rect(x, y, mask, rad) {
                    continue;
                }
                let t = 1.0 - ((x as f32 - sx) / w).abs();
                if t <= 0.0 {
                    continue;
                }
                let bg = self.get(x, y);
                self.put(x, y, lerp_rgb(bg, color, strength * t));
            }
        }
    }

    /// ヘアライン（金属の目）。`step` px ごとに 1 px だけ明暗を混ぜる。
    fn hairlines(&mut self, r: IR, step: i32, color: u32, strength: f32) {
        if step < 2 || strength <= 0.0 {
            return;
        }
        let mut x = r.l;
        let mut k = 0;
        while x < r.r {
            // 3 本に 1 本だけ引いて、細かく見せる。
            if k % 3 == 0 && x >= 0 && x < self.w {
                for y in r.t.max(0)..r.b.min(self.h) {
                    let t = 0.35 + 0.65 * hash2(x, y);
                    let bg = self.get(x, y);
                    self.put(x, y, lerp_rgb(bg, color, strength * t));
                }
            }
            x += step;
            k += 1;
        }
    }

    /// 面取り: 上左に光、下右に影を 1 px 入れる。
    fn bevel(&mut self, r: IR, hi: u32, lo: u32) {
        self.fill_rect(IR { t: r.t, b: r.t + 1, ..r }, hi);
        self.fill_rect(IR { l: r.l, r: r.l + 1, ..r }, hi);
        self.fill_rect(IR { t: r.b - 1, b: r.b, ..r }, lo);
        self.fill_rect(IR { l: r.r - 1, r: r.r, ..r }, lo);
    }

    /// アルマイトのざらつき（決定的ノイズ）。
    fn grain(&mut self, r: IR, amount: f32) {
        for y in r.t.max(0)..r.b.min(self.h) {
            for x in r.l.max(0)..r.r.min(self.w) {
                let n = (hash2(x, y) * amount).round() as i32;
                if n != 0 {
                    let c = self.get(x, y);
                    self.put(x, y, grain(c, n));
                }
            }
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp);
            let _ = DeleteDC(self.dc);
        }
    }
}

// --------------------------------------------------------------------------
// 描画 1 枚ぶんの入力
// --------------------------------------------------------------------------

/// 描画に必要な値をすべて集めた束。
///
/// `paint` が `&mut dib` と `&fonts` を同時に借りるため、`Panel` から
/// **所有権ごと**先に取り出しておく（文字列は clone する。40 ms ごとなので
/// 十分安い）。
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub scale: f32,
    pub layout: Layout,
    /// メーターの読み（針のリニア値・dBFS・ピーク・GR）。
    pub reading: Reading,
    /// 0 VU の dBFS（正の数）。
    pub vref: f32,
    /// 出力 3 バンドの表示値 0..1 とピーク。
    pub bands: [f32; 3],
    pub bands_peak: [f32; 3],
    pub values: [f32; 3],
    /// フェーダーの表示文字列（上段 / 下段）。
    pub fader_text: [(String, String); 3],
    /// フェーダーごとのミュート / バイパス。
    pub modes: [dsp::FeatureMode; 3],
    pub status: u32,
    pub status_text: String,
    pub summary: String,
    /// 一時的なメッセージ（メモリー操作など）。
    pub flash: Option<String>,
    pub dereverb: bool,
    /// エンジンのチャンネル数（1 = モノラル / 2 = ステレオ / 0 = 未構築）。
    pub channels: usize,
    /// エンジンが想定しているサンプルレート（0 = 未構築）。
    pub rate: u32,
    pub enabled: bool,
    pub model: ModelChoice,
    /// 埋まっているスロット。
    pub slots: [bool; SLOTS],
    /// 呼び出したスロット（LED を点ける）。
    pub current_slot: Option<usize>,
    pub save_armed: bool,
    pub hover_slot: Option<usize>,
    pub hover_save: bool,
    pub hover_button: bool,
    pub press_button: bool,
    pub hover_fader: Option<usize>,
    /// マウスが乗っているミュート / バイパスキー。
    pub hover_mute: Option<usize>,
    pub hover_byp: Option<usize>,
    pub hover_model: bool,
    pub hover_device: bool,
    /// MODEL / DEV キーを描くか（VST3 版だけ true）。
    pub model_key: bool,
    pub device_key: bool,
    /// モデル / デバイスのキーに描く文字（`model_key` が false でも作る）。
    pub model_label: String,
    pub device_label: String,
}

// --------------------------------------------------------------------------
// 描画本体
// --------------------------------------------------------------------------

/// DIB 1 枚ぶんを描く。`dib` は `layout` に合った大きさであること。
pub(crate) fn paint(dib: &mut Dib, fonts: &Fonts, s: &Snapshot) {
    let dc = dib.dc;
    let (w, h) = (dib.w, dib.h);
    let scale = s.scale;
    let sc = |v: f32| (v * scale).round() as i32;
    let lay = s.layout;

    unsafe {
        SetBkMode(dc, TRANSPARENT);

        // ---- 面板（黒アルマイト） ----
        paint_panel(dib, w, h, scale);

        // ---- 四隅のネジ ----
        let sr = sc(K_SCREW_R);
        let si = sc(K_SCREW_INSET);
        for (cx, cy) in [
            (si, si),
            (w - si, si),
            (si, h - si),
            (w - si, h - si),
        ] {
            paint_screw(dib, cx, cy, sr);
        }

        // ---- ヘッダ ----
        draw_engraved(
            dc,
            "OBS NOISE SUPPRESSOR",
            lay.title.top(sc(20.0)),
            fonts.title,
            SILK,
            scale,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
        draw_text(
            dc,
            &format!(
                "NEURAL DE-NOISE  ·  DE-REVERB  ·  {}",
                if s.rate > 0 {
                    crate::rate_label(s.rate)
                } else {
                    "48 kHz".to_string()
                }
            ),
            IR { t: lay.title.t + sc(19.0), b: lay.title.t + sc(35.0), ..lay.title },
            fonts.subtitle,
            SILK_FAINT,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );

        // ヘッダ下の溝（彫り込み）。
        let gy = lay.title.b + sc(K_GAP) / 2;
        dib.fill_rect(IR { l: lay.title.l, t: gy, r: lay.title.r, b: gy + 1 }, BEVEL_LO);
        dib.fill_rect(
            IR { l: lay.title.l, t: gy + 1, r: lay.title.r, b: gy + 2 },
            lerp_rgb(PANEL_BOT, BEVEL_HI, 0.35),
        );

        // ---- メモリー ----
        paint_memory(dib, dc, fonts, s, scale);

        // ---- セクション見出し ----
        let sec_names = ["IN / OUT", "CONTROL", "EFFECT"];
        for i in 0..3usize {
            let r = lay.section[i];
            draw_engraved(
                dc,
                sec_names[i],
                r,
                fonts.silk,
                SILK_DIM,
                scale,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE,
            );
            let y = r.b - 1;
            dib.fill_rect(IR { l: r.l, t: y, r: r.r, b: y + 1 }, BEVEL_LO);
            dib.fill_rect(
                IR { l: r.l, t: y + 1, r: r.r, b: y + 2 },
                lerp_rgb(PANEL_BOT, BEVEL_HI, 0.35),
            );
        }

        // セクションの区切り（彫り込みの縦溝）。実機のモジュール境界。
        let div_top = lay.title.b + sc(K_GAP) / 2;
        let div_bot = lay.status.t - sc(K_GAP);
        for i in 0..2usize {
            let x = (lay.section[i].r + lay.section[i + 1].l) / 2;
            dib.fill_rect(IR { l: x, t: div_top, r: x + 1, b: div_bot }, BEVEL_LO);
            dib.fill_rect(
                IR { l: x + 1, t: div_top, r: x + 2, b: div_bot },
                lerp_rgb(PANEL_BOT, BEVEL_HI, 0.35),
            );
        }
        // フッタの区切り（横溝）。
        let fy = lay.status.t - sc(K_GAP) / 2;
        dib.fill_rect(IR { l: lay.title.l, t: fy, r: lay.title.r, b: fy + 1 }, BEVEL_LO);
        dib.fill_rect(
            IR { l: lay.title.l, t: fy + 1, r: lay.title.r, b: fy + 2 },
            lerp_rgb(PANEL_BOT, BEVEL_HI, 0.35),
        );

        // 入力の基準値をセクション見出しの右端に出す。
        draw_text(
            dc,
            &format!("0 VU = -{:.0} dBFS", s.vref),
            lay.section[0],
            fonts.silk_small,
            SILK_FAINT,
            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
        );

        // ---- VU 2 本（アナログ針・上 = IN = 加工前 / 下 = OUT = 加工後） ----
        let on = s.channels > 0;
        let r = s.reading;
        // (針のリニア値, 現在値 dBFS, ピーク dBFS, 盤面の名前)
        let meters = [
            (r.vu_in_lin, r.vu_in_db, r.peak_in_db, "IN"),
            (r.vu_out_lin, r.vu_out_db, r.peak_out_db, "OUT"),
        ];
        for (m, (lin, db, peak, name)) in meters.iter().enumerate() {
            let pos = vu_to_pos(*lin, s.vref);
            paint_vu(dib, dc, fonts, scale, lay.vu[m], pos, name, on);
            paint_meter_num(dib, dc, fonts, scale, lay.vu_num[m], name, *db, *peak, on);
        }

        // ---- フェーダー 3 本（+ MUTE / BYP キー） ----
        for i in 0..3usize {
            let on = i != 2 || s.dereverb;
            let mode = s.modes[i];
            paint_fader(
                dib,
                dc,
                fonts,
                scale,
                lay.fader_track[i],
                lay.label[i],
                lay.value_db[i],
                lay.value_pc[i],
                i,
                s.values[i],
                &s.fader_text[i].0,
                &s.fader_text[i].1,
                on,
                // ミュート / バイパス中は値の表示を落として「効いていない」
                // ことを見せる（つまみの位置そのものは変わらない）。
                on && mode == dsp::FeatureMode::Active,
                s.hover_fader == Some(i),
            );
            paint_switch(
                dib,
                dc,
                fonts,
                scale,
                lay.mute[i],
                "MUTE",
                LED_RED,
                mode == dsp::FeatureMode::Mute,
                on,
                s.hover_mute == Some(i),
            );
            paint_switch(
                dib,
                dc,
                fonts,
                scale,
                lay.byp[i],
                "BYP",
                WARM,
                mode == dsp::FeatureMode::Bypass,
                on,
                s.hover_byp == Some(i),
            );
        }

        // ---- GR（コンプの GR と同じく 0 から下へ振れる） ----
        paint_gr(dib, dc, fonts, scale, lay.gr, lay.gr_scale, lay.gr_num, s.reading.gr_db);

        // ---- 出力 3 バンド（GR の隣に細く） ----
        paint_bands(
            dib,
            dc,
            fonts,
            scale,
            lay.bands,
            lay.bands_label,
            &s.bands,
            &s.bands_peak,
        );

        // ---- ステータス ----
        paint_status(dib, dc, fonts, s, scale);

        // ---- モデル / デバイスのキー（VST3 版のみ） ----
        if s.model_key {
            paint_label_key(
                dib,
                dc,
                fonts,
                scale,
                lay.model_key,
                &s.model_label,
                s.hover_model,
            );
        }
        if s.device_key {
            paint_label_key(
                dib,
                dc,
                fonts,
                scale,
                lay.device_key,
                &s.device_label,
                s.hover_device,
            );
        }

        // ---- BYPASS ----
        paint_key(
            dib,
            dc,
            fonts,
            scale,
            lay.button,
            "BYPASS",
            if !s.enabled { LED_RED } else { 0 },
            s.hover_button,
            s.press_button,
        );
    }
}

/// 面板（黒アルマイト）の地。縦グラデ + 上端の照り + ざらつき + 外周の溝。
fn paint_panel(dib: &mut Dib, w: i32, h: i32, scale: f32) {
    let sc = |v: f32| (v * scale).round() as i32;
    let all = IR { l: 0, t: 0, r: w, b: h };
    dib.fill_vgrad(all, PANEL_TOP, PANEL_BOT);
    // ヘアライン（金属の目）。実機のアルミパネルと同じ方向で薄く入れる。
    dib.hairlines(all, 3, 0x77808c, 0.10);
    dib.top_sheen(all, 0xffffff, sc(2.0).max(1), 0.045);
    dib.grain(all, 2.0);

    // 外周は彫り込み（上左に影 / 下右に光）。
    dib.fill_rect(IR { t: 0, b: 1, ..all }, BG);
    dib.fill_rect(IR { l: 0, r: 1, ..all }, BG);
    dib.fill_rect(IR { t: h - 1, b: h, ..all }, lerp_rgb(PANEL_BOT, BEVEL_HI, 0.40));
    dib.fill_rect(IR { l: w - 1, r: w, ..all }, lerp_rgb(PANEL_BOT, BEVEL_HI, 0.40));
    // その内側に面取りの光（板が浮いて見える）。
    let hi = lerp_rgb(PANEL_TOP, BEVEL_HI, 0.30);
    dib.fill_rect(IR { t: 1, b: 2, l: 1, r: w - 1 }, hi);
    dib.fill_rect(IR { l: 1, r: 2, t: 1, b: h - 1 }, hi);
}

/// 四隅のネジ（金属のドーム + 十字の溝）。
fn paint_screw(dib: &mut Dib, cx: i32, cy: i32, r: i32) {
    let r = r.max(3);
    // 座面（座ぐりの影）
    dib.fill_ellipse(cx, cy + 1, r + 1, 0x0a0b0d);
    // ドーム
    dib.fill_ellipse_vgrad(cx, cy, r, 0xaeb4ba, 0x494f56);
    // 十字の溝（中央が太く、外周へ細くなる = フィリップス）。
    let t = (r / 2).max(1);
    for k in 0..3 {
        let w = (t - k).max(1);
        let d = k;
        dib.fill_rect(
            IR { l: cx - r + 1 + d, t: cy - w / 2, r: cx + r - 1 - d, b: cy - w / 2 + w },
            0x181a1c,
        );
        dib.fill_rect(
            IR { l: cx - w / 2, t: cy - r + 1 + d, r: cx - w / 2 + w, b: cy + r - 1 - d },
            0x181a1c,
        );
    }
    // 溝の上辺に光（面取りが光る）。
    dib.fill_rect(IR { l: cx - r + 2, t: cy - t / 2 - 1, r: cx + r - 2, b: cy - t / 2 }, 0x7d838a);
    dib.fill_rect(IR { l: cx - t / 2 - 1, t: cy - r + 2, r: cx - t / 2, b: cy + r - 2 }, 0x7d838a);
    // 頭の照り
    dib.fill_ellipse(cx - r / 3, cy - r / 3, (r / 3).max(1), 0xc9ced4);
}

/// 押しボタン（黒いキー + 彫り込みラベル + LED）。
///
/// `led` が 0 以外なら点灯（ラベルも明るくする）。`pressed` で面取りを反転。
fn paint_key(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    r: IR,
    label: &str,
    led: u32,
    hover: bool,
    pressed: bool,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(3.0);
    let on = led != 0;

    let (top, bot) = if pressed {
        (0x141619, 0x22262a)
    } else if hover {
        (0x2c3035, 0x1f2327)
    } else {
        (0x262a2e, 0x1a1d21)
    };
    dib.fill_round_rect_vgrad(r, rad, top, bot);
    dib.grain(r, 1.0);

    if pressed {
        dib.inner_shadow(r, rad, 0x000000, sc(3.0), 0.55);
    } else {
        dib.bevel(r, lerp_rgb(top, BEVEL_HI, 0.55), BEVEL_LO);
    }

    // LED（キーの上寄り中央）。
    let lr = sc(2.0).max(2);
    let (lx, ly) = (r.cx(), r.t + sc(5.0));
    if on {
        dib.fill_ellipse(lx, ly, lr + sc(1.0), lerp_rgb(bot, led, 0.35));
        dib.fill_ellipse(lx, ly, lr, led);
        dib.fill_ellipse(lx - 1, ly - 1, (lr / 2).max(1), lighten(led, 0.55));
    } else {
        dib.fill_ellipse(lx, ly, lr, 0x101214);
        dib.fill_ellipse(lx, ly + 1, lr - 1, 0x181b1e);
    }

    let text_r = IR { t: r.t + sc(6.0), ..r };
    unsafe {
        draw_engraved(
            dc,
            label,
            text_r,
            fonts.button,
            if on { VALUE } else { SILK_DIM },
            scale,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

/// フッタの MODEL / DEV キー（VST3 版のみ）。
///
/// プロパティ画面が無いホスト用に、押すたび選択肢を送る小さいキー。見た目は
/// メモリーのスロットキーと同じ「黒いキー + 彫り込み文字」。
fn paint_label_key(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    r: IR,
    label: &str,
    hover: bool,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(3.0);
    let (top, bot) = if hover {
        (0x2c3035, 0x1f2327)
    } else {
        (0x262a2e, 0x1a1d21)
    };
    dib.fill_round_rect_vgrad(r, rad, top, bot);
    dib.grain(r, 1.0);
    dib.bevel(r, lerp_rgb(top, BEVEL_HI, 0.55), BEVEL_LO);

    // 左端の細い LED（選択中のものが「効いている」ことを示す）。
    let lamp = IR {
        l: r.l + sc(3.0),
        t: r.t + sc(6.0),
        r: r.l + sc(3.0) + sc(3.0).max(2),
        b: r.b - sc(6.0),
    };
    dib.fill_rect(lamp, 0x0b0d0f);
    dib.fill_rect(lamp.inset(1), WARM);

    unsafe {
        draw_engraved(
            dc,
            label,
            IR { l: lamp.r, r: r.r - sc(4.0), ..r },
            fonts.silk_small,
            if hover { VALUE } else { SILK_DIM },
            scale,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

/// フェーダーの MUTE / BYP キー（小さいトグル）。
///
/// 左端の細い窓が LED（点灯 = その状態が有効）、右に文字。`enabled` が false
/// のときは押せないので全体を沈める（REVERB が 1 出力モデルのとき）。
#[allow(clippy::too_many_arguments)]
unsafe fn paint_switch(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    r: IR,
    label: &str,
    led: u32,
    lit: bool,
    enabled: bool,
    hover: bool,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(2.0);

    let (top, bot) = if !enabled {
        (0x1b1e21, 0x141618)
    } else if lit {
        (0x2b3034, 0x1d2124)
    } else if hover {
        (0x30353a, 0x22262a)
    } else {
        (0x25292d, 0x1a1d20)
    };
    dib.fill_round_rect_vgrad(r, rad, top, bot);
    dib.bevel(r, lerp_rgb(top, BEVEL_HI, 0.5), BEVEL_LO);

    // LED（左端の縦長の窓）。消灯中は黒い穴のまま。
    let lamp = IR {
        l: r.l + sc(3.0),
        t: r.t + sc(3.0),
        r: r.l + sc(3.0) + sc(3.0).max(2),
        b: r.b - sc(3.0),
    };
    dib.fill_rect(lamp, 0x0b0d0f);
    if lit && enabled {
        dib.fill_rect(lamp.inset(1), led);
        dib.fill_rect(IR { t: lamp.t + 1, ..lamp.inset(1) }, lighten(led, 0.4));
    }

    draw_engraved(
        dc,
        label,
        IR { l: lamp.r, r: r.r - sc(2.0), ..r },
        fonts.silk_small,
        if !enabled {
            SILK_FAINT
        } else if lit {
            VALUE
        } else {
            SILK_DIM
        },
        scale,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
}

/// 小さいキー（メモリーのスロット）。
fn paint_slot_key(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    r: IR,
    label: &str,
    filled: bool,
    current: bool,
    hover: bool,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(2.0);
    let (top, bot) = if hover {
        (0x33383d, 0x22262a)
    } else {
        (0x24282c, 0x191c20)
    };
    dib.fill_round_rect_vgrad(r, rad, top, bot);
    dib.bevel(r, lerp_rgb(top, BEVEL_HI, 0.5), BEVEL_LO);

    // LED（上中央）。
    let lr = sc(1.8).max(2);
    let (lx, ly) = (r.cx(), r.t + sc(4.0));
    let led = if current {
        LED_GREEN
    } else if filled {
        WARM
    } else {
        0
    };
    if led != 0 {
        dib.fill_ellipse(lx, ly, lr, led);
        dib.fill_ellipse(lx - 1, ly - 1, (lr / 2).max(1), lighten(led, 0.6));
    } else {
        dib.fill_ellipse(lx, ly, lr, 0x101214);
    }

    let text_r = IR { t: r.t + sc(5.0), ..r };
    unsafe {
        draw_engraved(
            dc,
            label,
            text_r,
            fonts.digital_small,
            if filled { VALUE } else { SILK_FAINT },
            scale,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

/// メモリー行（見出し + スロット 8 + SAVE）。
fn paint_memory(dib: &mut Dib, dc: HDC, fonts: &Fonts, s: &Snapshot, scale: f32) {
    let lay = s.layout;
    unsafe {
        draw_text(
            dc,
            "MEMORY",
            lay.memory_label,
            fonts.silk_small,
            SILK_FAINT,
            DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
        );
    }
    for i in 0..SLOTS {
        paint_slot_key(
            dib,
            dc,
            fonts,
            scale,
            lay.slot[i],
            &(i + 1).to_string(),
            s.slots[i],
            s.current_slot == Some(i),
            s.hover_slot == Some(i),
        );
    }

    // SAVE: 待機中は赤く光らせる。空きスロットと区別するため幅を広めに取ってある。
    paint_key(
        dib,
        dc,
        fonts,
        scale,
        lay.save_btn,
        "SAVE",
        if s.save_armed { LED_RED } else { 0 },
        s.hover_save,
        false,
    );
}

/// アナログ VU 盤面（アイボリー + 針）。
unsafe fn paint_vu(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    r: IR,
    pos: f32,
    name: &str,
    on: bool,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(3.0);

    // ---- 盤面（ランプで後ろから照らした暖色のアイボリー）----
    let lit = if on { 1.0 } else { 0.45 };
    let (top, bot) = if on {
        (VU_FACE_TOP, VU_FACE_BOT)
    } else {
        (dim(VU_FACE_TOP, 0.34), dim(VU_FACE_BOT, 0.34))
    };
    dib.fill_round_rect_vgrad(r, rad, top, bot);
    // 光源は上（少し右）にある。盤面の上側が暖かく光る。
    dib.glow(
        r,
        rad,
        r.cx() + r.w() / 6,
        r.t - r.h() / 4,
        (r.h() as f32 * 1.3).round() as i32,
        VU_HOT,
        0.55 * lit,
    );
    // ガラスの反射（右下へ抜ける斜めの帯）。
    dib.glass_streak(
        r,
        rad,
        r.l as f32 + r.w() as f32 * 0.16,
        1.25,
        sc(26.0),
        0xffffff,
        0.14 * lit,
    );
    // 枠の内側の奥行き（ガラス越しの影）と、ガラス上端の照り。
    dib.inner_shadow(r, rad, 0x2a2116, sc(6.0), 0.42);
    dib.top_sheen(r.inset(sc(2.0)), 0xffffff, sc(3.0), 0.26);

    // 支点と半径。針は盤面の下寄りを中心に振る。
    let px = r.cx() as f32;
    let py = (r.b - sc(VU_PIVOT_LIFT)) as f32;
    let arc_r = (r.h() as f32 - 20.0 * scale).max(18.0 * scale);

    let vu_ang = |vu: f32| -> f32 {
        let t = ((vu - VU_VU_AT_MIN) / (VU_VU_AT_MAX - VU_VU_AT_MIN)).clamp(0.0, 1.0);
        VU_ANGLE_MIN + t * (VU_ANGLE_MAX - VU_ANGLE_MIN)
    };
    let pt = |a: f32, rr: f32| -> (f32, f32) {
        let rad = a.to_radians();
        (px + rr * rad.sin(), py - rr * rad.cos())
    };

    let ink = if on { VU_INK } else { dim(VU_INK, 0.40) };
    let ink_dim = if on { VU_INK_DIM } else { dim(VU_INK_DIM, 0.40) };
    let red = if on { VU_RED } else { dim(VU_RED, 0.40) };

    // ---- 目盛りの弧（実機は太い黒の弧。0 VU から +3 は赤い帯）----
    stroke_arc(
        dc,
        px,
        py,
        arc_r,
        vu_ang(VU_VU_AT_MIN),
        vu_ang(VU_VU_AT_MAX),
        ink,
        (1.8 * scale).max(2.0).round() as i32,
    );
    stroke_arc(
        dc,
        px,
        py,
        arc_r + 1.0 * scale,
        vu_ang(0.0),
        vu_ang(VU_VU_AT_MAX),
        red,
        (2.2 * scale).max(2.0).round() as i32,
    );

    // ---- 目盛り（0 付近が細かい = 実機の VU と同じ）----
    const TICKS: [f32; 11] = [-20.0, -10.0, -7.0, -5.0, -3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0];
    const LABELS: [i32; 5] = [-20, -10, -5, 0, 3];
    for &v in TICKS.iter() {
        let a = vu_ang(v);
        let len = if v >= 0.0 || (v.fract() == 0.0) { 6.5 * scale } else { 4.0 * scale };
        let (x0, y0) = pt(a, arc_r - len);
        let (x1, y1) = pt(a, arc_r + 0.5 * scale);
        let c = if v >= 0.0 { red } else { ink };
        stroke_line(
            dc,
            x0.round() as i32,
            y0.round() as i32,
            x1.round() as i32,
            y1.round() as i32,
            c,
            (1.3 * scale).max(1.0) as i32,
        );
    }
    for v in LABELS {
        let a = vu_ang(v as f32);
        let (x, y) = pt(a, arc_r - 16.0 * scale);
        let txt = if v > 0 { format!("+{v}") } else { v.to_string() };
        let rc = IR {
            l: (x - 12.0 * scale).round() as i32,
            t: (y - 5.0 * scale).round() as i32,
            r: (x + 12.0 * scale).round() as i32,
            b: (y + 5.0 * scale).round() as i32,
        };
        draw_text(
            dc,
            &txt,
            rc,
            fonts.vu,
            if v >= 0 { red } else { ink },
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }

    // ---- 2 本目の細い弧（0 VU = 100 % の校正目盛り。実機にある）----
    let cal_r = arc_r - 26.0 * scale;
    if cal_r > 12.0 * scale {
        stroke_arc(
            dc,
            px,
            py,
            cal_r,
            vu_ang(VU_VU_AT_MIN),
            vu_ang(0.0),
            ink_dim,
            (0.9 * scale).max(1.0) as i32,
        );
        for (v, txt) in [(-10.0f32, "50"), (-1.0, "100 %")] {
            let a = vu_ang(v);
            let (x, y) = pt(a, cal_r - 7.0 * scale);
            let rc = IR {
                l: (x - 16.0 * scale).round() as i32,
                t: (y - 4.0 * scale).round() as i32,
                r: (x + 16.0 * scale).round() as i32,
                b: (y + 4.0 * scale).round() as i32,
            };
            draw_text(
                dc,
                txt,
                rc,
                fonts.vu_tiny,
                ink_dim,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
    }

    // ---- 「VU」と「dB」の盤面印刷 ----
    draw_text(
        dc,
        "VU",
        IR {
            l: r.l + sc(8.0),
            t: r.t + sc(5.0),
            r: r.l + sc(38.0),
            b: r.t + sc(18.0),
        },
        fonts.vu,
        ink,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
    );
    draw_text(
        dc,
        "dB",
        IR {
            l: r.cx() - sc(8.0),
            t: r.b - sc(52.0),
            r: r.cx() + sc(36.0),
            b: r.b - sc(34.0),
        },
        fonts.vu_big,
        ink,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );

    // ---- 針（実機は細い黒針。付け根から先へ向けて細くする）----
    let a = VU_ANGLE_MIN + pos.clamp(0.0, 1.0) * (VU_ANGLE_MAX - VU_ANGLE_MIN);
    let (tx, ty) = pt(a, arc_r - 2.0 * scale);
    let (bx, by) = pt(a, -2.5 * scale); // 支点のすぐ向こう（カバーに隠れる）
    let needle = if on { NEEDLE } else { dim(NEEDLE, 0.40) };
    let (t0x, t0y) = pt(a, arc_r - 22.0 * scale);
    // 根元（太め）→ 先端（細い）の 2 本でテーパーにする。
    stroke_line(
        dc,
        bx.round() as i32,
        by.round() as i32,
        t0x.round() as i32,
        t0y.round() as i32,
        needle,
        (1.3 * scale).max(1.0).round() as i32,
    );
    stroke_line(
        dc,
        t0x.round() as i32,
        t0y.round() as i32,
        tx.round() as i32,
        ty.round() as i32,
        if on { NEEDLE_TIP } else { dim(NEEDLE_TIP, 0.40) },
        (1.2 * scale).max(1.0).round() as i32,
    );

    // 支点のカバー（実機の黒い丸い蓋）。
    let cap_w = sc(14.0).max(8);
    let cap_h = sc(7.0).max(5);
    let (cx, cy) = (px.round() as i32, py.round() as i32);
    dib.fill_round_rect(
        IR {
            l: cx - cap_w / 2,
            t: cy - cap_h / 2,
            r: cx + cap_w / 2,
            b: cy + cap_h / 2,
        },
        cap_h / 2,
        if on { 0x27241f } else { dim(0x27241f, 0.40) },
    );
    dib.top_sheen(
        IR {
            l: cx - cap_w / 2,
            t: cy - cap_h / 2,
            r: cx + cap_w / 2,
            b: cy,
        },
        if on { 0x8e887c } else { dim(0x8e887c, 0.40) },
        sc(1.0).max(1),
        0.55,
    );

    // ---- チャンネル名（右下）----
    draw_text(
        dc,
        name,
        IR {
            l: r.r - sc(28.0),
            t: r.b - sc(20.0),
            r: r.r - sc(8.0),
            b: r.b - sc(4.0),
        },
        fonts.silk,
        ink,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
    );

    // ---- ベゼル（黒いモールド枠 + 内側の金属縁 + ガラス端の光）----
    stroke_round_rect(dc, r, rad, 0x08090b, (2.0 * scale).max(2.0).round() as i32);
    stroke_round_rect(dc, r.inset(sc(2.0)), rad, 0x6a7076, 1);
    dib.bevel(r.inset(sc(2.0)), 0x9aa0a5, 0x14161a);
}

/// VU の数値窓（名前 + 現在値 dBFS + ピーク dBFS）。凹んだ表示窓。
fn paint_meter_num(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    r: IR,
    name: &str,
    db: f32,
    peak_db: f32,
    on: bool,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(3.0);

    dib.fill_round_rect_vgrad(r, rad, 0x0a0c0e, 0x111417);
    dib.inner_shadow(r, rad, 0x000000, sc(3.0), 0.6);
    dib.bevel(r, 0x2a2f35, 0x050607);

    let mid = r.t + r.h() / 2;
    let (c_now, c_peak) = if on {
        (VALUE, WARM_HI)
    } else {
        (dim(VALUE, 0.4), dim(WARM_HI, 0.4))
    };

    unsafe {
        // 名前（IN / OUT）。
        draw_text(
            dc,
            name,
            IR { l: r.l, t: r.t + sc(2.0), r: r.r, b: r.t + sc(13.0) },
            fonts.silk_small,
            if on { SILK_DIM } else { SILK_FAINT },
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        draw_text(
            dc,
            &db_text(db),
            IR { l: r.l, t: r.t + sc(13.0), r: r.r, b: mid - sc(2.0) },
            fonts.digital,
            c_now,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        draw_text(
            dc,
            "dBFS",
            IR { t: mid - sc(4.0), b: mid + sc(7.0), ..r },
            fonts.silk_small,
            SILK_FAINT,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        // 区切りの溝。
        dib.fill_rect(
            IR { l: r.l + sc(6.0), t: mid + sc(10.0), r: r.r - sc(6.0), b: mid + sc(11.0) },
            0x050607,
        );
        draw_text(
            dc,
            &db_text(peak_db),
            IR { l: r.l, t: mid + sc(12.0), r: r.r, b: r.b - sc(12.0) },
            fonts.digital_small,
            c_peak,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        draw_text(
            dc,
            "PEAK",
            IR { t: r.b - sc(13.0), b: r.b - sc(3.0), ..r },
            fonts.silk_small,
            SILK_FAINT,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

/// 縦フェーダー 1 本（目盛り + トラック + つまみ + dB / % の併記）。
///
/// `value_lit` は値表示を点けるか。ミュート / バイパス中はここだけ落とす
/// （つまみの位置そのものは変わらないので、柱とつまみは通常のまま）。
#[allow(clippy::too_many_arguments)]
unsafe fn paint_fader(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    track: IR,
    label_r: IR,
    db_r: IR,
    pc_r: IR,
    index: usize,
    value: f32,
    db_text_s: &str,
    pc_text_s: &str,
    on: bool,
    value_lit: bool,
    hover: bool,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let spec = &FADERS[index];

    // ---- 見出し（彫り込み）----
    draw_engraved(
        dc,
        spec.name,
        label_r,
        fonts.silk,
        if on { SILK } else { SILK_FAINT },
        scale,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );

    // ---- 数値（上段 = dB / %、下段 = もう一方）----
    let t = ((value - spec.min) / (spec.max - spec.min)).clamp(0.0, 1.0);
    let (up, low) = if spec.percent_first {
        (pc_text_s, db_text_s)
    } else {
        (db_text_s, pc_text_s)
    };
    draw_text(
        dc,
        up,
        db_r,
        fonts.digital_small,
        if value_lit { VALUE } else { dim(VALUE, 0.45) },
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
    draw_text(
        dc,
        low,
        pc_r,
        fonts.silk_small,
        if value_lit { SILK_DIM } else { SILK_FAINT },
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );

    // ---- トラック（掘り込んだスリット）----
    let hh = sc(K_FADER_HANDLE_H);
    let slot_w = sc(K_FADER_TRACK_W);
    let slot = IR {
        l: track.cx() - slot_w / 2,
        t: track.t,
        r: track.cx() + slot_w / 2,
        b: track.b,
    };
    dib.fill_round_rect(slot, slot_w / 2, 0x0a0c0e);
    dib.inner_shadow(slot, slot_w / 2, 0x000000, sc(2.0), 0.7);
    // 金属レール（実機のフェーダーはスリットの両側が金属）。
    let rail = sc(2.0).max(1);
    for side in [-1i32, 1] {
        let x = if side < 0 { slot.l - rail } else { slot.r };
        let rr = IR { l: x, t: track.t, r: x + rail, b: track.b };
        if on {
            dib.fill_vgrad(rr, 0x9aa0a6, 0x3d4349);
            // 内側の稜線に光を入れて、えぐれではなく浮きに見せる。
            let inner = if side < 0 { rr.r - 1 } else { rr.l };
            dib.fill_rect(IR { l: inner, t: rr.t, r: inner + 1, b: rr.b }, 0xc3c8ce);
        } else {
            dib.fill_vgrad(rr, dim(0x9aa0a6, 0.45), dim(0x3d4349, 0.45));
        }
    }

    // つまみの中心が動ける範囲。
    let y_top = track.t + hh / 2;
    let y_bot = track.b - hh / 2;
    let travel = (y_bot - y_top).max(1) as f32;
    let y_handle = y_top + ((1.0 - t) * travel).round() as i32;

    // ---- 目盛り（右側）----
    let ticks = 11;
    for k in 0..ticks {
        let tt = k as f32 / (ticks - 1) as f32;
        let y = y_top + ((1.0 - tt) * travel).round() as i32;
        let major = k % 5 == 0;
        let len = if major { sc(8.0) } else { sc(4.5) };
        let x0 = slot.r + sc(4.0);
        let c = if on { if major { SILK_DIM } else { SILK_FAINT } } else { dim(SILK_FAINT, 0.4) };
        dib.fill_rect(IR { l: x0, t: y, r: x0 + len, b: y + 1 }, c);
        dib.fill_rect(IR { l: x0, t: y + 1, r: x0 + len, b: y + 2 }, 0x0b0d0f);
    }
    // 0 dB / 中央の基準線（VOICE は中央、他は下端）。
    let y_ref = if index == 1 { track.cy() } else { y_bot };
    dib.fill_rect(
        IR { l: slot.l - sc(3.0), t: y_ref, r: slot.r + sc(3.0), b: y_ref + 1 },
        if on { WARM } else { dim(WARM, 0.35) },
    );

    // ---- 値のバー（基準線からつまみまで）----
    let (ya, yb) = if y_handle < y_ref { (y_handle, y_ref) } else { (y_ref, y_handle) };
    if yb - ya > 1 {
        let bar = IR { l: slot.l + sc(1.0), t: ya, r: slot.r - sc(1.0), b: yb };
        let (c0, c1) = if index == 1 {
            (ACCENT_HI, ACCENT)
        } else {
            (ACCENT, ACCENT_HI)
        };
        if on {
            dib.fill_vgrad(bar, c0, c1);
        } else {
            dib.fill_vgrad(bar, dim(ACCENT, 0.35), dim(ACCENT_HI, 0.35));
        }
    }

    // ---- つまみ（金属のキャップ）----
    let cap_w = sc(K_FADER_HANDLE_W);
    let cap = IR {
        l: slot.cx() - cap_w / 2,
        t: y_handle - hh / 2,
        r: slot.cx() + cap_w / 2,
        b: y_handle + hh / 2,
    };
    // 落ち影。
    dib.fill_round_rect(IR { t: cap.t + sc(2.0), b: cap.b + sc(2.0), ..cap }, sc(2.0), 0x08090b);
    let rad = sc(2.0);
    let (ct, cb) = if hover {
        (0xb9c0c7, 0x565c63)
    } else {
        (0xa5acb3, 0x464c52)
    };
    dib.fill_round_rect_vgrad(
        cap,
        rad,
        if on { ct } else { dim(ct, 0.45) },
        if on { cb } else { dim(cb, 0.45) },
    );
    // 中央の溝（指をかけるところ）。
    let mid_y = cap.cy();
    dib.fill_rect(IR { l: cap.l + sc(1.0), t: mid_y - 1, r: cap.r - sc(1.0), b: mid_y + 1 }, 0x2b3036);
    dib.fill_rect(IR { l: cap.l + sc(1.0), t: mid_y + 1, r: cap.r - sc(1.0), b: mid_y + 2 }, 0xd7dce1);
    // 上下のミゾ（滑り止め）。
    for dy in [-sc(4.0), sc(4.0)] {
        dib.fill_rect(IR { l: cap.l + sc(3.0), t: mid_y + dy, r: cap.r - sc(3.0), b: mid_y + dy + 1 }, 0x353a40);
    }
    dib.bevel(cap, 0xd2d8de, 0x1b1e22);

    if hover && on {
        // つまみを触っている間は縁をアクセントで光らせる。
        stroke_round_rect(dc, cap.inset(sc(1.0)), rad, ACCENT_HI, 1);
    }
}

/// GR メーター（コンプの GR と同じく 0 から下へ振れる）。
fn paint_gr(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    bar: IR,
    scale_r: IR,
    num_r: IR,
    gr_db: f32,
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let rad = sc(2.0);

    // 見出し。
    unsafe {
        draw_engraved(
            dc,
            "GR",
            IR { t: bar.t - sc(14.0), b: bar.t - sc(2.0), ..bar },
            fonts.silk_small,
            SILK_FAINT,
            scale,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }

    // 0 dB の基準線。
    dib.fill_rect(IR { l: bar.l - sc(2.0), t: bar.t - 1, r: bar.r + sc(2.0), b: bar.t + 1 }, WARM);

    // バー（掘り込み + 1 dB 刻みのセグメント LED）。
    dib.fill_round_rect(bar, rad, 0x0a0c0e);
    dib.inner_shadow(bar, rad, 0x000000, sc(2.0), 0.7);

    const SEGS: i32 = 30;
    let seg = bar.h() as f32 / SEGS as f32;
    let lit = (gr_db / GR_DB_RANGE * SEGS as f32).clamp(0.0, SEGS as f32);
    for k in 0..SEGS {
        let y0 = bar.t + (k as f32 * seg).round() as i32;
        let y1 = bar.t + ((k + 1) as f32 * seg).round() as i32 - sc(1.0);
        if y1 <= y0 {
            continue;
        }
        let t = (k as f32 + 1.0) / SEGS as f32;
        // 深い抑圧ほど暖色〜赤。
        let c = if t > 0.8 {
            lerp_rgb(WARM, RED, (t - 0.8) / 0.2)
        } else {
            lerp_rgb(ACCENT, WARM, (t / 0.8).powf(1.4))
        };
        let r = IR { t: y0, b: y1, ..bar };
        if (k as f32) < lit {
            let head = (lit - k as f32).min(1.0);
            dib.fill_rect(r, lerp_rgb(0x14171a, c, 0.35 + 0.65 * head));
        } else {
            dib.fill_rect(r, 0x15181b);
        }
    }

    // 目盛り（0 / -6 / -12 / -18 / -24 / -30）。
    unsafe {
        for k in 0..=5 {
            let db = -(k as f32 * 6.0);
            let y = bar.t + (db.abs() / GR_DB_RANGE * bar.h() as f32).round() as i32;
            let x0 = bar.r + sc(2.0);
            stroke_line(dc, x0, y, x0 + sc(3.0), y, SILK_FAINT, 1);
            draw_text(
                dc,
                &format!("{}", db as i32),
                IR { l: x0 + sc(4.0), t: y - sc(6.0), r: scale_r.r, b: y + sc(6.0) },
                fonts.silk_small,
                SILK_FAINT,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE,
            );
        }
    }

    // 数値（現在の抑圧量）。
    let num_text = if gr_db < 0.05 {
        "0.0".to_string()
    } else {
        format!("-{gr_db:.1}")
    };
    unsafe {
        draw_text(
            dc,
            &num_text,
            IR {
                l: num_r.l,
                t: num_r.t + num_r.h() / 3 - sc(8.0),
                r: num_r.r,
                b: num_r.t + num_r.h() / 3 + sc(8.0),
            },
            fonts.digital,
            if gr_db > 0.05 { WARM_HI } else { VALUE_DIM },
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        draw_text(
            dc,
            "dB",
            IR {
                l: num_r.l,
                t: num_r.t + num_r.h() / 3 + sc(6.0),
                r: num_r.r,
                b: num_r.t + num_r.h() / 3 + sc(20.0),
            },
            fonts.silk_small,
            SILK_FAINT,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

/// 出力 3 バンド（低 / 中 / 高）の LED バー。
fn paint_bands(
    dib: &mut Dib,
    dc: HDC,
    fonts: &Fonts,
    scale: f32,
    bars: IR,
    labels: IR,
    levels: &[f32; 3],
    peaks: &[f32; 3],
) {
    let sc = |v: f32| (v * scale).round() as i32;
    let band_w = sc(K_BAND_W);
    let gap = sc(K_BAND_GAP);
    let rad = sc(2.0);
    const SEGS: i32 = 20; // 3 dB 刻み（-60 〜 0 dBFS）

    for b in 0..3usize {
        let x = bars.l + b as i32 * (band_w + gap);
        let r = IR { l: x, t: bars.t, r: x + band_w, b: bars.b };
        dib.fill_round_rect(r, rad, 0x0a0c0e);
        dib.inner_shadow(r, rad, 0x000000, sc(2.0), 0.7);

        let seg = bars.h() as f32 / SEGS as f32;
        let lit = (levels[b] * SEGS as f32).clamp(0.0, SEGS as f32);
        for k in 0..SEGS {
            // k = 0 が下端。
            let y1 = bars.b - (k as f32 * seg).round() as i32;
            let y0 = bars.b - ((k + 1) as f32 * seg).round() as i32 + sc(1.0);
            if y1 <= y0 {
                continue;
            }
            let db = -60.0 + 3.0 * (k as f32 + 1.0);
            let c = if db > -6.0 {
                RED
            } else if db > -18.0 {
                WARM
            } else {
                ACCENT
            };
            let seg_r = IR { t: y0, b: y1, ..r };
            if (k as f32) < lit {
                let head = (lit - k as f32).min(1.0);
                dib.fill_rect(seg_r, lerp_rgb(0x14171a, c, 0.35 + 0.65 * head));
            } else {
                dib.fill_rect(seg_r, 0x15181b);
            }
        }

        // ピークホールド（白い線）。
        let yp = bars.b - (peaks[b] * bars.h() as f32).round() as i32;
        dib.fill_rect(IR { t: yp, b: yp + sc(1.5).max(1), ..r }, 0xe9eef2);

        // ラベル。
        let names = ["L", "M", "H"];
        unsafe {
            draw_text(
                dc,
                names[b],
                IR { l: r.l, t: labels.t, r: r.r, b: labels.b },
                fonts.silk_small,
                SILK_FAINT,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
    }
}

/// ステータス（LED + 状態テキスト + モデル概要）。
fn paint_status(dib: &mut Dib, dc: HDC, fonts: &Fonts, s: &Snapshot, scale: f32) {
    let sc = |v: f32| (v * scale).round() as i32;
    let r = s.layout.status;

    let led_r = sc(4.0).max(3);
    let cx = r.l + led_r + sc(2.0);
    let cy = r.cy();

    // 点灯色: OK は緑、それ以外（未構築 / モデル無しなど）はアンバー。
    let (base, tint) = if s.status == crate::STATUS_OK {
        (LED_GREEN, 0x1d5f31)
    } else {
        (WARM, 0x6a4a15)
    };
    dib.fill_ellipse(cx, cy + 1, led_r + 1, 0x090a0c);
    dib.fill_ellipse(cx, cy, led_r, tint);
    dib.fill_ellipse_vgrad(cx, cy, led_r - 1, lighten(base, 0.45), base);
    dib.fill_ellipse(cx - 1, cy - 1, (led_r / 3).max(1), 0xf4fff6);

    let text_r = IR {
        l: cx + led_r + sc(8.0),
        ..s.layout.status_text
    };
    let (line1, c1) = match &s.flash {
        Some(f) => (f.as_str(), WARM_HI),
        None => (s.status_text.as_str(), SILK_DIM),
    };
    unsafe {
        draw_text(
            dc,
            line1,
            IR { t: text_r.t, b: text_r.cy() + sc(1.0), ..text_r },
            fonts.status,
            c1,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        draw_text(
            dc,
            &s.summary,
            IR { t: text_r.cy() + sc(1.0), b: text_r.b, ..text_r },
            fonts.silk_small,
            SILK_FAINT,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        // 使っているモデルの刻印（フッタ右端）。
        //
        // OBS 版はここに出す（VST3 版は MODEL キーが同じことを示すので出さない）。
        if !s.model_key {
            draw_engraved(
                dc,
                &format!("MODEL {}", s.model.as_str().to_uppercase()),
                IR { l: r.r - sc(K_FOOTER_KEY_W), t: r.cy() - sc(8.0), r: r.r, b: r.cy() + sc(8.0) },
                fonts.silk_small,
                if s.model == ModelChoice::Auto { SILK_FAINT } else { SILK_DIM },
                scale,
                DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
            );
        }
    }
}

// --------------------------------------------------------------------------
// GDI ヘルパ
// --------------------------------------------------------------------------

unsafe fn make_font(face: &str, height: f32, weight: i32) -> HFONT {
    let name: Vec<u16> = face.encode_utf16().chain(std::iter::once(0)).collect();
    let h = -(height.round().max(1.0) as i32);
    CreateFontW(
        h,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_TT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        // iPitchAndFamily: DEFAULT_PITCH (0) | FF_DONTCARE (0)。
        0,
        name.as_ptr(),
    )
}

unsafe fn select_font(dc: HDC, font: HFONT) -> HGDIOBJ {
    SelectObject(dc, font)
}

/// 彫り込み風の文字（下に 1 px の影 -> 本体）。静的ラベル用。
unsafe fn draw_engraved(
    dc: HDC,
    text: &str,
    r: IR,
    font: HFONT,
    color: u32,
    scale: f32,
    flags: u32,
) {
    let d = scale.round().max(1.0) as i32;
    draw_text(dc, text, IR { t: r.t + d, b: r.b + d, ..r }, font, ENGRAVE_HI, flags);
    draw_text(dc, text, r, font, color, flags);
}

unsafe fn draw_text(dc: HDC, text: &str, r: IR, font: HFONT, color: u32, flags: u32) {
    let old = select_font(dc, font);
    SetTextColor(dc, colorref(color));
    let w: Vec<u16> = text.encode_utf16().collect();
    let mut rc = RECT { left: r.l, top: r.t, right: r.r, bottom: r.b };
    DrawTextW(dc, w.as_ptr(), w.len() as i32, &mut rc, flags);
    SelectObject(dc, old);
}

unsafe fn stroke_line(dc: HDC, x0: i32, y0: i32, x1: i32, y1: i32, color: u32, width: i32) {
    let pen = CreatePen(PS_SOLID, width.max(1), colorref(color));
    if pen.0.is_null() {
        return;
    }
    let old = SelectObject(dc, pen);
    let _ = MoveToEx(dc, x0, y0, ptr::null_mut());
    let _ = LineTo(dc, x1, y1);
    SelectObject(dc, old);
    let _ = DeleteObject(pen);
}

unsafe fn stroke_round_rect(dc: HDC, r: IR, rad: i32, color: u32, width: i32) {
    let pen = CreatePen(PS_SOLID, width.max(1), colorref(color));
    if pen.0.is_null() {
        return;
    }
    let old_pen = SelectObject(dc, pen);
    let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
    let _ = RoundRect(dc, r.l, r.t, r.r, r.b, rad * 2, rad * 2);
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    let _ = DeleteObject(pen);
}

/// 弧（角度は 12 時方向を 0 として時計回り、度）。
unsafe fn stroke_arc(dc: HDC, cx: f32, cy: f32, rad: f32, a0: f32, a1: f32, color: u32, width: i32) {
    let steps = (((a1 - a0).abs() / 4.0).ceil() as i32).max(2);
    let pen = CreatePen(PS_SOLID, width.max(1), colorref(color));
    if pen.0.is_null() {
        return;
    }
    let old = SelectObject(dc, pen);
    for i in 0..=steps {
        let a = (a0 + (a1 - a0) * i as f32 / steps as f32).to_radians();
        let x = (cx + rad * a.sin()).round() as i32;
        let y = (cy - rad * a.cos()).round() as i32;
        if i == 0 {
            let _ = MoveToEx(dc, x, y, ptr::null_mut());
        } else {
            let _ = LineTo(dc, x, y);
        }
    }
    SelectObject(dc, old);
    let _ = DeleteObject(pen);
}

/// DIB の中身を 32 bit BMP として書き出す（検証用の画像ダンプ）。
pub(crate) fn write_bmp(dib: &Dib, path: &std::path::Path) -> std::io::Result<()> {
    let (w, h) = (dib.w, dib.h);
    let pix = (w * h * 4) as u32;
    let mut out: Vec<u8> = Vec::with_capacity((54 + pix) as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + pix).to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&pix.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    // DIB は top-down、BMP は bottom-up。
    unsafe {
        for y in (0..h).rev() {
            let row = std::slice::from_raw_parts(dib.bits.add((y * w) as usize), w as usize);
            for &px in row {
                out.push((px & 0xFF) as u8);
                out.push(((px >> 8) & 0xFF) as u8);
                out.push(((px >> 16) & 0xFF) as u8);
                out.push(0xFF);
            }
        }
    }
    std::fs::write(path, out)
}

// --------------------------------------------------------------------------
// オフラインのプレビュー（テスト）
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn lin(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    /// テスト用の 1 枚。`Snapshot` は文字列を多く持つので、既定値を埋める。
    fn base_snapshot(scale: f32, layout: Layout) -> Snapshot {
        Snapshot {
            scale,
            layout,
            reading: Reading::default(),
            vref: 18.0,
            bands: [0.0; 3],
            bands_peak: [0.0; 3],
            values: [
                dsp::AMBIENCE_DEFAULT as f32,
                dsp::VOICE_DEFAULT as f32,
                dsp::VOICE_REVERB_DEFAULT as f32,
            ],
            fader_text: [
                fader_text(0, dsp::AMBIENCE_DEFAULT as f32),
                fader_text(1, dsp::VOICE_DEFAULT as f32),
                fader_text(2, dsp::VOICE_REVERB_DEFAULT as f32),
            ],
            modes: [dsp::FeatureMode::Active; 3],
            status: crate::STATUS_OK,
            status_text: String::new(),
            summary: String::new(),
            flash: None,
            dereverb: false,
            channels: 0,
            rate: 0,
            enabled: true,
            model: ModelChoice::Auto,
            slots: [false; SLOTS],
            current_slot: None,
            save_armed: false,
            hover_slot: None,
            hover_save: false,
            hover_button: false,
            press_button: false,
            hover_fader: None,
            hover_mute: None,
            hover_byp: None,
            hover_model: false,
            hover_device: false,
            model_key: false,
            device_key: false,
            model_label: "MODEL AUTO".into(),
            device_label: "DEV CPU".into(),
        }
    }

    /// 1 枚描く（`DPDFNET_SNAPSHOT_DIR` があれば BMP も残す）。
    fn render(name: &str, mut s: Snapshot, model_key: bool, device_key: bool) {
        let scale = s.scale;
        let w = (CLIENT_W * scale).round() as i32;
        let h = (CLIENT_H * scale).round() as i32;
        let mut dib = unsafe { Dib::new(w, h).expect("DIB") };
        let fonts = unsafe { Fonts::new(scale) };
        s.layout = compute_layout(scale, w, h, model_key, device_key);
        s.model_key = model_key;
        s.device_key = device_key;
        paint(&mut dib, &fonts, &s);

        let Some(dir) = std::env::var_os("DPDFNET_SNAPSHOT_DIR") else {
            return;
        };
        let path = PathBuf::from(dir).join(format!("{name}.bmp"));
        if let Err(e) = write_bmp(&dib, &path) {
            panic!("BMP を書けません {}: {e}", path.display());
        }
    }

    fn demo(scale: f32) -> Snapshot {
        let layout = compute_layout(scale, (CLIENT_W * scale) as i32, (CLIENT_H * scale) as i32, false, false);
        let mut s = base_snapshot(scale, layout);
        s.reading = Reading {
            vu_in_lin: lin(-20.5),
            vu_in_db: -20.5,
            vu_out_lin: lin(-32.5),
            vu_out_db: -32.5,
            peak_in_db: -17.8,
            peak_out_db: -29.4,
            gr_db: 11.6,
        };
        s.bands = [0.72, 0.45, 0.28];
        s.bands_peak = [0.78, 0.50, 0.31];
        s.values = [0.62, 3.5, 0.28];
        s.fader_text = [fader_text(0, 0.62), fader_text(1, 3.5), fader_text(2, 0.28)];
        s.modes[0] = dsp::FeatureMode::Active;
        s.model = ModelChoice::Quality;
        s.channels = 2;
        s.dereverb = true;
        s.slots[0] = true;
        s.slots[2] = true;
        s.slots[6] = true;
        s.current_slot = Some(2);
        s.status_text =
            "48 kHz / ch=2 / latency 2880 samples / dpdfnet8_48khz_hr_2out.onnx".into();
        s.summary = "2 ch  ·  48 kHz  ·  遅延 2880 samples (60 ms)  ·  dereverb あり  ·  model dp8"
            .into();
        s.rate = 48_000;
        s
    }

    #[test]
    fn snapshot_panel_2ch() {
        let s = demo(1.0);
        // OBS 版（キー無し）と VST3 版（キーあり）の両方を残す。
        render("panel-2ch", s.clone(), false, false);
        render("panel-vst3-2ch", s, true, true);
    }

    #[test]
    fn snapshot_panel_mono_save_armed() {
        let mut s = demo(1.0);
        // モノラル + SAVE 待機 + モデル auto + 基準 -20 dBFS。
        s.channels = 1;
        s.model = ModelChoice::Auto;
        s.model_label = "MODEL AUTO".into();
        s.vref = 20.0;
        s.summary = "1 ch  ·  48 kHz  ·  遅延 2880 samples (60 ms)  ·  dereverb あり  ·  model auto"
            .into();
        s.save_armed = true;
        s.hover_slot = Some(4);
        s.hover_button = true;
        render("panel-mono-save", s, false, false);
    }

    /// MUTE / BYP が灯った状態（AMBIENCE をミュート、VOICE をバイパス）。
    #[test]
    fn snapshot_panel_modes() {
        let mut s = demo(1.0);
        s.modes[0] = dsp::FeatureMode::Mute;
        s.modes[1] = dsp::FeatureMode::Bypass;
        s.hover_mute = Some(2);
        s.flash = Some("VOICE: バイパス — presence を掛けません (0 dB)".into());
        render("panel-modes", s, false, false);
    }

    #[test]
    fn snapshot_panel_hover_drag() {
        let mut s = demo(1.0);
        s.hover_fader = Some(1);
        s.flash = Some("MEMORY 3 を呼び出しました".into());
        // REVERB 無効（1 出力モデル）。
        s.dereverb = false;
        s.values[2] = 0.0;
        s.fader_text[2] = fader_text(2, 0.0);
        s.summary = "2 ch  ·  48 kHz  ·  遅延 2880 samples (60 ms)  ·  dereverb なし  ·  model dp8"
            .into();
        render("panel-hover-flash", s, false, false);
    }

    /// 画面の大きさを変えても破綻しないこと（レイアウトの算術チェック）。
    ///
    /// キーの有無の 4 通りすべてで確かめる（OBS 版は矩形だけ計算するので、
    /// 描かなくてもはみ出してはいけない）。
    #[test]
    fn layout_stays_inside() {
        for (cw, chh) in [(700.0, 360.0), (780.0, 400.0), (1100.0, 620.0)] {
            for (mk, dk) in [(false, false), (true, false), (false, true), (true, true)] {
                let l = compute_layout(1.0, cw as i32, chh as i32, mk, dk);
                for r in [
                    l.title,
                    l.status,
                    l.status_text,
                    l.button,
                    l.model_key,
                    l.device_key,
                    l.gr,
                    l.gr_scale,
                    l.gr_num,
                    l.bands,
                    l.save_btn,
                    l.memory_label,
                ]
                .into_iter()
                .chain(l.slot)
                .chain(l.section)
                .chain(l.vu)
                .chain(l.vu_num)
                .chain(l.fader)
                .chain(l.fader_track)
                .chain(l.label)
                .chain(l.value_db)
                .chain(l.value_pc)
                .chain(l.mute)
                .chain(l.byp)
                .chain([l.bands_label])
                {
                    assert!(
                        r.l >= 0 && r.t >= 0 && r.r <= cw as i32 && r.b <= chh as i32,
                        "はみ出し: {r:?} ({cw}x{chh}, model_key={mk}, device_key={dk})"
                    );
                    assert!(r.r > r.l && r.b > r.t, "潰れた: {r:?}");
                }
            }
        }
    }

    /// キーの有無でステータスの文字幅だけが変わり、他の配置は動かないこと。
    #[test]
    fn footer_keys_do_not_move_the_rest_of_the_panel() {
        let obs = compute_layout(1.0, 780, 400, false, false);
        let vst = compute_layout(1.0, 780, 400, true, true);
        assert_eq!(obs.status, vst.status);
        assert_eq!(obs.button, vst.button);
        assert_eq!(obs.title, vst.title);
        assert_eq!(obs.section, vst.section);
        // キー 2 個ぶん右を詰める。
        assert_eq!(obs.status_text.r - vst.status_text.r, 76);
    }

    #[test]
    fn fader_text_matches_range() {
        // AMBIENCE 100 % = 6 dB までしか抑えない / 0 % = 48 dB。
        assert!(fader_text(0, 1.0).0.starts_with("-6.0"));
        assert!(fader_text(0, 0.0).0.starts_with("-48.0"));
        assert_eq!(fader_text(0, 0.5).1, "50 %");
        assert_eq!(fader_text(1, 0.0).0, "+0.0 dB");
        assert_eq!(fader_text(1, 12.0).1, "100 %");
        assert_eq!(fader_text(2, 1.0).0, "100 %");
        assert_eq!(fader_text(2, 0.0).1, "-∞ dB");
    }

    #[test]
    fn mode_messages_name_the_effect() {
        assert_eq!(
            mode_message(0, dsp::FeatureMode::Active),
            "AMBIENCE: 有効（つまみの値）"
        );
        // フェーダーごとに効きが違うので、押した直後の文章も違うこと。
        assert!(mode_message(0, dsp::FeatureMode::Mute).contains("暗騒音を残しません"));
        assert!(mode_message(0, dsp::FeatureMode::Bypass).contains("100 %"));
        assert!(mode_message(1, dsp::FeatureMode::Mute).contains("-12 dB"));
        assert!(mode_message(1, dsp::FeatureMode::Bypass).contains("0 dB"));
        assert!(mode_message(2, dsp::FeatureMode::Mute).contains("残響を残しません"));
        assert!(mode_message(2, dsp::FeatureMode::Bypass).contains("100 %"));
    }

    #[test]
    fn vu_reference_shifts_the_scale() {
        // 0 VU = -18 dBFS なら -38 dBFS が左端。
        assert!(vu_to_pos(lin(-38.0), 18.0) < 0.01);
        assert!(vu_to_pos(lin(-15.0), 18.0) > 0.99);
        // 基準を -14 dBFS にすると同じ値でも針は下がる。
        let a = vu_to_pos(lin(-20.0), 18.0);
        let b = vu_to_pos(lin(-20.0), 14.0);
        assert!(b < a, "-20 dBFS の針位置: {a} -> {b}");
    }
}


