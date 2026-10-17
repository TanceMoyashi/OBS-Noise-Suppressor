#include "DpdfNet.h"
#include "OrtRuntime.h"

#include <algorithm>
#include <cstring>
#include <filesystem>
#include <stdexcept>

namespace dpdfnet
{
    namespace
    {
        // ORT の環境はプロセスに 1 つあればよい (Krisp SDK の globalInit 相当)。
        // 終了時の破棄順でクラッシュしないよう意図的にリークさせる。
        //
        // ensureOrtLoaded() は必ず Ort::Env の生成より前に必要。これを怠ると
        // Ort::GetApi() が初期化されていない状態で参照され、クラッシュする
        // (通常は DpdfNet::load() の先頭で済ませている)。ここは多層防御。
        Ort::Env& sharedEnv()
        {
            ensureOrtLoaded();
            static Ort::Env* env = new Ort::Env (ORT_LOGGING_LEVEL_WARNING, "DPDFNetVoice");
            return *env;
        }
    }

    DpdfNet::DpdfNet() = default;
    DpdfNet::~DpdfNet() = default;

    bool DpdfNet::load (const std::string& modelPathUtf8, int nThreads)
    {
        loaded = false;
        bindingsReady = false;
        session.reset();
        lastError.clear();
        numThreads = nThreads;

        if (modelPathUtf8.empty())
        {
            lastError = "model path is empty";
            return false;
        }

        // onnxruntime を明示的に「同梱のもの」へ確定させる。失敗しても例外にせず、
        // 状態として返す (ホストを落とさない)。
        if (! ensureOrtLoaded())
        {
            lastError = ortRuntimeDescription();
            return false;
        }

        try
        {
            Ort::SessionOptions so;
            so.SetIntraOpNumThreads (nThreads);
            so.SetInterOpNumThreads (1);
            so.SetGraphOptimizationLevel (GraphOptimizationLevel::ORT_ENABLE_ALL);
            so.SetExecutionMode (ExecutionMode::ORT_SEQUENTIAL);

           #ifdef _WIN32
            const std::filesystem::path p = std::filesystem::u8path (modelPathUtf8);
            const auto widePath = p.wstring();
            session = std::make_unique<Ort::Session> (sharedEnv(), widePath.c_str(), so);
           #else
            session = std::make_unique<Ort::Session> (sharedEnv(), modelPathUtf8.c_str(), so);
           #endif
        }
        catch (const std::exception& e)
        {
            lastError = std::string ("Ort::Session failed: ") + e.what();
            return false;
        }

        try
        {
            Ort::AllocatorWithDefaultOptions alloc;
            OrtAllocator* rawAlloc = alloc;   // Base<T> の暗黙変換 (GetAllocator() は無い)

            modelMeta = ModelMeta::fromMetadata (session->GetModelMetadata());
            if (! modelMeta.valid)
            {
                lastError = "ONNX metadata is not a DPDFNet streaming export "
                            "(need model_type/n_fft/hop_length/window_length/freq_bins/state_size"
                            "/erb_norm_init/spec_norm_init)";
                return false;
            }

            freqBins  = modelMeta.freqBins;
            stateSize = modelMeta.stateSize;

            // ---- 入出力名を集めて妥当性を確認する ----
            inputNames.clear();
            for (size_t i = 0; i < session->GetInputCount(); ++i)
                inputNames.emplace_back (session->GetInputNameAllocated (i, rawAlloc).get());

            outputNames.clear();
            for (size_t i = 0; i < session->GetOutputCount(); ++i)
                outputNames.emplace_back (session->GetOutputNameAllocated (i, rawAlloc).get());

            specInputIndex = stateInputIndex = -1;
            for (int i = 0; i < (int) inputNames.size(); ++i)
            {
                if (inputNames[(size_t) i] == "spec")     specInputIndex  = i;
                if (inputNames[(size_t) i] == "state_in") stateInputIndex = i;
            }

            fullOutputIndex = stateOutputIndex = nsOutputIndex = -1;
            for (int i = 0; i < (int) outputNames.size(); ++i)
            {
                const auto& n = outputNames[(size_t) i];
                if (n == "spec_e")    fullOutputIndex  = i;
                if (n == "state_out") stateOutputIndex = i;
                if (n == "spec_ns" || n == "spec_m") nsOutputIndex = i;
            }

            if (specInputIndex < 0 || stateInputIndex < 0
                || fullOutputIndex < 0 || stateOutputIndex < 0)
            {
                lastError = "unexpected ONNX I/O names; expected inputs (spec, state_in) and "
                            "outputs (spec_e, state_out)";
                return false;
            }

            // ---- 入力 shape を検証 (公式スクリプトはここから win_len を復元する) ----
            const auto specShapeInfo = session->GetInputTypeInfo ((size_t) specInputIndex)
                                              .GetTensorTypeAndShapeInfo()
                                              .GetShape();
            if (specShapeInfo.size() != 4)
            {
                lastError = "spec input must be rank 4 [1,1,F,2]";
                return false;
            }

            const auto tensorF = specShapeInfo[2];
            if (tensorF > 0 && (int) tensorF != freqBins)
            {
                lastError = "freq_bins mismatch: metadata=" + std::to_string (freqBins)
                          + " graph=" + std::to_string (tensorF);
                return false;
            }

            const auto stateShapeInfo = session->GetInputTypeInfo ((size_t) stateInputIndex)
                                               .GetTensorTypeAndShapeInfo()
                                               .GetShape();
            if (! stateShapeInfo.empty() && stateShapeInfo[0] > 0
                && (int) stateShapeInfo[0] != stateSize)
            {
                lastError = "state_size mismatch: metadata=" + std::to_string (stateSize)
                          + " graph=" + std::to_string (stateShapeInfo[0]);
                return false;
            }

            // ---- 事前確保 ----
            const size_t specCount = (size_t) (2 * freqBins);

            specBuf.assign    (specCount, 0.0f);
            outFullBuf.assign (specCount, 0.0f);
            outNsBuf.assign   (specCount, 0.0f);

            const auto initState = modelMeta.makeInitialState();
            stateBufA = initState;
            stateBufB = initState;

            specShape  = { 1, 1, (int64_t) freqBins, 2 };
            stateShape = { (int64_t) stateSize };

            const auto mem = Ort::MemoryInfo::CreateCpu (OrtArenaAllocator, OrtMemTypeDefault);

            specValue = std::make_unique<Ort::Value> (
                Ort::Value::CreateTensor<float> (mem, specBuf.data(), specBuf.size(),
                                                 specShape.data(), specShape.size()));
            outFullValue = std::make_unique<Ort::Value> (
                Ort::Value::CreateTensor<float> (mem, outFullBuf.data(), outFullBuf.size(),
                                                 specShape.data(), specShape.size()));
            outNsValue = std::make_unique<Ort::Value> (
                Ort::Value::CreateTensor<float> (mem, outNsBuf.data(), outNsBuf.size(),
                                                 specShape.data(), specShape.size()));
            stateValueA = std::make_unique<Ort::Value> (
                Ort::Value::CreateTensor<float> (mem, stateBufA.data(), stateBufA.size(),
                                                 stateShape.data(), stateShape.size()));
            stateValueB = std::make_unique<Ort::Value> (
                Ort::Value::CreateTensor<float> (mem, stateBufB.data(), stateBufB.size(),
                                                 stateShape.data(), stateShape.size()));

            const auto& specName  = inputNames[(size_t) specInputIndex];
            const auto& stateName = inputNames[(size_t) stateInputIndex];
            const auto& fullName  = outputNames[(size_t) fullOutputIndex];
            const auto& outStateName = outputNames[(size_t) stateOutputIndex];

            const auto makeBinding = [&] (Ort::Value& stateIn, Ort::Value& stateOut)
            {
                auto b = std::make_unique<Ort::IoBinding> (*session);
                b->BindInput (specName.c_str(),  *specValue);
                b->BindInput (stateName.c_str(), stateIn);
                b->BindOutput (fullName.c_str(), *outFullValue);
                b->BindOutput (outStateName.c_str(), stateOut);
                if (nsOutputIndex >= 0)
                    b->BindOutput (outputNames[(size_t) nsOutputIndex].c_str(), *outNsValue);
                return b;
            };

            bindingA = makeBinding (*stateValueA, *stateValueB);   // input A -> output B
            bindingB = makeBinding (*stateValueB, *stateValueA);   // input B -> output A

            loaded = true;
            bindingsReady = true;
            stateInA = true;
        }
        catch (const std::exception& e)
        {
            lastError = std::string ("binding setup failed: ") + e.what();
            loaded = false;
            bindingsReady = false;
            return false;
        }

        runWarmup();
        return true;
    }

