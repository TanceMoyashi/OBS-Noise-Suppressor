//! libobs (OBS Studio 32.2.2) への手書き FFI バインディング。
//!
//! bindgen を使わない理由:
//!   `obs.h` は `graphics/graphics.h` 等を巻き込むため、ヘッダのクロージャを
//!   揃えるだけで SDK 一式が必要になる。実際に必要なのは
//!     ・モジュールエクスポート 3 つ
//!     ・`obs_source_info` のレイアウト
//!     ・`filter_audio` / `obs_properties_*` / `obs_data_*` の数個
//!   だけなので、手書きの方が依存もビルドも軽い。
//!
//! レイアウトの正しさはファイル末尾の `offset_of!` アサーションで
//! コンパイル時に検証している。期待値は OBS 32.2.2 の `libobs/obs-source.h` を
//! MSVC でコンパイルして実測したもの。

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::os::raw::{c_char, c_int, c_void};

// --------------------------------------------------------------------------
// 不透明型
// --------------------------------------------------------------------------

pub type obs_module_t = c_void;
pub type obs_data_t = c_void;
pub type obs_source_t = c_void;
pub type obs_properties_t = c_void;
pub type obs_property_t = c_void;
pub type obs_source_frame = c_void;
pub type obs_source_audio_mix = c_void;
pub type audio_output_data = c_void;
pub type gs_effect_t = c_void;
pub type obs_missing_files_t = c_void;
pub type obs_mouse_event = c_void;
pub type obs_key_event = c_void;
pub type gs_color_space = c_int;
pub type lookup_t = c_void;

// --------------------------------------------------------------------------
// 定数
// --------------------------------------------------------------------------

pub const MAX_AV_PLANES: usize = 8;
pub const MAX_CHANNELS: usize = 64;

pub const OBS_SOURCE_TYPE_INPUT: c_int = 0;
pub const OBS_SOURCE_TYPE_FILTER: c_int = 1;
pub const OBS_SOURCE_TYPE_TRANSITION: c_int = 2;
pub const OBS_SOURCE_TYPE_SCENE: c_int = 3;

pub const OBS_SOURCE_VIDEO: u32 = 1 << 0;
pub const OBS_SOURCE_AUDIO: u32 = 1 << 1;
pub const OBS_SOURCE_ASYNC: u32 = 1 << 2;

/// `libobs/obs-config.h` の `LIBOBS_API_VER`。
///
/// libobs は「自分より新しい API でビルドされたプラグイン」だけを拒否する
/// (`obs-module.c`: `ver & 0xFFFF0000 > LIBOBS_API_VER`。パッチ番号は無視)。
/// よってこの値は対象 OBS と同一かそれ以下ならよい。
pub const LIBOBS_API_VER: u32 = (32 << 24) | (2 << 16) | 2;

pub const LOG_ERROR: c_int = 100;
pub const LOG_WARNING: c_int = 200;
pub const LOG_INFO: c_int = 300;
pub const LOG_DEBUG: c_int = 400;

pub const OBS_PROPERTY_BOOL: c_int = 1;
pub const OBS_PROPERTY_FLOAT: c_int = 3;
pub const OBS_TEXT_INFO: c_int = 3;

/// `enum obs_combo_type` (libobs/obs-properties.h)。0 = INVALID / 1 = EDITABLE /
/// 2 = LIST / 3 = RADIO。**値はヘッダの並び順そのまま**。
pub const OBS_COMBO_TYPE_LIST: c_int = 2;
/// `enum obs_combo_format`。0 = INVALID / 1 = INT / 2 = FLOAT / 3 = STRING。
pub const OBS_COMBO_FORMAT_STRING: c_int = 3;

/// `enum speaker_layout` (media-io/audio-io.h)
pub const SPEAKERS_UNKNOWN: c_int = 0;
pub const SPEAKERS_MONO: c_int = 1;
pub const SPEAKERS_STEREO: c_int = 2;

