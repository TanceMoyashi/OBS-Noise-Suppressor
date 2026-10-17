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

const SPEC_RANK: usize = 4;
const SPEC_COUNT_RANK: usize = 4;

pub struct DpdfNet {
    api: &'static OrtApi,
    session: *mut OrtSession,
    mem: *mut OrtMemoryInfo,

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
    pub fn load(model_path: &Path, n_threads: i32, ort_dirs: &[PathBuf]) -> Result<Self, String> {
        let api = ort::ensure_loaded(ort_dirs)?;
        let env_addr = ort::global_env(api)?;

        let mut me = Self {
            api,
            session: ptr::null_mut(),
            mem: ptr::null_mut(),
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
            last_error: String::new(),
            meta: ModelMeta::default(),
            freq_bins: 0,
            state_size: 0,
        };

        // ここから先で失敗しても Drop が後始末する。
        me.create_session(env_addr as *const OrtEnv, model_path, n_threads)?;
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

    fn create_session(&mut self, env: *const OrtEnv, model_path: &Path, n_threads: i32) -> Result<(), String> {
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
            create_tensor(
                api,
                mem,
                &mut self.out_ns_buf,
                spec_count,
                &spec_shape_i64,
            )?
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