    void DpdfNet::runWarmup()
    {
        // ORT 内部アリーナをここで成長させ、オーディオスレッドでの確保を避ける。
        std::fill (specBuf.begin(), specBuf.end(), 0.0f);
        for (int i = 0; i < 4; ++i)
            processFrame (specBuf.data(), outFullBuf.data(), outNsBuf.data());

        resetState();
    }

    void DpdfNet::resetState() noexcept
    {
        if (! loaded)
            return;

        const auto initState = modelMeta.makeInitialState();
        stateBufA = initState;
        stateBufB = initState;
        stateInA = true;
    }

    void DpdfNet::processFrame (const float* specInterleaved,
                               float* outFullInterleaved,
                               float* outNsInterleaved) noexcept
    {
        const size_t specCount = (size_t) (2 * freqBins);

        if (! loaded || ! bindingsReady || specInterleaved == nullptr || outFullInterleaved == nullptr)
        {
            if (outFullInterleaved != nullptr)
                std::memset (outFullInterleaved, 0, specCount * sizeof (float));
            if (outNsInterleaved != nullptr)
                std::memset (outNsInterleaved, 0, specCount * sizeof (float));
            return;
        }

        std::memcpy (specBuf.data(), specInterleaved, specCount * sizeof (float));

        auto& binding = stateInA ? *bindingA : *bindingB;

        try
        {
            session->Run (Ort::RunOptions { nullptr }, binding);
        }
        catch (const std::exception&)
        {
            // 音を止めない: 入力をそのまま返し、状態は据え置く
            std::memcpy (outFullInterleaved, specInterleaved, specCount * sizeof (float));
            if (outNsInterleaved != nullptr)
                std::memcpy (outNsInterleaved, specInterleaved, specCount * sizeof (float));
            return;
        }

        std::memcpy (outFullInterleaved, outFullBuf.data(), specCount * sizeof (float));

        if (outNsInterleaved != nullptr)
        {
            if (nsOutputIndex >= 0)
                std::memcpy (outNsInterleaved, outNsBuf.data(), specCount * sizeof (float));
            else
                std::memcpy (outNsInterleaved, outFullBuf.data(), specCount * sizeof (float));
        }

        // 新しい状態は binding の出力側バッファに入っている
        stateInA = ! stateInA;
    }
}