// --------------------------------------------------------------------------
// 構造体
// --------------------------------------------------------------------------

/// フィルタに渡される音声バッファ。planar float32。
///
/// チャンネル数はフィールドとして存在しないため、`data[i]` が非 NULL の
/// 個数を数えて判定する（OBS の音声フィルタの通例）。
#[repr(C)]
pub struct obs_audio_data {
    pub data: [*mut u8; MAX_AV_PLANES],
    pub frames: u32,
    pub timestamp: u64,
}

pub type obs_property_clicked_t =
    Option<unsafe extern "C" fn(props: *mut obs_properties_t, property: *mut obs_property_t, data: *mut c_void) -> bool>;

/// `struct obs_source_info` (OBS 32.2.2)。
///
/// フィールド順・型は `libobs/obs-source.h` と完全に同一であること。
/// libobs は渡された `size` バイトだけをコピーするが、`info->version` は
/// `size` に関係なく構造体の実オフセットから読む。そのため「途中まで」の
/// 定義では危険があり、ここでは全体を定義して `version` まで正しく置く。
#[repr(C)]
pub struct obs_source_info {
    pub id: *const c_char,
    pub type_: c_int,
    pub output_flags: u32,

    pub get_name: Option<unsafe extern "C" fn(type_data: *mut c_void) -> *const c_char>,
    pub create: Option<unsafe extern "C" fn(settings: *mut obs_data_t, source: *mut obs_source_t) -> *mut c_void>,
    pub destroy: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub get_width: Option<unsafe extern "C" fn(data: *mut c_void) -> u32>,
    pub get_height: Option<unsafe extern "C" fn(data: *mut c_void) -> u32>,

    pub get_defaults: Option<unsafe extern "C" fn(settings: *mut obs_data_t)>,
    pub get_properties: Option<unsafe extern "C" fn(data: *mut c_void) -> *mut obs_properties_t>,
    pub update: Option<unsafe extern "C" fn(data: *mut c_void, settings: *mut obs_data_t)>,
    pub activate: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub deactivate: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub show: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub hide: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub video_tick: Option<unsafe extern "C" fn(data: *mut c_void, seconds: f32)>,
    pub video_render: Option<unsafe extern "C" fn(data: *mut c_void, effect: *mut gs_effect_t)>,
    pub filter_video:
        Option<unsafe extern "C" fn(data: *mut c_void, frame: *mut obs_source_frame) -> *mut obs_source_frame>,
    pub filter_audio:
        Option<unsafe extern "C" fn(data: *mut c_void, audio: *mut obs_audio_data) -> *mut obs_audio_data>,

