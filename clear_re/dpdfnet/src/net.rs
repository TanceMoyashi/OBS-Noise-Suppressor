//! DPDFNet の ONNX セッション 1 本ぶんのラッパ。
//!
//! 移植元: `clear_re/plugin/src/DpdfNet.cpp`。
//!
//! 設計上の要点（オーディオスレッドで一切確保しない）:
//!   * テンソルはすべて `CreateTensorWithDataAsOrtValue` で**自分が確保した
//!     バッファを指す**形で作り、実行時は `IoBinding` で入出力を差し替えるだけ。
//!   * 状態 (`state_in` -> `state_out`) はバッファ A/B を交互に使う 2 本の
//!     binding で ping-pong する。モデルの出力をそのまま次の入力にできる。
//!   * `RunWithBinding` は確保を行わない（ORT 内部アリーナは warmup で成長済み）。

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr;

use ort_sys::{
    ONNXTensorElementDataType, OrtAllocator, OrtAllocatorType, OrtApi, OrtEnv, OrtIoBinding,
    OrtMemType, OrtMemoryInfo, OrtModelMetadata, OrtRunOptions, OrtSession, OrtSessionOptions,
    OrtValue, ExecutionMode, GraphOptimizationLevel,
};

use super::meta::ModelMeta;
use super::ort;
use crate::paths::DeviceChoice;

const SPEC_RANK: usize = 4;
const SPEC_COUNT_RANK: usize = 4;

pub struct DpdfNet {
    api: &'static OrtApi,
    session: *mut OrtSession,
    mem: *mut OrtMemoryInfo,
    /// 出力用のメモリ情報（`OrtMemTypeCPUOutput`）。GPU のときだけ作る。
    ///
    /// これが無いと、DML は出力を**デバイス側に確保して書き込み**、こちらが
    /// 用意した CPU バッファは空のままになる（症状: 出力が無音同然）。
    /// CPU EP では不要なので `null`（= `mem` を使う）。
    mem_out: *mut OrtMemoryInfo,
    /// このセッションの推論デバイス（出力のメモリ種別を決めるのに使う）。
    device: DeviceChoice,
    /// GPU のときだけ入る入出力名（毎 hop 束ね直すために要る）。
    ///
    /// CPU は束ねを使い回せる（実機でビット一致を確認した経路をそのまま
    /// 使いたい）ので `None`。
    names: Option<IoNames>,

    binding_a: *mut OrtIoBinding,
    binding_b: *mut OrtIoBinding,

    spec_value: *mut OrtValue,
    out_full_value: *mut OrtValue,
    out_ns_value: *mut OrtValue,
    state_value_a: *mut OrtValue,
    state_value_b: *mut OrtValue,

    // これらは **再確保しない**。ORT のテンソルが生ポインタで指しているため。
    spec_buf: Vec<f32>,
    out_full_buf: Vec<f32>,
    out_ns_buf: Vec<f32>,
    state_buf_a: Vec<f32>,
    state_buf_b: Vec<f32>,

    state_in_a: bool,
    has_ns_output: bool,
    loaded: bool,
    last_error: String,

    pub meta: ModelMeta,
    freq_bins: usize,
    state_size: usize,
}

impl DpdfNet {
    pub fn load(
        model_path: &Path,
        n_threads: i32,
        device: DeviceChoice,
        ort_dirs: &[PathBuf],
    ) -> Result<Self, String> {
        let rt = ort::ensure_loaded(device, ort_dirs)?;
        let api = rt.api();
        let env_addr = rt.env_addr();

        let mut me = Self {
            api,
            session: ptr::null_mut(),
            mem: ptr::null_mut(),
            mem_out: ptr::null_mut(),
            device,
            binding_a: ptr::null_mut(),
            binding_b: ptr::null_mut(),
            spec_value: ptr::null_mut(),
            out_full_value: ptr::null_mut(),
            out_ns_value: ptr::null_mut(),
            state_value_a: ptr::null_mut(),
            state_value_b: ptr::null_mut(),
            spec_buf: Vec::new(),
            out_full_buf: Vec::new(),
            out_ns_buf: Vec::new(),
            state_buf_a: Vec::new(),
            state_buf_b: Vec::new(),
            state_in_a: true,
            has_ns_output: false,
            loaded: false,
            names: None,
            last_error: String::new(),
            meta: ModelMeta::default(),
            freq_bins: 0,
            state_size: 0,
        };

        // ここから先で失敗しても Drop が後始末する。
        me.create_session(env_addr as *const OrtEnv, model_path, n_threads, device)?;
        me.setup_bindings()?;
        me.run_warmup();
        Ok(me)
    }

