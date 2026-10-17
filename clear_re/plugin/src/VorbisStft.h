#pragma once

#include <vector>
#include <cstddef>
#include <utility>

// n_fft = 960 = 2^6 * 3 * 5 の混合基数 FFT。juce::dsp::FFT は 2 の冪のみなので使えない。
#include "kiss_fftr.h"

namespace dpdfnet
{
    /**
        DPDFNet 用のストリーミング STFT / iSTFT。

        仕様 (公式エクスポートの metadata + onnx_model/infer_dpdfnet_onnx.py と一致):
          - n_fft == win_len == 960, hop == win_len / 2 == 480
          - 窓は Vorbis: w[i] = sin(pi/2 * sin^2(pi*(i+0.5)/(win/2)))
          - center == true (reflect) 相当。フレーム t は入力 [(t-1)*hop, (t+1)*hop) を使う
          - 生の複素スペクトルをそのまま ONNX に渡す (wnorm は外から掛けない)
          - iSTFT は窓包絡線で正規化する WOLA

        n_fft = 960 = 2^6 * 3 * 5 のため juce::dsp::FFT は使えず KissFFT (kiss_fftr) を使う。
        kiss_fftr は非正規化 (numpy/torch の rfft と同じ)、kiss_fftri は N 倍するので 1/N を掛ける。
    */
    class VorbisStft
    {
    public:
        VorbisStft();
        ~VorbisStft();

        VorbisStft (const VorbisStft&) = delete;
        VorbisStft& operator= (const VorbisStft&) = delete;

        // std::vector<ChannelDsp> の resize が MoveInsertable を要求するため、
        // デストラクタをユーザー宣言した上でムーブを明示的に定義する。
        VorbisStft (VorbisStft&& other) noexcept;
        VorbisStft& operator= (VorbisStft&& other) noexcept;

        /** 事前確保はすべてここで行う (オーディオスレッドで確保しない)。 */
        void prepare (int nFft, int winLen, int hop);

        void reset();

        int getFreqBins() const noexcept { return freqBins; }
        int getHop()      const noexcept { return hop; }
        int getWinLen()   const noexcept { return winLen; }

        /** hop 個の入力を取り込み、1 フレーム分の複素スペクトルを書き出す。

            @param hopIn          内部サンプルレートの入力 hop サンプル
            @param frameInterleaved 2*freqBins 個。[re0, im0, re1, im1, ...]
                                    (ONNX 入力 [1,1,freqBins,2] とそのまま同じ並び)
        */
        void analyse (const float* hopIn, float* frameInterleaved) noexcept;

        /** 1 フレーム分の複素スペクトルを重ね合わせ、hop 個の出力を書き出す。

            @returns 出力が有効なら true。ストリーム先頭の 1 回目だけ false
                     (重なりが片側しか揃わないため)。呼び出し側はこのとき
                     hop 個のゼロを出力して遅延会計を維持すること。
        */
        bool synthesise (const float* frameInterleaved, float* hopOut) noexcept;

    private:
        int nFft = 0, winLen = 0, hop = 0, freqBins = 0;

        std::vector<float> window;   // winLen
        std::vector<float> env;      // hop   : w[m]^2 + w[m+hop]^2
        std::vector<float> inBuf;    // winLen: sliding analysis buffer
        std::vector<float> wbuf;     // winLen: windowed scratch
        std::vector<float> acc;      // winLen + hop: overlap-add accumulator
        std::vector<float> tbuf;     // nFft  : time-domain scratch

        kiss_fftr_cfg fwdCfg = nullptr;
        kiss_fftr_cfg invCfg = nullptr;
        std::vector<float> fftScratch;       // kiss_fftr の内部作業領域

        bool firstSynthesis = true;
    };
}