    pub enum_active_sources:
        Option<unsafe extern "C" fn(data: *mut c_void, cb: *mut c_void, param: *mut c_void)>,
    pub save: Option<unsafe extern "C" fn(data: *mut c_void, settings: *mut obs_data_t)>,
    pub load: Option<unsafe extern "C" fn(data: *mut c_void, settings: *mut obs_data_t)>,
    pub mouse_click: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            event: *const obs_mouse_event,
            type_: c_int,
            mouse_up: bool,
            click_count: u32,
        ),
    >,
    pub mouse_move:
        Option<unsafe extern "C" fn(data: *mut c_void, event: *const obs_mouse_event, mouse_leave: bool)>,
    pub mouse_wheel:
        Option<unsafe extern "C" fn(data: *mut c_void, event: *const obs_mouse_event, x: c_int, y: c_int)>,
    pub focus: Option<unsafe extern "C" fn(data: *mut c_void, focus: bool)>,
    pub key_click: Option<unsafe extern "C" fn(data: *mut c_void, event: *const obs_key_event, key_up: bool)>,
    pub filter_remove: Option<unsafe extern "C" fn(data: *mut c_void, source: *mut obs_source_t)>,
    pub type_data: *mut c_void,
    pub free_type_data: Option<unsafe extern "C" fn(type_data: *mut c_void)>,
    pub audio_render: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            ts_out: *mut u64,
            audio_output: *mut obs_source_audio_mix,
            mixers: u32,
            channels: usize,
            sample_rate: usize,
        ) -> bool,
    >,
    pub enum_all_sources:
        Option<unsafe extern "C" fn(data: *mut c_void, cb: *mut c_void, param: *mut c_void)>,
    pub transition_start: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub transition_stop: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub get_defaults2: Option<unsafe extern "C" fn(type_data: *mut c_void, settings: *mut obs_data_t)>,
    pub get_properties2:
        Option<unsafe extern "C" fn(data: *mut c_void, type_data: *mut c_void) -> *mut obs_properties_t>,
    pub audio_mix: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            ts_out: *mut u64,
            audio_output: *mut audio_output_data,
            channels: usize,
            sample_rate: usize,
        ) -> bool,
    >,
    pub icon_type: c_int,

    pub media_play_pause: Option<unsafe extern "C" fn(data: *mut c_void, pause: bool)>,
    pub media_restart: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub media_stop: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub media_next: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub media_previous: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub media_get_duration: Option<unsafe extern "C" fn(data: *mut c_void) -> i64>,
    pub media_get_time: Option<unsafe extern "C" fn(data: *mut c_void) -> i64>,
    pub media_set_time: Option<unsafe extern "C" fn(data: *mut c_void, milliseconds: i64)>,
    pub media_get_state: Option<unsafe extern "C" fn(data: *mut c_void) -> c_int>,
    pub version: u32,
    pub unversioned_id: *const c_char,

    pub missing_files: Option<unsafe extern "C" fn(data: *mut c_void) -> *mut obs_missing_files_t>,
    pub video_get_color_space: Option<
        unsafe extern "C" fn(data: *mut c_void, count: usize, preferred: *const gs_color_space) -> gs_color_space,
    >,
    pub filter_add: Option<unsafe extern "C" fn(data: *mut c_void, source: *mut obs_source_t)>,
    pub get_dark_icon: Option<unsafe extern "C" fn(type_data: *mut c_void) -> *const c_char>,
    pub get_light_icon: Option<unsafe extern "C" fn(type_data: *mut c_void) -> *const c_char>,
}

impl obs_source_info {
    /// 全フィールドをゼロで埋めたインスタンス。
    pub const fn zeroed() -> Self {
        // SAFETY: すべてのフィールドがゼロ (=NULL / 0) で有効な「未設定」を表す。
        unsafe { std::mem::zeroed() }
    }
}

// SAFETY: この構造体は libobs に登録した後、libobs が `memcpy` で読み取るだけの
// 不変データ（関数ポインタと静的 C 文字列）である。共有しても内部可変状態は無い。
unsafe impl Send for obs_source_info {}
unsafe impl Sync for obs_source_info {}

// --------------------------------------------------------------------------
// モジュール / ソース / 設定
// --------------------------------------------------------------------------