    pub fn has_dereverb_output(&self) -> bool {
        self.has_ns_output
    }

    // 以下はテスト（`dsp::offline_tests`）から状態を覗くためのアクセサ。
    #[cfg(test)]
    pub fn last_error(&self) -> &str {
        &self.last_error
    }

    #[cfg(test)]
    pub fn state_size(&self) -> usize {
        self.state_size
    }

    #[cfg(test)]
    pub fn freq_bins(&self) -> usize {
        self.freq_bins
    }

    // ----------------------------------------------------------------------
    // ロード
    // ----------------------------------------------------------------------

    fn create_session(
        &mut self,
        env: *const OrtEnv,
        model_path: &Path,
        n_threads: i32,
        device: DeviceChoice,
    ) -> Result<(), String> {
        let api = self.api;

        let mut options: *mut OrtSessionOptions = ptr::null_mut();
        ort::check(api, unsafe { (api.CreateSessionOptions)(&mut options) })?;

        macro_rules! opt {
            ($call:expr) => {
                ort::check(api, $call)?
            };
        }

        opt!(unsafe { (api.SetIntraOpNumThreads)(options, n_threads) });
        opt!(unsafe { (api.SetInterOpNumThreads)(options, 1) });
        opt!(unsafe {
            (api.SetSessionGraphOptimizationLevel)(options, GraphOptimizationLevel::ORT_ENABLE_ALL)
        });
        opt!(unsafe { (api.SetSessionExecutionMode)(options, ExecutionMode::ORT_SEQUENTIAL) });

        // ---- ここは実測で決めている。動かす前に測り直すこと ----
        //
        // 計測 (Ryzen 9 9950X3D / ORT 1.31.0 / 48 kHz ステレオ / 1 hop = 480
        // サンプル = 10 ms、候補を交互に測った中央値):
        //
        //   1 ホップの内訳: ONNX 推論 ~99 %、それ以外 (STFT + ブレンド +
        //   メーター + FIFO) は約 10 µs = 0.1〜0.5 %。STFT 単体は 4 µs 程度。
        //   => 推論以外を削っても意味は無い。効くのは推論そのものだけ。
        //
        //   dp8 (2out, ステレオ): 既定 5.3 ms (53 %) / dp2: 2.2 ms (22 %)。
        //   `SetIntraOpNumThreads` は 1 が最良 (既定 = 1): 2 で +5〜7 %、4 で
        //   +12〜15 % (CPU は 3〜7 倍)、0 (自動 = 32 threads) は dp8 で
        //   22 ms = 予算の 216 % と論外。この小さなモデルでは ORT の
        //   スレッド間同期のほうが計算より高い。追加スレッドは CPU を
        //   食うだけで遅くなるので **1 のまま**。
        //
        //   実行モード `ORT_PARALLEL` / `DisableCpuMemArena` / `DisableMemPattern`
        //   / グラフ最適化 `EXTENDED`・`BASIC` は、いずれも中央値が
        //   ±5 % 以内か悪化 (最大 +15 %)。**既定 (SEQUENTIAL / arena on /
        //   mem pattern on / ORT_ENABLE_ALL) が最良**なので変えない。
        //
        //   2 出力 (`_2out`) と 1 出力 (`_hr`) の推論時間差は測定ノイズ未満
        //   (dp8 ステレオ 5.63 vs 5.60 ms)。残響ヘッドは無視できるほど軽く、
        //   1 出力へ切り替えても CPU は下がらない (機能が減るだけ)。
        //
        //   ステレオはチャンネルごとにセッション + Run を分けたまま (5.3 ms)。
        //   モデルの batch 次元が固定 1 なので batch=2 の 1 Run は作れない。
        //   仮に再エクスポートしても、このモデルは層ごとの計算なので
        //   期待できる短縮は 1 Run あたりの固定費 (数十 µs) 程度。
        //
        // 補足: `measure_hop_us` と `bench_provider.py` で誰でも同じ数字が出る。

        if device == DeviceChoice::Gpu {
            // DirectML は **mem pattern 最適化と並列実行に対応しない**（公式
            // ドキュメント）。特に mem pattern を有効にしたままだと、内部の
            // 作業バッファが正しく確保されず**出力が壊れる**（エラーにならない
            // ので気付きにくい）。並列実行は上で `ORT_SEQUENTIAL` にしてある。
            opt!(unsafe { (api.DisableMemPattern)(options) });

            // DML EP を先頭に挿す（対応しないノードだけ CPU に落ちる）。
            //
            // DML の関数は `OrtApi` ではなく EP 専用の `OrtDmlApi` にあり、
            // `GetExecutionProviderApi("DML", ...)` で取る。DML 非対応ビルドでは
            // ここで失敗するので、CPU 版の DLL を掴んでいても安全に検出できる。
            //
            // ここで失敗したら options を解放して呼び出し側に返す
            // （呼び出し側が CPU で組み直す）。
            let mut dml: *const c_void = ptr::null();
            if let Err(e) = ort::check(api, unsafe {
                (api.GetExecutionProviderApi)(
                    c"DML".as_ptr(),
                    ort_sys::ORT_API_VERSION,
                    &mut dml,
                )
            }) {
                unsafe { (api.ReleaseSessionOptions)(options) };
                return Err(format!("この onnxruntime は DirectML EP を持たない: {e}"));
            }
            if dml.is_null() {
                unsafe { (api.ReleaseSessionOptions)(options) };
                return Err("この onnxruntime は DirectML EP を持たない (API が空)".to_string());
            }

            // SAFETY: dml は ORT が返した OrtDmlApi で、ロードした DLL 内で有効。
            let dml_api = dml as *const ort_sys::OrtDmlApi;
            let st = unsafe { ((*dml_api).SessionOptionsAppendExecutionProvider_DML)(options, 0) };
            if let Err(e) = ort::check(api, st) {
                unsafe { (api.ReleaseSessionOptions)(options) };
                return Err(format!("DirectML EP を追加できません: {e}"));
            }
        }

        let wide = to_wide_path(model_path);
        let mut session: *mut OrtSession = ptr::null_mut();
        let st = unsafe { (api.CreateSession)(env, wide.as_ptr(), options, &mut session) };

        // options は session 作成後は不要。
        unsafe { (api.ReleaseSessionOptions)(options) };

        if let Err(e) = ort::check(api, st) {
            return Err(format!("CreateSession({}): {e}", model_path.display()));
        }

        self.session = session;
        Ok(())
    }

