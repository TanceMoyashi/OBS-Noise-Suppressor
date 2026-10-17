#include "VorbisStft.h"

#include <cmath>
#include <cstring>
#include <algorithm>

#include "kiss_fftr.h"

namespace dpdfnet
{
    namespace
    {
        constexpr double kPi = 3.1415926535897932384626433;

        // モデル側 model/utils.py の vorbis_window() と同一:
        //   half = winLen / 2
        //   s    = sin(0.5*pi*(i+0.5)/half)
        //   w[i] = sin(0.5*pi*s*s)
        // 50% オーバーラップで w[i]^2 + w[i+hop]^2 == 1 (厳密に Princen-Bradley) になる。
        void fillVorbisWindow (std::vector<float>& w, int winLen)
        {
            w.resize ((size_t) winLen);
            const double half = winLen / 2.0;
            for (int i = 0; i < winLen; ++i)
            {
                const double s = std::sin (0.5 * kPi * (i + 0.5) / half);
                w[(size_t) i] = (float) std::sin (0.5 * kPi * s * s);
            }
        }
    }

    VorbisStft::VorbisStft() = default;

    VorbisStft::~VorbisStft()
    {
        if (fwdCfg != nullptr) kiss_fftr_free (fwdCfg);
        if (invCfg != nullptr) kiss_fftr_free (invCfg);
    }

    VorbisStft::VorbisStft (VorbisStft&& other) noexcept
    {
        *this = std::move (other);
    }

    VorbisStft& VorbisStft::operator= (VorbisStft&& other) noexcept
    {
        if (this != &other)
        {
            if (fwdCfg != nullptr) kiss_fftr_free (fwdCfg);
            if (invCfg != nullptr) kiss_fftr_free (invCfg);

            nFft     = other.nFft;
            winLen   = other.winLen;
            hop      = other.hop;
            freqBins = other.freqBins;

            window     = std::move (other.window);
            env        = std::move (other.env);
            inBuf      = std::move (other.inBuf);
            wbuf       = std::move (other.wbuf);
            acc        = std::move (other.acc);
            tbuf       = std::move (other.tbuf);
            fftScratch = std::move (other.fftScratch);

            fwdCfg = other.fwdCfg;   other.fwdCfg = nullptr;
            invCfg = other.invCfg;   other.invCfg = nullptr;

            firstSynthesis = other.firstSynthesis;
        }

        return *this;
    }

    void VorbisStft::prepare (int newNfft, int newWinLen, int newHop)
    {
        nFft     = newNfft;
        winLen   = newWinLen;
        hop      = newHop;
        freqBins = nFft / 2 + 1;

        // DPDFNet の 48 kHz HR モデルは n_fft == win_len == 960, hop == win_len/2 を要求する。
        // 異なる値なら失敗させ、中途半端に動かさない。
        if (winLen != nFft || hop != winLen / 2 || (nFft % 2) != 0)
        {
            nFft = winLen = hop = freqBins = 0;
            return;
        }

        if (fwdCfg != nullptr) { kiss_fftr_free (fwdCfg); fwdCfg = nullptr; }
        if (invCfg != nullptr) { kiss_fftr_free (invCfg); invCfg = nullptr; }

        fwdCfg = kiss_fftr_alloc (nFft, 0, nullptr, nullptr);
        invCfg = kiss_fftr_alloc (nFft, 1, nullptr, nullptr);

        fillVorbisWindow (window, winLen);

        // WOLA の包絡線 env[m] = w[m]^2 + w[m+hop]^2。
        // Vorbis 窓は hop = win_len/2 で厳密に Princen-Bradley (env == 1) になるが、
        // 一般性と数値検証のために実際の和で割る。
        env.resize ((size_t) hop);
        for (int m = 0; m < hop; ++m)
        {
            const float a = window[(size_t) m];
            const float b = window[(size_t) (m + hop)];
            env[(size_t) m] = a * a + b * b;
        }

        inBuf.assign   ((size_t) winLen, 0.0f);
        wbuf.assign    ((size_t) winLen, 0.0f);
        acc.assign     ((size_t) winLen + (size_t) hop, 0.0f);
        tbuf.assign    ((size_t) nFft, 0.0f);
        fftScratch.assign ((size_t) (2 * (nFft / 2 + 1)), 0.0f);

        reset();
    }

    void VorbisStft::reset()
    {
        std::fill (inBuf.begin(), inBuf.end(), 0.0f);
        std::fill (acc.begin(), acc.end(), 0.0f);
        firstSynthesis = true;
    }

    void VorbisStft::analyse (const float* hopIn, float* frameInterleaved) noexcept
    {
        // 先頭 hop を捨てて新しい hop を末尾に足す (center=true 相当の 1 フレーム)
        std::memmove (inBuf.data(), inBuf.data() + hop, (size_t) (winLen - hop) * sizeof (float));
        std::memcpy  (inBuf.data() + (winLen - hop), hopIn, (size_t) hop * sizeof (float));

        for (int i = 0; i < winLen; ++i)
            wbuf[(size_t) i] = inBuf[(size_t) i] * window[(size_t) i];

        // 非正規化の実 FFT。freqBins 個の (re, im)。
        kiss_fftr (fwdCfg, wbuf.data(), reinterpret_cast<kiss_fft_cpx*> (fftScratch.data()));

        for (int k = 0; k < freqBins; ++k)
        {
            frameInterleaved[2 * k]     = fftScratch[(size_t) (2 * k)];
            frameInterleaved[2 * k + 1] = fftScratch[(size_t) (2 * k + 1)];
        }
    }

    bool VorbisStft::synthesise (const float* frameInterleaved, float* hopOut) noexcept
    {
        for (int k = 0; k < freqBins; ++k)
        {
            fftScratch[(size_t) (2 * k)]     = frameInterleaved[2 * k];
            fftScratch[(size_t) (2 * k + 1)] = frameInterleaved[2 * k + 1];
        }

        // irfft と同じく DC とナイキストの虚部は無視される
        auto* cpx = reinterpret_cast<kiss_fft_cpx*> (fftScratch.data());
        cpx[0].i = 0.0f;
        cpx[freqBins - 1].i = 0.0f;

        kiss_fftri (invCfg, cpx, tbuf.data());   // = nFft * ifft

        const float inv = 1.0f / (float) nFft;
        for (int i = 0; i < winLen; ++i)
            tbuf[(size_t) i] *= inv * window[(size_t) i];

        const bool emitNow = ! firstSynthesis;

        if (firstSynthesis)
        {
            std::fill (acc.begin(), acc.end(), 0.0f);
            firstSynthesis = false;
        }
        else
        {
            // hop だけ進める。acc[0..hop) は前回放出済みなので捨ててよい。
            std::memmove (acc.data(), acc.data() + hop, (size_t) winLen * sizeof (float));
            std::memset  (acc.data() + winLen, 0, (size_t) hop * sizeof (float));
        }

        for (int i = 0; i < winLen; ++i)
            acc[(size_t) i] += tbuf[(size_t) i];

        if (! emitNow)
            return false;

        for (int m = 0; m < hop; ++m)
        {
            const float e = env[(size_t) m];
            hopOut[m] = e > 1.0e-8f ? acc[(size_t) m] / e : 0.0f;
        }

        return true;
    }
}
