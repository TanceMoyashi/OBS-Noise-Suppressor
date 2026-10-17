#pragma once

#include <memory>
#include <string>
#include <vector>

#include <onnxruntime_cxx_api.h>

#include "ModelMeta.h"

namespace dpdfnet
{
    /**
        ONNX Runtime の推論ラッパ。

        契約 (ANALYSIS.md §13 / 公式 onnx_model/infer_dpdfnet_onnx.py で確定):
            入力  spec     float32 [1, 1, F, 2]   1 フレームだけ
                  state_in float32 [state_size]
            出力  spec_e   float32 [1, 1, F, 2]   (NS + dereverb)
                  state_out float32 [state_size]
            任意  spec_ns  float32 [1, 1, F, 2]   (マスク経路のみ。グラフ手術で追加した場合)

        設計:
          - 事前確保した Ort::Value を IoBinding で束ねる。processFrame() 内で
            ヒープ確保・ファイル I/O・ロックを一切行わない。
          - state はダブルバッファ + binding の交互切替で受け渡す。
          - ORT の内部アリーナは load() 時のウォームアップで成長させ、以後は再利用させる。
    */
    class DpdfNet
    {
    public:
        DpdfNet();
        ~DpdfNet();

        DpdfNet (const DpdfNet&) = delete;
        DpdfNet& operator= (const DpdfNet&) = delete;

        /** モデルを読み込む。必ずメッセージスレッド / prepareToPlay から呼ぶこと。

            オーディオスレッドからは絶対に呼ばない (ファイル I/O とヒープ確保を伴う)。
        */
        bool load (const std::string& modelPathUtf8, int numThreads = 1);

        bool isLoaded() const noexcept { return loaded; }

        const ModelMeta& meta() const noexcept { return modelMeta; }

        /** true なら Voice Reverb ノブが機能する (2出力グラフ)。 */
        bool hasDereverbOutput() const noexcept { return nsOutputIndex >= 0; }

        const std::string& getLastError() const noexcept { return lastError; }

        /** 状態を初期値に戻す。アロケーションなし。 */
        void resetState() noexcept;

        /** 1 フレーム推論。アロケーションなし。

            @param specInterleaved   入力。[re,im] インターリーブで 2*F 個
            @param outFullInterleaved 出力 (NS + dereverb)。2*F 個
            @param outNsInterleaved   2出力モデルのときのマスク経路のみの出力。
                                      nullptr か、hasDereverbOutput() が false なら未使用
        */
        void processFrame (const float* specInterleaved,
                           float* outFullInterleaved,
                           float* outNsInterleaved) noexcept;

    private:
        bool buildBindings();
        void runWarmup();

        ModelMeta modelMeta;
        std::string lastError;
        bool loaded = false;
        int  numThreads = 1;

        std::unique_ptr<Ort::Session> session;
        std::vector<std::string> inputNames, outputNames;
        int stateInputIndex = -1;
        int specInputIndex  = -1;
        int fullOutputIndex = -1;
        int stateOutputIndex = -1;
        int nsOutputIndex   = -1;   // 追加出力 (spec_ns / spec_m)。無ければ -1

        int freqBins = 0, stateSize = 0;

        // 事前確保バッファ
        std::vector<float> specBuf, outFullBuf, outNsBuf;
        std::vector<float> stateBufA, stateBufB;
        std::vector<int64_t> specShape, stateShape;

        // Ort::Value はデータを所有しない (CreateTensor に渡したバッファを参照する)
        std::unique_ptr<Ort::Value> specValue, outFullValue, outNsValue;
        std::unique_ptr<Ort::Value> stateValueA, stateValueB;
        std::unique_ptr<Ort::IoBinding> bindingA, bindingB;

        bool stateInA = true;   // 現在の有効な状態が stateBufA にあるか
        bool bindingsReady = false;
    };
}