    fn setup_bindings(&mut self) -> Result<(), String> {
        let api = self.api;

        let mut alloc: *mut OrtAllocator = ptr::null_mut();
        ort::check(api, unsafe { (api.GetAllocatorWithDefaultOptions)(&mut alloc) })?;

        // ---- metadata ----
        let mut md: *mut OrtModelMetadata = ptr::null_mut();
        ort::check(api, unsafe { (api.SessionGetModelMetadata)(self.session, &mut md) })?;
        if md.is_null() {
            return Err("SessionGetModelMetadata が NULL を返した".to_string());
        }

        let meta = {
            let get = |key: &str| unsafe { meta_get(api, md, alloc, key) };
            ModelMeta::from_lookup(&get)
        };
        unsafe { (api.ReleaseModelMetadata)(md) };

        if !meta.valid {
            return Err(
                "ONNX metadata が DPDFNet のストリーミング書き出しではない \
                 (model_type/n_fft/hop_length/window_length/freq_bins/state_size/\
                 erb_norm_init/spec_norm_init が必要)"
                    .to_string(),
            );
        }

        self.meta = meta;
        self.freq_bins = self.meta.freq_bins.max(0) as usize;
        self.state_size = self.meta.state_size.max(0) as usize;

        // ---- 入出力名 ----
        let input_count = self.io_count(true)?;
        let output_count = self.io_count(false)?;

        let mut spec_name: Option<CString> = None;
        let mut state_in_name: Option<CString> = None;
        for i in 0..input_count {
            let name = unsafe { self.io_name(alloc, i, true) }?;
            if let Some(n) = name {
                if n.as_c_str() == c"spec" {
                    spec_name = Some(n);
                } else if n.as_c_str() == c"state_in" {
                    state_in_name = Some(n);
                }
            }
        }

        let mut full_name: Option<CString> = None;
        let mut state_out_name: Option<CString> = None;
        let mut ns_name: Option<CString> = None;
        for i in 0..output_count {
            let name = unsafe { self.io_name(alloc, i, false) }?;
            if let Some(n) = name {
                if n.as_c_str() == c"spec_e" {
                    full_name = Some(n);
                } else if n.as_c_str() == c"state_out" {
                    state_out_name = Some(n);
                } else if n.as_c_str() == c"spec_ns" || n.as_c_str() == c"spec_m" {
                    ns_name = Some(n);
                }
            }
        }

        let (Some(spec_name), Some(state_in_name), Some(full_name), Some(state_out_name)) =
            (spec_name, state_in_name, full_name, state_out_name)
        else {
            return Err(
                "ONNX の入出力名が想定外 (inputs: spec, state_in / outputs: spec_e, state_out)"
                    .to_string(),
            );
        };
        self.has_ns_output = ns_name.is_some();

        // ---- shape 検証 ----
        let spec_shape = unsafe { self.input_shape(0, spec_name.as_c_str()) }?;
        if spec_shape.len() != SPEC_RANK {
            return Err("spec 入力は rank 4 [1,1,F,2] である必要がある".to_string());
        }
        if spec_shape[2] > 0 && spec_shape[2] as usize != self.freq_bins {
            return Err(format!(
                "freq_bins 不一致: metadata={} graph={}",
                self.freq_bins, spec_shape[2]
            ));
        }

        // ---- 事前確保 ----
        let spec_count = 2 * self.freq_bins;

        self.spec_buf = vec![0.0f32; spec_count];
        self.out_full_buf = vec![0.0f32; spec_count];
        self.out_ns_buf = vec![0.0f32; spec_count];

        let init_state = self.meta.make_initial_state();
        self.state_buf_a = init_state.clone();
        self.state_buf_b = init_state;

        let spec_shape_i64: [i64; SPEC_COUNT_RANK] = [1, 1, self.freq_bins as i64, 2];
        let state_shape_i64: [i64; 1] = [self.state_size as i64];

        let mut mem: *mut OrtMemoryInfo = ptr::null_mut();
        ort::check(
            api,
            unsafe {
                (api.CreateCpuMemoryInfo)(
                    OrtAllocatorType::OrtArenaAllocator,
                    OrtMemType::OrtMemTypeDefault,
                    &mut mem,
                )
            },
        )?;
        self.mem = mem;

        if self.device == DeviceChoice::Gpu {
            // 出力は CPU 側のバッファへ戻す。`OrtMemTypeCPUOutput` を付けないと
            // ORT は「出力はデバイス側のメモリ」と解釈し、こちらのバッファを
            // 書かない（症状: 出力が無音）。
            let mut m: *mut OrtMemoryInfo = ptr::null_mut();
            ort::check(
                api,
                unsafe {
                    (api.CreateCpuMemoryInfo)(
                        OrtAllocatorType::OrtArenaAllocator,
                        OrtMemType::OrtMemTypeCPUOutput,
                        &mut m,
                    )
                },
            )?;
            self.mem_out = m;

            // 束ね（OrtValue / IoBinding）は `run_gpu_inner` が毎 hop 作り直す。
            // 使い回すと DirectML が**最初に上げた入力をそのまま使い続け**、
            // 以降の hop が古いデータで計算されてしまう（実測で確認。CPU EP では
            // 起きないので、既定の CPU 経路は今までどおり使い回す）。
            self.names = Some(IoNames {
                spec: spec_name,
                state_in: state_in_name,
                full: full_name,
                state_out: state_out_name,
                ns: ns_name,
            });
            self.loaded = true;
            self.state_in_a = true;
            return Ok(());
        }

        // ---- CPU: 使い回しの束ね ----
        self.spec_value = unsafe {
            create_tensor(
                api,
                mem,
                &mut self.spec_buf,
                spec_count,
                &spec_shape_i64,
            )?
        };
        self.out_full_value = unsafe {
            create_tensor(
                api,
                mem,
                &mut self.out_full_buf,
                spec_count,
                &spec_shape_i64,
            )?
        };
        self.out_ns_value = unsafe {
            create_tensor(api, mem, &mut self.out_ns_buf, spec_count, &spec_shape_i64)?
        };
        self.state_value_a = unsafe {
            create_tensor(
                api,
                mem,
                &mut self.state_buf_a,
                self.state_size,
                &state_shape_i64,
            )?
        };
        self.state_value_b = unsafe {
            create_tensor(
                api,
                mem,
                &mut self.state_buf_b,
                self.state_size,
                &state_shape_i64,
            )?
        };

        // ---- IoBinding (状態の ping-pong) ----
        let ns_c = ns_name.as_ref().map(|s| s.as_c_str());
        self.binding_a = unsafe {
            make_binding(
                api,
                self.session,
                spec_name.as_c_str(),
                state_in_name.as_c_str(),
                full_name.as_c_str(),
                state_out_name.as_c_str(),
                ns_c,
                self.spec_value,
                self.state_value_a,
                self.out_full_value,
                self.state_value_b,
                self.out_ns_value,
            )?
        };
        self.binding_b = unsafe {
            make_binding(
                api,
                self.session,
                spec_name.as_c_str(),
                state_in_name.as_c_str(),
                full_name.as_c_str(),
                state_out_name.as_c_str(),
                ns_c,
                self.spec_value,
                self.state_value_b,
                self.out_full_value,
                self.state_value_a,
                self.out_ns_value,
            )?
        };

        self.loaded = true;
        self.state_in_a = true;
        Ok(())
    }

