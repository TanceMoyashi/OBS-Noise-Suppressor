// ONNX Runtime のロードと DPDFNet の推論を、プラグイン本体と同じコードで検証する。
// GUI も JUCE も使わない (PluginProcessor の DSP 部分と同じ手順をここで再現する)。
//
// 目的:
//   1. Windows 11 の C:\Windows\System32\onnxruntime.dll (1.17) ではなく、同梱の
//      dpdfnet_onnxruntime.dll (1.31) が使われることを確認する。
//   2. DpdfNet::load / processFrame と VorbisStft が実際に動くことを確認する
//      (プラグインの C++ 経路はこれが初回実行になる)。
//   3. フレーム遅延 (期待 4 hop = 1920 サンプル) を実測する。
//
// 使い方: dpdfnet_ort_smoke [model.onnx]
//   省略時は 自分 / ../models などから dpdfnet8_48khz_hr_2out.onnx を探す。

#include "DpdfNet.h"
#include "OrtRuntime.h"
#include "VorbisStft.h"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <string>
#include <vector>

namespace
{
    constexpr double kPi = 3.14159265358979323846;

    struct RunningStats
    {
        double sumSq = 0.0;
        double peak = 0.0;
        long long count = 0;
        bool finite = true;

        void push (float x)
        {
            if (! std::isfinite (x))
                finite = false;

            sumSq += (double) x * (double) x;
            peak = std::max (peak, (double) std::fabs (x));
            ++count;
        }

        double rms() const { return count > 0 ? std::sqrt (sumSq / (double) count) : 0.0; }
    };

    double db (double x) { return x > 1.0e-12 ? 20.0 * std::log10 (x) : -240.0; }

    std::string findModel (int argc, char** argv)
    {
        if (argc > 1)
            return std::filesystem::path (argv[1]).string();

        const char* names[] = { "dpdfnet8_48khz_hr_2out.onnx",
                                "dpdfnet2_48khz_hr_2out.onnx",
                                "dpdfnet8_48khz_hr.onnx",
                                "dpdfnet2_48khz_hr.onnx" };

        std::vector<std::filesystem::path> dirs;
        auto own = std::filesystem::path (dpdfnet::ownDirectoryUtf8());

        for (int i = 0; i < 4 && ! own.empty(); ++i)
        {
            dirs.push_back (own / "models");
            own = own.parent_path();
        }

        dirs.push_back (std::filesystem::current_path() / "models");
        dirs.push_back (std::filesystem::current_path());

        std::error_code ec;
        for (const auto& d : dirs)
            for (const auto* n : names)
                if (std::filesystem::exists (d / n, ec))
                    return (d / n).string();

        return {};
    }

    /** PluginProcessor::runHop と同じ手順で 1 フレーム処理する。
        alpha=1 は「整列入力をそのまま合成」= 遅延測定用の参照経路。 */
    struct Chain
    {
        dpdfnet::VorbisStft stft;
        dpdfnet::DpdfNet&    net;
        int hop = 0, freqBins = 0, step = 0;

        std::vector<float> frame, outFull, outNs, blended, hopOut;
        std::vector<std::vector<float>> ring;
        static constexpr int kLookahead = 4;   // kModelLookaheadHops

        Chain (dpdfnet::DpdfNet& n, const dpdfnet::ModelMeta& m)
            : net (n), hop (m.hop), freqBins (m.freqBins)
        {
            stft.prepare (m.nFft, m.winLen, m.hop);

            const size_t s = (size_t) (2 * freqBins);
            frame.assign (s, 0.0f);
            outFull.assign (s, 0.0f);
            outNs.assign (s, 0.0f);
            blended.assign (s, 0.0f);
            hopOut.assign ((size_t) hop, 0.0f);
            ring.assign ((size_t) kLookahead, std::vector<float> (s, 0.0f));
        }

        void hop_ (const float* in, float alpha, RunningStats& outStats, std::vector<float>* rawOut)
        {
            stft.analyse (in, frame.data());

            const int slot = step & (kLookahead - 1);
            const float* prevNoisy = ring[(size_t) slot].data();

            net.processFrame (frame.data(), outFull.data(), outNs.data());

            // alpha=0: モデル出力 (2 出力モデルならマスク経路) / alpha=1: 整列入力
            for (int k = 0; k < 2 * freqBins; ++k)
                blended[(size_t) k] = (1.0f - alpha) * outNs[(size_t) k] + alpha * prevNoisy[k];

            const bool got = step >= 2 && stft.synthesise (blended.data(), hopOut.data());

            for (int m = 0; m < hop; ++m)
            {
                const float x = got ? hopOut[(size_t) m] : 0.0f;
                outStats.push (x);
                if (rawOut != nullptr)
                    rawOut->push_back (x);
            }

            std::copy (frame.begin(), frame.end(), ring[(size_t) slot].begin());
            ++step;
        }
    };
}

