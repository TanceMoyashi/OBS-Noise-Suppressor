#pragma once

#include <vector>
#include <string>
#include <onnxruntime_cxx_api.h>

namespace dpdfnet
{
    /**
        DPDFNet の ONNX metadata から読み出した、STFT と状態管理に必要な全パラメータ。

        metadata のキー (公式エクスポートが埋め込む):
            n_fft, hop_length, window_length, window_type, normalized, center, pad_mode,
            freq_bins, erb_bins, spec_bins, state_size,
            erb_norm_state_size, spec_norm_state_size, erb_norm_init, spec_norm_init,
            sample_rate, model_type, version, profile
    */
    struct ModelMeta
    {
        bool        valid             = false;
        std::string modelType;
        std::string profile;
        int   sampleRate        = 48000;
        int   nFft              = 960;
        int   hop               = 480;
        int   winLen            = 960;
        int   freqBins          = 481;
        int   erbBins           = 32;
        int   specBins          = 96;
        int   stateSize         = 0;
        int   erbNormStateSize  = 0;
        int   specNormStateSize = 0;
        bool  centre            = true;
        std::string windowType  = "vorbis";
        std::vector<float> erbNormInit;
        std::vector<float> specNormInit;

        /** 状態ベクトルの初期値。

            ONNX グラフ内の連結順は
              erb_norm > spec_norm > encoder > erb_decoder > df_decoder > mask > df_op
            で、erb_norm / spec_norm 以外はすべてゼロ初期化。
            よって先頭 2 ブロックだけ metadata から埋め、残りはゼロでよい。
        */
        std::vector<float> makeInitialState() const;

        /** metadata を読み出す。失敗時は valid == false のまま返す。 */
        static ModelMeta fromMetadata (const Ort::ModelMetadata& meta);
    };
}