    fn io_count(&self, input: bool) -> Result<usize, String> {
        let api = self.api;
        let mut n: usize = 0;
        let st = unsafe {
            if input {
                (api.SessionGetInputCount)(self.session, &mut n)
            } else {
                (api.SessionGetOutputCount)(self.session, &mut n)
            }
        };
        ort::check(api, st)?;
        Ok(n)
    }

    /// `SessionGetInputName` / `SessionGetOutputName`。戻り値は必要なら `AllocatorFree` 済み。
    unsafe fn io_name(
        &self,
        alloc: *mut OrtAllocator,
        index: usize,
        input: bool,
    ) -> Result<Option<CString>, String> {
        let api = self.api;
        let mut ptr_out: *mut c_char = ptr::null_mut();
        let st = unsafe {
            if input {
                (api.SessionGetInputName)(self.session, index, alloc, &mut ptr_out)
            } else {
                (api.SessionGetOutputName)(self.session, index, alloc, &mut ptr_out)
            }
        };
        ort::check(api, st)?;

        if ptr_out.is_null() {
            return Ok(None);
        }

        let s = unsafe { CStr::from_ptr(ptr_out) }.to_owned();
        let st = unsafe { (api.AllocatorFree)(alloc, ptr_out as *mut c_void) };
        let _ = ort::check(api, st);

        Ok(Some(s))
    }

