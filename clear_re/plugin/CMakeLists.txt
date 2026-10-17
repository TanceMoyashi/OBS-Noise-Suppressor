cmake_minimum_required(VERSION 3.22)

project(DPDFNetVoice VERSION 0.1.0 LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# ---------------------------------------------------------------------------
# JUCE 8 (FetchContent)
#   8.0.12 は参照実装 (Alt Denoiser / voice-denoiser) と同じ系列。
# ---------------------------------------------------------------------------
include(FetchContent)

FetchContent_Declare(JUCE
    GIT_REPOSITORY https://github.com/juce-framework/JUCE.git
    GIT_TAG        8.0.12
    GIT_SHALLOW    TRUE)
FetchContent_MakeAvailable(JUCE)

# ---------------------------------------------------------------------------
# KissFFT (third_party/ に同梱。BSD-3-Clause)
#   n_fft = 960 = 2^6 * 3 * 5 は 2 の冪ではないので juce::dsp::FFT は使えない。
#   kiss_fftr は任意の偶数サイズ (= 混合基数) に対応する。
#   同梱理由: kissfft 本家の CMake は多数のバリアントをビルドするため、
#             必要な 2 ソースだけを直接コンパイルする方が確実。
# ---------------------------------------------------------------------------
add_library(kissfft_static STATIC
    third_party/kissfft/kiss_fft.c
    third_party/kissfft/kiss_fftr.c)

target_include_directories(kissfft_static PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/third_party/kissfft)

# 注意: kissfft は FIXED_POINT / USE_SIMD / KISS_FFT_USE_ALLOCA の判定に #ifdef を使うため、
# これらを 0 で定義してはいけない (定義すると有効化されてしまう)。未定義のままにする。

if (MSVC)
    target_compile_options(kissfft_static PRIVATE /W0)
endif()

# ---------------------------------------------------------------------------
# ONNX Runtime
#   優先 1: -DONNXRUNTIME_ROOT=<prebuilt の展開先 (include\ と lib\ がある場所)>
#   優先 2: vcpkg の onnxruntime パッケージ
# ---------------------------------------------------------------------------
set(ORT_COPY_DLL FALSE)

if (DEFINED ONNXRUNTIME_ROOT)
    find_path(ORT_INCLUDE_DIR onnxruntime_cxx_api.h
        HINTS ${ONNXRUNTIME_ROOT}
        PATH_SUFFIXES include include/onnxruntime
        NO_DEFAULT_PATH REQUIRED)

    find_library(ORT_LIBRARY onnxruntime
        HINTS ${ONNXRUNTIME_ROOT}
        PATH_SUFFIXES lib
        NO_DEFAULT_PATH REQUIRED)

    add_library(ort_imported UNKNOWN IMPORTED)
    set_target_properties(ort_imported PROPERTIES
        IMPORTED_LOCATION "${ORT_LIBRARY}"
        INTERFACE_INCLUDE_DIRECTORIES "${ORT_INCLUDE_DIR}")

    set(ORT_LINK ort_imported)
    set(ORT_COPY_DLL TRUE)
    message(STATUS "ONNX Runtime (prebuilt): ${ORT_LIBRARY}")
else()
    find_package(onnxruntime CONFIG QUIET)

    if (TARGET onnxruntime::onnxruntime)
        set(ORT_LINK onnxruntime::onnxruntime)
    elseif (TARGET onnxruntime)
        set(ORT_LINK onnxruntime)
    else()
        message(FATAL_ERROR
            "ONNX Runtime not found. Pass -DONNXRUNTIME_ROOT=<prebuilt dir> or install it "
            "with vcpkg (vcpkg install onnxruntime:x64-windows). See README section 2.")
    endif()

    message(STATUS "ONNX Runtime: found via find_package")
endif()

# 同梱する onnxruntime.dll の実体 (POST_BUILD で bundle へコピーする)
if (ORT_COPY_DLL)
    get_filename_component(_ort_dir "${ORT_LIBRARY}" DIRECTORY)
endif()

# ---------------------------------------------------------------------------
# プラグイン本体
#   チャンネル構成は CMake 引数ではなく BusesProperties / isBusesLayoutSupported で
#   指定する (JUCE の CMake API には PLUGIN_CHANNEL_CONFIGURATIONS は存在しない)。
# ---------------------------------------------------------------------------
juce_add_plugin(DPDFNetVoice
    COMPANY_NAME               "self"
    PLUGIN_MANUFACTURER_CODE    Synt
    PLUGIN_CODE                 Dpfn
    PRODUCT_NAME               "DPDFNet Voice"
    DESCRIPTION                "Neural noise suppression and de-reverberation (DPDFNet, 48 kHz)"
    FORMATS                    VST3
    IS_SYNTH                   FALSE
    NEEDS_MIDI_INPUT           FALSE
    NEEDS_MIDI_OUTPUT          FALSE
    IS_MIDI_EFFECT             FALSE
    EDITOR_WANTS_KEYBOARD_FOCUS FALSE
    COPY_PLUGIN_AFTER_BUILD    FALSE
    VST3_CATEGORIES            Restoration Fx)

juce_generate_juce_header(DPDFNetVoice)

target_sources(DPDFNetVoice PRIVATE
    src/OrtRuntime.cpp
    src/ModelMeta.cpp
    src/VorbisStft.cpp
    src/DpdfNet.cpp
    src/PluginProcessor.cpp
    src/PluginEditor.cpp)

target_include_directories(DPDFNetVoice PRIVATE src)

if (MSVC)
    # ソースは UTF-8 (日本語コメント入り)。/permissive- は JUCE が推奨する厳格モード。
    target_compile_options(DPDFNetVoice PRIVATE /utf-8 /permissive- /Zc:__cplusplus /MP)
endif()

target_compile_definitions(DPDFNetVoice PUBLIC
    # onnxruntime を import table の依存にしない。定義しないと、VST3 のモジュール
    # ローダ (素の LoadLibraryW) が探索順で C:\Windows\System32\onnxruntime.dll
    # (Windows ML 同梱の ONNX Runtime 1.17) を先に読み込み、API バージョン不一致で
    # ホストプロセスごと即死する。src/OrtRuntime.cpp が同梱 DLL を明示的に読み込む。
    ORT_API_MANUAL_INIT=1
    JUCE_WEB_BROWSER=0
    JUCE_USE_CURL=0
    JUCE_VST3_CAN_REPLACE_VST2=0
    JUCE_DISPLAY_SPLASH_SCREEN=0
    JUCE_STRICT_REFCOUNTEDPOINTER=1
    $<$<CONFIG:Debug>:JUCE_DEBUG=1>)

target_link_libraries(DPDFNetVoice PRIVATE
    ${ORT_LINK}
    kissfft_static
    juce::juce_audio_utils
    juce::juce_dsp
    PUBLIC
    juce::juce_recommended_config_flags
    juce::juce_recommended_lto_flags
    juce::juce_recommended_warning_flags)

# ---------------------------------------------------------------------------
# ビルド後処理
#   DLL / モデルは「実際のプラグイン DLL の隣」に置く必要がある。
#   TARGET_FILE_DIR:DPDFNetVoice_VST3 = <bundle>/Contents/x86_64-win/
#   (共有コード側の TARGET_FILE_DIR では 1 階層上になり、ホストが解決できない)
# ---------------------------------------------------------------------------
if (WIN32 AND ORT_COPY_DLL)
    # 一意な名前で同梱する。onnxruntime.dll という名前だと、Windows 11 の
    # C:\Windows\System32\onnxruntime.dll (Windows ML, ORT 1.17) と名前が衝突し、
    # ローダの探索順 (プラグインのディレクトリは入らない) でそちらが優先される。
    # 名前を変えれば衝突が起こり得ない。読み込みは src/OrtRuntime.cpp が
    # 絶対パス + LOAD_WITH_ALTERED_SEARCH_PATH で行う。
    add_custom_command(TARGET DPDFNetVoice_VST3 POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
                "${_ort_dir}/onnxruntime.dll"
                "$<TARGET_FILE_DIR:DPDFNetVoice_VST3>/dpdfnet_onnxruntime.dll"
        # 旧ビルドが置いた onnxruntime.dll は使われない (import table に無い)。
        # 名前衝突の再発防止と混乱回避のため消しておく。
        COMMAND ${CMAKE_COMMAND} -E rm -f
                "$<TARGET_FILE_DIR:DPDFNetVoice_VST3>/onnxruntime.dll"
        COMMENT "Copying ONNX Runtime into the VST3 bundle as dpdfnet_onnxruntime.dll")
endif()

# models/ は PluginProcessor の探索順と一致する <artefacts>/VST3/models にコピーする
# (pluginDir から 3 階層上)。ホストが読むのは ONNX だけなので *.onnx のみを運ぶ
# (テスト用 wav を配布物に混ぜない)。
file(GLOB _model_onnx CONFIGURE_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/models/*.onnx")
if (_model_onnx)
    add_custom_command(TARGET DPDFNetVoice_VST3 POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E make_directory
                "$<TARGET_FILE_DIR:DPDFNetVoice_VST3>/../../../models"
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
                ${_model_onnx}
                "$<TARGET_FILE_DIR:DPDFNetVoice_VST3>/../../../models"
        COMMENT "Copying models/*.onnx next to the VST3 directory")
endif()

# ---------------------------------------------------------------------------
# 検証用 EXE: ONNX Runtime のロードと DPDFNet 推論 (JUCE / GUI 非依存)
#   OBS を立ち上げずに「同梱の dpdfnet_onnxruntime.dll が使われているか」と
#   DpdfNet::load / processFrame / VorbisStft が実際に動くかを確認する。
#   実行: <build>/dpdfnet_ort_smoke[.exe] [model.onnx]
# ---------------------------------------------------------------------------
add_executable(dpdfnet_ort_smoke
    tools/ort_smoke_test.cpp
    src/OrtRuntime.cpp
    src/ModelMeta.cpp
    src/VorbisStft.cpp
    src/DpdfNet.cpp)

target_include_directories(dpdfnet_ort_smoke PRIVATE src)

target_compile_definitions(dpdfnet_ort_smoke PRIVATE ORT_API_MANUAL_INIT=1)

if (MSVC)
    target_compile_options(dpdfnet_ort_smoke PRIVATE /utf-8 /Zc:__cplusplus)
endif()

target_link_libraries(dpdfnet_ort_smoke PRIVATE
    ${ORT_LINK}
    kissfft_static)

if (WIN32 AND ORT_COPY_DLL)
    add_custom_command(TARGET dpdfnet_ort_smoke POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
                "${_ort_dir}/onnxruntime.dll"
                "$<TARGET_FILE_DIR:dpdfnet_ort_smoke>/dpdfnet_onnxruntime.dll"
        COMMENT "Copying ONNX Runtime next to the smoke test exe")
endif()

# ---------------------------------------------------------------------------
# 開発用: エディタをヘッドレスで描画して PNG にする (見た目の確認。配布しない)
#   .\build\Release\dpdfnet_ui_snapshot.exe ui.png
# ---------------------------------------------------------------------------
juce_add_console_app(dpdfnet_ui_snapshot PRODUCT_NAME "DPDFNet UI snapshot")

target_sources(dpdfnet_ui_snapshot PRIVATE
    tools/ui_snapshot.cpp
    src/OrtRuntime.cpp
    src/ModelMeta.cpp
    src/VorbisStft.cpp
    src/DpdfNet.cpp
    src/PluginProcessor.cpp
    src/PluginEditor.cpp)

target_include_directories(dpdfnet_ui_snapshot PRIVATE src)

if (MSVC)
    target_compile_options(dpdfnet_ui_snapshot PRIVATE /utf-8 /Zc:__cplusplus /MP)
endif()

target_compile_definitions(dpdfnet_ui_snapshot PRIVATE
    ORT_API_MANUAL_INIT=1
    JUCE_WEB_BROWSER=0
    JUCE_USE_CURL=0
    # タイマを数回まわしてメータを描かせるため (MessageManager::runDispatchLoopUntil)
    JUCE_MODAL_LOOPS_PERMITTED=1)

target_link_libraries(dpdfnet_ui_snapshot PRIVATE
    ${ORT_LINK}
    kissfft_static
    juce::juce_audio_utils
    juce::juce_dsp
    PUBLIC
    juce::juce_recommended_config_flags
    juce::juce_recommended_warning_flags)

if (WIN32 AND ORT_COPY_DLL)
    add_custom_command(TARGET dpdfnet_ui_snapshot POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
                "${_ort_dir}/onnxruntime.dll"
                "$<TARGET_FILE_DIR:dpdfnet_ui_snapshot>/dpdfnet_onnxruntime.dll"
        COMMENT "Copying ONNX Runtime next to the snapshot tool")
endif()
