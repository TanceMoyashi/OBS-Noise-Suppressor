#pragma once

#include <string>

namespace dpdfnet
{
    /** プロセスに読み込む ONNX Runtime を「このプラグインに同梱したもの」に確定する。

        背景 (OBS + atkAudio で OBS が即死した原因):
          VST3 のモジュールローダ (VST3 SDK public.sdk/source/vst/hosting/module_win32.cpp、
          JUCE 経由) は LoadLibraryW をそのまま呼ぶ。そのため依存 DLL の探索順に
          「プラグイン自身のディレクトリ」が入らず、Windows 11 (24H2 以降) が持つ
          C:\\Windows\\System32\\onnxruntime.dll (Windows ML 同梱, ONNX Runtime 1.17) が
          同梱版 (1.31) より先に見つかる。

          ORT は OrtGetApiBase()->GetApi(ORT_API_VERSION) で API のバージョンを照合し、
          古いランタイムでは nullptr を返す。C++ ラッパはそれを即座に参照するため、
          例外ではなくアクセス違反 (プロセス即死) になる。ホスト側にクラッシュログが
          残らないのもこのため (SEH を経由しない)。

        対策 (ANALYSIS.md §16):
          - onnxruntime を import table から外す (ORT_API_MANUAL_INIT)。
          - 同梱 DLL を一意な名前 (dpdfnet_onnxruntime.dll) で出荷し、絶対パス +
            LOAD_WITH_ALTERED_SEARCH_PATH で読み込む。System32 との名前衝突を完全に回避し、
            同梱 DLL 自身の依存 (VC++ ランタイム) も解決できる。
          - バージョン不一致は「失敗」として扱い、クラッシュさせない。

        @returns true なら Ort::InitApi() 済みで ORT が使える。結果はプロセス内で 1 回だけ
                 評価され、以降は同じ値を返す (スレッドセーフ)。
    */
    bool ensureOrtLoaded();

    /** ensureOrtLoaded() の結果説明 (成功: バージョンとパス / 失敗: 理由)。 */
    const std::string& ortRuntimeDescription();

    /** 自分 (プラグイン DLL / EXE) があるディレクトリ (UTF-8)。取得できなければ空。 */
    std::string ownDirectoryUtf8();
}