    /// `spec` 入力の shape。rank が 1 以上であることだけ確認して返す。
    unsafe fn input_shape(
        &self,
        index: usize,
        _name: &CStr,
    ) -> Result<Vec<i64>, String> {
        let api = self.api;
        let mut info: *mut ort_sys::OrtTypeInfo = ptr::null_mut();
        ort::check(
            api,
            unsafe { (api.SessionGetInputTypeInfo)(self.session, index, &mut info) },
        )?;

        let mut tensor_info: *const ort_sys::OrtTensorTypeAndShapeInfo = ptr::null();
        let st = unsafe { (api.CastTypeInfoToTensorInfo)(info, &mut tensor_info) };
        if let Err(e) = ort::check(api, st) {
            unsafe { (api.ReleaseTypeInfo)(info) };
            return Err(e);
        }

        let mut rank: usize = 0;
        let st = unsafe { (api.GetDimensionsCount)(tensor_info, &mut rank) };
        if let Err(e) = ort::check(api, st) {
            unsafe { (api.ReleaseTypeInfo)(info) };
            return Err(e);
        }

        let mut dims = vec![0i64; rank];
        let st = unsafe { (api.GetDimensions)(tensor_info, dims.as_mut_ptr(), rank) };
        if let Err(e) = ort::check(api, st) {
            unsafe { (api.ReleaseTypeInfo)(info) };
            return Err(e);
        }

        // ここから下の `tensor_info` は `info` が所有する借用ポインタ。
        // **解放してはいけない**（`CastTypeInfoToTensorInfo` は
        // `UnownedTensorTypeAndShapeInfo` 相当を返す）。
        unsafe {
            (api.ReleaseTypeInfo)(info);
        }

        Ok(dims)
    }