extern "C" {
    /// 可変長。呼び出しは安定、定義のみ unstable。
    pub fn blog(level: c_int, format: *const c_char, ...);

    pub fn bmalloc(size: usize) -> *mut c_void;
    pub fn bfree(ptr: *mut c_void);

    pub fn obs_register_source_s(info: *const obs_source_info, size: usize);

    pub fn obs_data_get_double(data: *mut obs_data_t, name: *const c_char) -> f64;
    pub fn obs_data_set_double(data: *mut obs_data_t, name: *const c_char, value: f64);
    pub fn obs_data_get_bool(data: *mut obs_data_t, name: *const c_char) -> bool;
    pub fn obs_data_set_bool(data: *mut obs_data_t, name: *const c_char, value: bool);
    pub fn obs_data_set_default_double(data: *mut obs_data_t, name: *const c_char, value: f64);
    pub fn obs_data_set_default_bool(data: *mut obs_data_t, name: *const c_char, value: bool);
    pub fn obs_data_set_string(data: *mut obs_data_t, name: *const c_char, value: *const c_char);
    pub fn obs_data_get_string(data: *mut obs_data_t, name: *const c_char) -> *const c_char;

    pub fn obs_properties_create() -> *mut obs_properties_t;
    pub fn obs_properties_add_bool(
        props: *mut obs_properties_t,
        name: *const c_char,
        description: *const c_char,
    ) -> *mut obs_property_t;
    pub fn obs_properties_add_float_slider(
        props: *mut obs_properties_t,
        name: *const c_char,
        description: *const c_char,
        min: f64,
        max: f64,
        step: f64,
    ) -> *mut obs_property_t;
    pub fn obs_properties_add_text(
        props: *mut obs_properties_t,
        name: *const c_char,
        description: *const c_char,
        type_: c_int,
    ) -> *mut obs_property_t;
    pub fn obs_properties_add_button2(
        props: *mut obs_properties_t,
        name: *const c_char,
        text: *const c_char,
        callback: obs_property_clicked_t,
        priv_: *mut c_void,
    ) -> *mut obs_property_t;
    pub fn obs_properties_add_group(
        props: *mut obs_properties_t,
        name: *const c_char,
        description: *const c_char,
        type_: c_int,
        group: *mut obs_properties_t,
    ) -> *mut obs_property_t;

    /// ドロップダウン（`type_` = `OBS_COMBO_TYPE_LIST` / `format` = `OBS_COMBO_FORMAT_STRING`）。
    pub fn obs_properties_add_list(
        props: *mut obs_properties_t,
        name: *const c_char,
        description: *const c_char,
        type_: c_int,
        format: c_int,
    ) -> *mut obs_property_t;
    /// リストに 1 項目足す。戻り値は追加後の項目数（使わない）。
    pub fn obs_property_list_add_string(
        p: *mut obs_property_t,
        name: *const c_char,
        val: *const c_char,
    ) -> usize;
    /// リスト項目を使えない状態にする（プラグインに同梱されていないモデルなど）。
    pub fn obs_property_list_item_disable(p: *mut obs_property_t, idx: usize, disabled: bool);

    pub fn obs_property_set_long_description(p: *mut obs_property_t, long_description: *const c_char);
    pub fn obs_property_set_visible(p: *mut obs_property_t, visible: bool);
    pub fn obs_property_float_set_suffix(p: *mut obs_property_t, suffix: *const c_char);

    pub fn obs_source_get_settings(source: *mut obs_source_t) -> *mut obs_data_t;
    pub fn obs_source_get_name(source: *mut obs_source_t) -> *const c_char;
    pub fn obs_source_update_properties(source: *mut obs_source_t);

    /// `obs_module_file()` マクロの実体。戻り値は `bfree` で解放する。
    pub fn obs_find_module_file(module: *mut obs_module_t, file: *const c_char) -> *mut c_char;

    /// `obs_module_config_path()` マクロの実体（`%APPDATA%\obs-studio\plugin_config\<module>\`）。
    /// 戻り値は `bfree` で解放する。ディレクトリは libobs 側が作る。
    pub fn obs_module_get_config_path(module: *mut obs_module_t, file: *const c_char) -> *mut c_char;

    // -- 音声設定（フィルタのチャンネル数/サンプルレートを知る） ---------------
    pub fn obs_get_audio() -> *mut c_void;
    pub fn audio_output_get_channels(audio: *mut c_void) -> u32;
    pub fn audio_output_get_sample_rate(audio: *mut c_void) -> u32;

    // -- フィルタの親ソース -------------------------------------------------
    pub fn obs_filter_get_parent(source: *mut obs_source_t) -> *mut obs_source_t;

    // -- プロパティ ---------------------------------------------------------
    pub fn obs_properties_add_float(
        props: *mut obs_properties_t,
        name: *const c_char,
        description: *const c_char,
        min: f64,
        max: f64,
        step: f64,
    ) -> *mut obs_property_t;
    pub fn obs_property_float_set_limits(
        p: *mut obs_property_t,
        min: f64,
        max: f64,
        step: f64,
    );
    pub fn obs_property_set_description(p: *mut obs_property_t, description: *const c_char);
    pub fn obs_data_set_default_string(
        data: *mut obs_data_t,
        name: *const c_char,
        value: *const c_char,
    );

    // -- 設定の書き戻し（パネル UI から） -----------------------------------
    pub fn obs_data_create() -> *mut obs_data_t;
    pub fn obs_data_release(data: *mut obs_data_t);
    pub fn obs_source_update(source: *mut obs_source_t, settings: *mut obs_data_t);

    // -- フィルタの有効/無効（OBS 標準のバイパス） ---------------------------
    pub fn obs_source_enabled(source: *mut obs_source_t) -> bool;
    pub fn obs_source_set_enabled(source: *mut obs_source_t, enabled: bool);
}

