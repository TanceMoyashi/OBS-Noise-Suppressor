#include "ModelMeta.h"

#include <sstream>
#include <cstdlib>

namespace dpdfnet
{
    namespace
    {
        bool has (const Ort::ModelMetadata& m, OrtAllocator* alloc, const char* key)
        {
            auto v = m.LookupCustomMetadataMapAllocated (key, alloc);
            return v != nullptr;
        }

        std::string get (const Ort::ModelMetadata& m, OrtAllocator* alloc, const char* key)
        {
            auto v = m.LookupCustomMetadataMapAllocated (key, alloc);
            return v != nullptr ? std::string (v.get()) : std::string();
        }

        int getInt (const Ort::ModelMetadata& m, OrtAllocator* alloc, const char* key, int fallback)
        {
            const auto s = get (m, alloc, key);
            return s.empty() ? fallback : (int) std::strtol (s.c_str(), nullptr, 10);
        }

        float getFloat (const Ort::ModelMetadata& m, OrtAllocator* alloc, const char* key, float fallback)
        {
            const auto s = get (m, alloc, key);
            return s.empty() ? fallback : (float) std::strtod (s.c_str(), nullptr);
        }

        // metadata の "1,2,3" 形式 (serialize_float_list) を読む
        std::vector<float> getFloatList (const Ort::ModelMetadata& m, OrtAllocator* alloc, const char* key)
        {
            std::vector<float> out;
            std::istringstream ss (get (m, alloc, key));
            std::string tok;
            while (std::getline (ss, tok, ','))
                if (! tok.empty())
                    out.push_back ((float) std::strtod (tok.c_str(), nullptr));
            return out;
        }
    }

    ModelMeta ModelMeta::fromMetadata (const Ort::ModelMetadata& meta)
    {
        ModelMeta r;
        // Ort::AllocatorWithDefaultOptions は Base<T> の暗黙変換で OrtAllocator* になる
        // (GetAllocator() というメソッドは存在しない)。既定アロケータは ORT が所有する。
        Ort::AllocatorWithDefaultOptions allocObject;
        OrtAllocator* alloc = allocObject;

        if (! has (meta, alloc, "model_type"))
            return r;  // DPDFNet のストリーミング・エクスポートではない

        r.modelType = get (meta, alloc, "model_type");
        r.profile   = get (meta, alloc, "profile");

        r.sampleRate = getInt (meta, alloc, "sample_rate", 48000);
        r.nFft       = getInt (meta, alloc, "n_fft", 960);
        r.hop        = getInt (meta, alloc, "hop_length", 480);
        r.winLen     = getInt (meta, alloc, "window_length", r.nFft);
        r.freqBins   = getInt (meta, alloc, "freq_bins", r.nFft / 2 + 1);
        r.erbBins    = getInt (meta, alloc, "erb_bins", 32);
        r.specBins   = getInt (meta, alloc, "spec_bins", 96);
        r.stateSize  = getInt (meta, alloc, "state_size", 0);
        r.erbNormStateSize  = getInt (meta, alloc, "erb_norm_state_size", 0);
        r.specNormStateSize = getInt (meta, alloc, "spec_norm_state_size", 0);
        r.centre     = getInt (meta, alloc, "center", 1) != 0;
        r.windowType = get (meta, alloc, "window_type");

        r.erbNormInit  = getFloatList (meta, alloc, "erb_norm_init");
        r.specNormInit = getFloatList (meta, alloc, "spec_norm_init");

        // 公式スクリプトと同じ健全性チェック
        const bool ok = r.windowType == "vorbis"
                     && r.winLen == r.nFft
                     && r.hop == r.winLen / 2
                     && r.freqBins == r.nFft / 2 + 1
                     && r.stateSize > 0
                     && (int) r.erbNormInit.size()  == r.erbNormStateSize
                     && (int) r.specNormInit.size() == r.specNormStateSize
                     && r.erbNormStateSize + r.specNormStateSize <= r.stateSize;

        if (! ok)
            return r;

        r.valid = true;
        return r;
    }

    std::vector<float> ModelMeta::makeInitialState() const
    {
        std::vector<float> s ((size_t) stateSize, 0.0f);
        if (! valid)
            return s;

        for (size_t i = 0; i < erbNormInit.size(); ++i)
            s[i] = erbNormInit[i];

        for (size_t i = 0; i < specNormInit.size(); ++i)
            s[(size_t) erbNormStateSize + i] = specNormInit[i];

        return s;
    }
}