    fn run_warmup(&mut self) {
        // ORT 内部アリーナをここで成長させる（オーディオスレッドでの確保を避ける）。
        self.spec_buf.iter_mut().for_each(|v| *v = 0.0);
        for _ in 0..4 {
            let _ = self.run_internal();
        }
        self.reset_state();
    }

    pub fn reset_state(&mut self) {
        if !self.loaded {
            return;
        }

        let init = self.meta.make_initial_state();
        if init.len() == self.state_size {
            self.state_buf_a.copy_from_slice(&init);
            self.state_buf_b.copy_from_slice(&init);
        }
        self.state_in_a = true;
    }

    /// 1 フレーム推論。`spec_in` は長さ `2 * freq_bins` 以上、`out_full` / `out_ns` も同様。
    /// 失敗しても音は止めない（入力をそのまま返す）。
    pub fn process_frame(&mut self, spec_in: &[f32], out_full: &mut [f32], out_ns: &mut [f32]) {
        let n = 2 * self.freq_bins;
        if !self.loaded || n == 0 || spec_in.len() < n || out_full.len() < n || out_ns.len() < n {
            if out_full.len() >= n {
                out_full[..n].iter_mut().for_each(|v| *v = 0.0);
            }
            if out_ns.len() >= n {
                out_ns[..n].iter_mut().for_each(|v| *v = 0.0);
            }
            return;
        }

        self.spec_buf[..n].copy_from_slice(&spec_in[..n]);

        if self.run_internal() {
            out_full[..n].copy_from_slice(&self.out_full_buf[..n]);
            out_ns[..n].copy_from_slice(&self.out_ns_buf[..n]);
        } else {
            out_full[..n].copy_from_slice(&self.spec_buf[..n]);
            out_ns[..n].copy_from_slice(&self.spec_buf[..n]);
        }
    }