int main (int argc, char** argv)
{
    std::printf ("own directory : %s\n", dpdfnet::ownDirectoryUtf8().c_str());

    const bool ortOk = dpdfnet::ensureOrtLoaded();
    std::printf ("onnx runtime  : %s\n", dpdfnet::ortRuntimeDescription().c_str());
    std::printf ("ort usable    : %s\n", ortOk ? "yes" : "NO");

    if (! ortOk)
    {
        std::printf ("FAIL: ONNX Runtime could not be loaded\n");
        return 2;
    }

    const auto modelPath = findModel (argc, argv);
    if (modelPath.empty())
    {
        std::printf ("FAIL: model not found (pass a path as argv[1])\n");
        return 3;
    }

    dpdfnet::DpdfNet net;

    if (! net.load (modelPath, 1))
    {
        std::printf ("FAIL: model load: %s\n", net.getLastError().c_str());
        return 4;
    }

    const auto& m = net.meta();
    std::printf ("model         : %s\n", modelPath.c_str());
    std::printf ("                n_fft=%d win=%d hop=%d freq_bins=%d state=%d dereverb=%s\n",
                 m.nFft, m.winLen, m.hop, m.freqBins, m.stateSize,
                 net.hasDereverbOutput() ? "yes (spec_ns)" : "no");

    constexpr int kFrames = 200;

    // ---- A) レベル: サイン + ノイズを通す ----
    {
        Chain chain (net, m);
        RunningStats inStats, outStats;

        std::vector<float> in ((size_t) m.hop);
        unsigned int rng = 12345u;
        const auto noise = [&rng]
        {
            rng = rng * 1664525u + 1013904223u;
            return ((float) (rng >> 8) / (float) (1 << 24)) - 0.5f;
        };

        long long pos = 0;
        for (int f = 0; f < kFrames; ++f)
        {
            for (int i = 0; i < m.hop; ++i, ++pos)
            {
                const double t = (double) pos / 48000.0;
                in[(size_t) i] = 0.25f * (float) std::sin (2.0 * kPi * 220.0 * t) + 0.05f * noise();

                if (f >= 20)   // 立ち上がりは無視
                    inStats.push (in[(size_t) i]);
            }

            chain.hop_ (in.data(), 0.0f, outStats, nullptr);
        }

        std::printf ("A) level      : in %.2f dB  out %.2f dB  (diff %+.2f dB)  finite=%s\n",
                     db (inStats.rms()), db (outStats.rms()),
                     db (outStats.rms()) - db (inStats.rms()),
                     outStats.finite ? "yes" : "NO");

        if (! outStats.finite || outStats.rms() <= 0.0)
        {
            std::printf ("FAIL: output is not usable\n");
            return 5;
        }
    }

    // ---- B) 遅延: インパルスのピーク位置 ----
    for (int mode = 0; mode < 2; ++mode)
    {
        Chain chain (net, m);
        RunningStats discard;
        std::vector<float> out;

        std::vector<float> in ((size_t) m.hop, 0.0f);
        const int impulseFrame = 30;
        bool impulseSent = false;

        for (int f = 0; f < 80; ++f)
        {
            std::fill (in.begin(), in.end(), 0.0f);

            if (f == impulseFrame)
            {
                in[0] = 1.0f;
                impulseSent = true;
            }

            chain.hop_ (in.data(), mode == 0 ? 1.0f : 0.0f, discard, &out);
        }

        if (! impulseSent)
        {
            std::printf ("FAIL: impulse not sent\n");
            return 6;
        }

        size_t peakIdx = 0;
        double peak = 0.0;
        for (size_t i = 0; i < out.size(); ++i)
            if (std::fabs ((double) out[i]) > peak)
            {
                peak = std::fabs ((double) out[i]);
                peakIdx = i;
            }

        const long long expected = (long long) impulseFrame * m.hop;
        const long long lag = (long long) peakIdx - expected;

        std::printf ("B) lag (%s): peak at %lld, impulse at %lld -> lag %lld samples (%.2f frames) peak=%.4f\n",
                     mode == 0 ? "aligned noisy" : "model output",
                     (long long) peakIdx, expected, lag,
                     (double) lag / (double) m.hop, peak);

        if (mode == 0 && (lag < 3 * m.hop || lag > 7 * m.hop))
        {
            std::printf ("FAIL: unexpected latency %lld\n", lag);
            return 7;
        }
    }

    std::printf ("OK\n");
    return 0;
}