// --------------------------------------------------------------------------
// ログヘルパ
// --------------------------------------------------------------------------

/// `blog()` を呼ぶ可変長ヘルパ。書式は固定の `"[obs-noise-suppressor] %s"`。
pub fn log(level: c_int, message: &std::ffi::CStr) {
    unsafe {
        blog(level, c"[obs-noise-suppressor] %s".as_ptr(), message.as_ptr());
    }
}

pub fn log_cstr(level: c_int, message: &std::ffi::CStr) {
    log(level, message);
}

/// マクロで包むための簡易版。
#[macro_export]
macro_rules! obs_log {
    ($level:expr, $($arg:tt)*) => {{
        let s = ::std::ffi::CString::new(format!($($arg)*)).unwrap_or_else(|_| {
            ::std::ffi::CString::new("<log format error>").unwrap()
        });
        $crate::ffi::log_cstr($level, &s);
    }};
}

// --------------------------------------------------------------------------
// レイアウト検証（OBS 32.2.2 で実測した値）
// --------------------------------------------------------------------------

const _: () = {
    use std::mem::{offset_of, size_of};

    assert!(size_of::<obs_audio_data>() == 80);
    assert!(offset_of!(obs_audio_data, frames) == 64);
    assert!(offset_of!(obs_audio_data, timestamp) == 72);

    assert!(size_of::<obs_source_info>() == 424);
    assert!(offset_of!(obs_source_info, id) == 0);
    assert!(offset_of!(obs_source_info, type_) == 8);
    assert!(offset_of!(obs_source_info, output_flags) == 12);
    assert!(offset_of!(obs_source_info, get_name) == 16);
    assert!(offset_of!(obs_source_info, create) == 24);
    assert!(offset_of!(obs_source_info, destroy) == 32);
    assert!(offset_of!(obs_source_info, get_width) == 40);
    assert!(offset_of!(obs_source_info, get_height) == 48);
    assert!(offset_of!(obs_source_info, get_defaults) == 56);
    assert!(offset_of!(obs_source_info, get_properties) == 64);
    assert!(offset_of!(obs_source_info, update) == 72);
    assert!(offset_of!(obs_source_info, activate) == 80);
    assert!(offset_of!(obs_source_info, deactivate) == 88);
    assert!(offset_of!(obs_source_info, show) == 96);
    assert!(offset_of!(obs_source_info, hide) == 104);
    assert!(offset_of!(obs_source_info, video_tick) == 112);
    assert!(offset_of!(obs_source_info, video_render) == 120);
    assert!(offset_of!(obs_source_info, filter_video) == 128);
    assert!(offset_of!(obs_source_info, filter_audio) == 136);
    assert!(offset_of!(obs_source_info, type_data) == 216);
    assert!(offset_of!(obs_source_info, icon_type) == 288);
    assert!(offset_of!(obs_source_info, version) == 368);
    assert!(offset_of!(obs_source_info, unversioned_id) == 376);
    assert!(offset_of!(obs_source_info, get_dark_icon) == 408);
    assert!(offset_of!(obs_source_info, get_light_icon) == 416);
};