    /// 確保も移送もしない本体。成功で `true`。
    fn run_internal(&mut self) -> bool {
        if !self.loaded {
            return false;
        }

        if self.device == DeviceChoice::Gpu {
            return match self.run_gpu_inner() {
                Ok(()) => true,
                Err(e) => {
                    if self.last_error != e {
                        self.last_error = e;
                    }
                    false
                }
            };
        }

        // ---- CPU EP（束ねを使い回す。実機でビット一致を確認した経路）----
        let binding = if self.state_in_a {
            self.binding_a
        } else {
            self.binding_b
        };

        let st = unsafe {
            (self.api.RunWithBinding)(
                self.session,
                ptr::null::<OrtRunOptions>(),
                binding,
            )
        };

        if !st.0.is_null() {
            // エラー本文を控えてから status を解放する。
            let msg = unsafe {
                let p = (self.api.GetErrorMessage)(st.0);
                if p.is_null() {
                    "RunWithBinding failed".to_string()
                } else {
                    CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            };
            unsafe { (self.api.ReleaseStatus)(st.0) };
            if self.last_error != msg {
                self.last_error = msg;
            }
            return false;
        }

        let n = 2 * self.freq_bins;
        if !self.has_ns_output {
            // 1 出力モデルでは 2 出力目を full と同じにする（Voice Reverb を無効化）。
            self.out_ns_buf[..n].copy_from_slice(&self.out_full_buf[..n]);
        }

        // 新しい状態は binding の出力側バッファに入っている。
        self.state_in_a = !self.state_in_a;
        true
    }

    /// GPU (DirectML) の 1 hop。**束ねを毎 hop 作り直す。**
    ///
    /// `OrtValue` と `OrtIoBinding` を使い回すと、DirectML は**最初に上げた
    /// 入力をそのまま使い続ける**（ホスト側のバッファを書き換えても再転送
    /// されない）。症状は「モデルの出力が無音になり、確認できるのは乾いた音
    /// の漏れだけ」。毎 hop 作り直すと CPU EP と 96 dB で一致する（計測済み）。
    ///
    /// CPU EP では起きない現象なので、既定の CPU 経路は使い回しのままにして
    /// ある（1 hop ごとの確保を持ち込まないため）。
    fn run_gpu_inner(&mut self) -> Result<(), String> {
        let api = self.api;
        let Some(names) = self.names.as_ref() else {
            return Err("GPU の入出力名が無い（初期化の順序がおかしい）".to_string());
        };

        let spec_count = 2 * self.freq_bins;
        let spec_shape: [i64; SPEC_COUNT_RANK] = [1, 1, self.freq_bins as i64, 2];
        let state_shape: [i64; 1] = [self.state_size as i64];

        // 状態は ping-pong（今回の出力が次の入力になる）。
        let (state_in_buf, state_out_buf) = if self.state_in_a {
            (&mut self.state_buf_a, &mut self.state_buf_b)
        } else {
            (&mut self.state_buf_b, &mut self.state_buf_a)
        };

        let mut binding: *mut OrtIoBinding = ptr::null_mut();
        ort::check(api, unsafe { (api.CreateIoBinding)(self.session, &mut binding) })?;

        // 途中で失敗しても作った分は必ず解放する。
        let mut guard = ValueGuard {
            api,
            binding,
            values: Vec::with_capacity(5),
        };

        let spec_value = unsafe {
            create_tensor(api, self.mem, &mut self.spec_buf, spec_count, &spec_shape)?
        };
        guard.values.push(spec_value);
        let state_in_value = unsafe {
            create_tensor(api, self.mem, state_in_buf, self.state_size, &state_shape)?
        };
        guard.values.push(state_in_value);
        let full_value = unsafe {
            create_tensor(api, self.mem_out, &mut self.out_full_buf, spec_count, &spec_shape)?
        };
        guard.values.push(full_value);
        let state_out_value = unsafe {
            create_tensor(api, self.mem_out, state_out_buf, self.state_size, &state_shape)?
        };
        guard.values.push(state_out_value);

        unsafe {
            ort::check(api, (api.BindInput)(binding, names.spec.as_ptr(), spec_value))?;
            ort::check(
                api,
                (api.BindInput)(binding, names.state_in.as_ptr(), state_in_value),
            )?;
            ort::check(
                api,
                (api.BindOutput)(binding, names.full.as_ptr(), full_value),
            )?;
            ort::check(
                api,
                (api.BindOutput)(binding, names.state_out.as_ptr(), state_out_value),
            )?;
        }

        if let Some(ns) = names.ns.as_ref() {
            let ns_value = unsafe {
                create_tensor(api, self.mem_out, &mut self.out_ns_buf, spec_count, &spec_shape)?
            };
            guard.values.push(ns_value);
            ort::check(api, unsafe {
                (api.BindOutput)(binding, ns.as_ptr(), ns_value)
            })?;
        }

        ort::check(api, unsafe {
            (api.RunWithBinding)(self.session, ptr::null::<OrtRunOptions>(), binding)
        })?;
        // 出力を CPU のバッファへ移送する（DML の出力はデバイス側にある）。
        ort::check(api, unsafe { (api.SynchronizeBoundOutputs)(binding) })?;

        if !self.has_ns_output {
            // 1 出力モデルでは 2 出力目を full と同じにする（Voice Reverb を無効化）。
            self.out_ns_buf[..spec_count].copy_from_slice(&self.out_full_buf[..spec_count]);
        }

        // 新しい状態は出力側のバッファに入っている。
        self.state_in_a = !self.state_in_a;
        Ok(())
    }
}

/// GPU のときだけ要る入出力名。
struct IoNames {
    spec: CString,
    state_in: CString,
    full: CString,
    state_out: CString,
    ns: Option<CString>,
}

/// 毎 hop 作る `OrtValue` と `OrtIoBinding` を確実に解放する。
///
/// `?` で抜ける経路があるので、解放漏れを防ぐために guard で持つ。
struct ValueGuard {
    api: &'static OrtApi,
    binding: *mut OrtIoBinding,
    values: Vec<*mut OrtValue>,
}

impl Drop for ValueGuard {
    fn drop(&mut self) {
        unsafe {
            for v in self.values.drain(..) {
                if !v.is_null() {
                    (self.api.ReleaseValue)(v);
                }
            }
            if !self.binding.is_null() {
                (self.api.ReleaseIoBinding)(self.binding);
            }
        }
    }
}

impl Drop for DpdfNet {
    fn drop(&mut self) {
        let api = self.api;

        unsafe {
            for b in [self.binding_a, self.binding_b] {
                if !b.is_null() {
                    (api.ReleaseIoBinding)(b);
                }
            }
            for v in [
                self.spec_value,
                self.out_full_value,
                self.out_ns_value,
                self.state_value_a,
                self.state_value_b,
            ] {
                if !v.is_null() {
                    (api.ReleaseValue)(v);
                }
            }
            if !self.mem.is_null() {
                (api.ReleaseMemoryInfo)(self.mem);
            }
            if !self.mem_out.is_null() {
                (api.ReleaseMemoryInfo)(self.mem_out);
            }
            if !self.session.is_null() {
                (api.ReleaseSession)(self.session);
            }
        }
    }
}

// SAFETY: DpdfNet は 1 つのスレッド（オーディオスレッド）からのみ触る。
// ただしフィルタの構造体に埋め込んで `*mut c_void` として OBS に渡すため、
// Send/Sync を明示しておく（実際の排他はフィルタ側が保証する）。
unsafe impl Send for DpdfNet {}
unsafe impl Sync for DpdfNet {}

// --------------------------------------------------------------------------
// ORT ヘルパ
// --------------------------------------------------------------------------

unsafe fn meta_get(
    api: &OrtApi,
    md: *const OrtModelMetadata,
    alloc: *mut OrtAllocator,
    key: &str,
) -> Option<String> {
    let ckey = CString::new(key).ok()?;
    let mut val: *mut c_char = ptr::null_mut();

    let st = unsafe { (api.ModelMetadataLookupCustomMetadataMap)(md, alloc, ckey.as_ptr(), &mut val) };

    if !st.0.is_null() {
        // キーが無い場合にエラーを返す ORT もあるので、どちらでも「無し」に倒す。
        unsafe { (api.ReleaseStatus)(st.0) };
        return None;
    }

    if val.is_null() {
        return None;
    }

    let s = unsafe { CStr::from_ptr(val) }.to_string_lossy().into_owned();
    let st = unsafe { (api.AllocatorFree)(alloc, val as *mut c_void) };
    if !st.0.is_null() {
        unsafe { (api.ReleaseStatus)(st.0) };
    }

    Some(s)
}

unsafe fn create_tensor(
    api: &OrtApi,
    mem: *mut OrtMemoryInfo,
    buf: &mut [f32],
    count: usize,
    shape: &[i64],
) -> Result<*mut OrtValue, String> {
    let mut v: *mut OrtValue = ptr::null_mut();
    let st = unsafe {
        (api.CreateTensorWithDataAsOrtValue)(
            mem,
            buf.as_mut_ptr() as *mut c_void,
            count * std::mem::size_of::<f32>(),
            shape.as_ptr(),
            shape.len(),
            ONNXTensorElementDataType::ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
            &mut v,
        )
    };
    ort::check(api, st)?;
    if v.is_null() {
        return Err("CreateTensorWithDataAsOrtValue が NULL を返した".to_string());
    }
    Ok(v)
}

#[allow(clippy::too_many_arguments)]
unsafe fn make_binding(
    api: &OrtApi,
    session: *mut OrtSession,
    spec_name: &CStr,
    state_in_name: &CStr,
    full_name: &CStr,
    state_out_name: &CStr,
    ns_name: Option<&CStr>,
    spec_value: *mut OrtValue,
    state_in_value: *mut OrtValue,
    full_value: *mut OrtValue,
    state_out_value: *mut OrtValue,
    ns_value: *mut OrtValue,
) -> Result<*mut OrtIoBinding, String> {
    let mut b: *mut OrtIoBinding = ptr::null_mut();
    ort::check(api, unsafe { (api.CreateIoBinding)(session, &mut b) })?;

    ort::check(
        api,
        unsafe { (api.BindInput)(b, spec_name.as_ptr(), spec_value) },
    )?;
    ort::check(
        api,
        unsafe { (api.BindInput)(b, state_in_name.as_ptr(), state_in_value) },
    )?;
    ort::check(
        api,
        unsafe { (api.BindOutput)(b, full_name.as_ptr(), full_value) },
    )?;
    ort::check(
        api,
        unsafe { (api.BindOutput)(b, state_out_name.as_ptr(), state_out_value) },
    )?;
    if let Some(ns) = ns_name {
        ort::check(api, unsafe { (api.BindOutput)(b, ns.as_ptr(), ns_value) })?;
    }

    Ok(b)
}

fn to_wide_path(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}
