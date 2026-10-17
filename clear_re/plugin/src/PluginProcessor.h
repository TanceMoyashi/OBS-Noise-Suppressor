#pragma once

#include <atomic>
#include <memory>
#include <vector>

#include <juce_audio_processors/juce_audio_processors.h>

#include "DpdfNet.h"
#include "Parameters.h"
#include "VorbisStft.h"

class DPDFNetVoiceProcessor : public juce::AudioProcessor
{
public:
    DPDFNetVoiceProcessor();
    ~DPDFNetVoiceProcessor() override;

    //--------------------------------------------------------------------------
    void prepareToPlay (double sampleRate, int samplesPerBlock) override;
    void releaseResources() override;
    void reset() override;

    bool isBusesLayoutSupported (const BusesLayout&) const override;
    void processBlock (juce::AudioBuffer<float>&, juce::MidiBuffer&) override;

    juce::AudioProcessorEditor* createEditor() override;
    bool hasEditor() const override { return true; }

    // juce_add_plugin が JucePlugin_Name を定義する。開発用の検証 EXE (ヘッドレス) でも
    // 同じソースを使えるようフォールバックを置く。
   #ifdef JucePlugin_Name
    const juce::String getName() const override { return JucePlugin_Name; }
   #else
    const juce::String getName() const override { return "DPDFNet Voice"; }
   #endif
    bool acceptsMidi() const override { return false; }
    bool producesMidi() const override { return false; }
    bool isMidiEffect() const override { return false; }
    double getTailLengthSeconds() const override { return 0.05; }

    int getNumPrograms() override { return 1; }
    int getCurrentProgram() override { return 0; }
    void setCurrentProgram (int) override {}
    const juce::String getProgramName (int) override { return {}; }
    void changeProgramName (int, const juce::String&) override {}

    void getStateInformation (juce::MemoryBlock&) override;
    void setStateInformation (const void*, int) override;

    //--------------------------------------------------------------------------
    juce::AudioProcessorValueTreeState apvts;

    /** エディタへ状態を渡すためのフラグ (ロックフリー)。 */
    enum class Status
    {
        ok = 0,
        hostSampleRateNotSupported,   // 48 kHz 以外
        modelNotFound,
        modelLoadFailed,
        ortUnavailable                // 同梱の ONNX Runtime をロードできなかった
    };

    Status getStatus() const noexcept        { return status.load (std::memory_order_relaxed); }
    juce::String getStatusMessage() const;
    bool isDereverbAvailable() const noexcept { return dereverbAvailable.load (std::memory_order_relaxed); }

    /** 3 バンドメータ。 left/right はチャンネル、0/1/2 は low/mid/high。 */
    float getBandLevel (int channel, int band) const noexcept
    {
        return bandLevels[(size_t) juce::jlimit (0, 1, channel)][(size_t) juce::jlimit (0, 2, band)]
                   .load (std::memory_order_relaxed);
    }

    /** 内部サンプルレート。DPDFNet 48 kHz HR モデルの native rate。 */
    static constexpr double internalSampleRate = 48000.0;

    /** DPDFNet ONNX のフレーム遅延。出力フレーム t は入力フレーム t-4 に対応する。

        根拠:
          - 公式実装 (dpdfnet パッケージ) の ATTN_LIMIT_NOISY_FRAME_OFFSET = 4
          - 公式オフライン経路は istft 出力の先頭 2*win_len = 4 hop を捨てて遅延を消す
          - tools/sim_stream.py の実測 (スペクトル照合) も 4 フレーム
        この値は Ambience ブレンドで「同じ内容の入力フレーム」を参照するのに必要。
    */
    static constexpr int kModelLookaheadHops = 4;

    /** 処理遅延 (hop 単位) = レイテンシ補償として報告する値。

        内訳 (すべて tools/sim_stream.py で実測):
          4 hop : モデル自身の内容遅延 (kModelLookaheadHops)
          1 hop : WOLA の構造。step s で確定する hop は入力 hop (s-5) の内容なので、
                  モデルの 4 hop に加えて「1 つ次のフレームを待つ」分が乗る
          1 hop : size_t 出力位置。完成した hop は次に読むサンプルから書くため、
                  出力グリッド上では 1 hop 後ろに載る
        = 6 hop。合成をスキップするフレーム数 (runHop の c.step >= 2) を
        変えても、この合計は 6 hop のままであることを実測で確認済み
        (skip を増やすと起動過渡が減るが、その分だけ prime が増える)。

        「固定長ブロックを必ず返す」VST3 で実現可能な最小値でもある。
        公式 StreamEnhancer は 5 hop だが、それはブロック長より短い返り値を
        許してホスト側にゼロ埋めさせる (sounddevice 前提の) 実装だから。

        => 6 * 480 = 2880 サンプル = 60 ms @48 kHz。
    */
    static constexpr int kLatencyHops = 6;

    static juce::AudioProcessorValueTreeState::ParameterLayout createLayout();

private:
    //--------------------------------------------------------------------------
    struct ChannelDsp
    {
        dpdfnet::VorbisStft stft;
        std::unique_ptr<dpdfnet::DpdfNet> engine;

        std::vector<float> frame;           // 2*F  現在フレームの複素スペクトル
        // 2*F x (kModelLookaheadHops)。現在フレームを書く前に読むので、
        // 読み出しは常に kModelLookaheadHops フレーム前の整列入力になる。
        std::vector<float> frameRing[kModelLookaheadHops];
        std::vector<float> outFull;         // 2*F
        std::vector<float> outNs;           // 2*F
        std::vector<float> blended;         // 2*F
        std::vector<float> hopOut;          // hop

        int step = 0;
    };

    void allocateDsp (int numChannels);
    void runHop();
    void updateParameters();
    void updateMeters (const juce::AudioBuffer<float>& output, int numSamples);
    juce::String resolveModelFile() const;

    std::vector<ChannelDsp> channels;

    int hop = 0, freqBins = 0;
    double hostSampleRate = 48000.0;
    bool dspReady = false;

    std::vector<std::vector<float>> inFifo;    // numChannels x hop
    std::vector<std::vector<float>> outFifo;   // numChannels x hop
    int inFill = 0, outFill = 0, outRead = 0;

    // パラメータのフレーム単位スムージング
    float smoothedNoisyWeight = 0.0f;
    float smoothedReverbBlend = 0.0f;

    // Voice (presence) 用 one-pole high shelf の状態
    std::vector<float> voiceHpState, voiceHpPrevX;
    float voiceHpCoef = 0.0f;
    float voiceGain = 1.0f;

    // 3 バンドメータ用の one-pole 状態 (300 Hz / 3 kHz)
    float meterLpA[2] {}, meterLpB[2] {};
    float meterLpACoef = 0.0f, meterLpBCoef = 0.0f;
    float meterBandSmoothed[2][3] {};

    // メータ
    std::atomic<float> bandLevels[2][3];

    std::atomic<Status> status { Status::modelNotFound };
    std::atomic<bool> dereverbAvailable { false };

    JUCE_DECLARE_NON_COPYABLE_WITH_LEAK_DETECTOR (DPDFNetVoiceProcessor)
};
