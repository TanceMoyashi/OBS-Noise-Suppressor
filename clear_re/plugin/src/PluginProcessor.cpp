#include "PluginProcessor.h"
#include "PluginEditor.h"
#include "OrtRuntime.h"

#include <cmath>
#include <cstdlib>
#include <cstring>

#ifdef _WIN32
 #include <windows.h>
#endif

// パラメータ ID と変換関数は dpdfnet::params にある。
// ファイル内では `params::` と `using namespace params;` の両方を使うためエイリアスを張る。
namespace params = dpdfnet::params;

namespace
{
    // 自分の DLL / bundle のディレクトリ。VST3 は <bundle>/Contents/x86_64-win/ なので
    // models/ は 3 つ上まで探す。
    juce::File getPluginDirectory()
    {
       #ifdef _WIN32
        HMODULE h = nullptr;
        if (GetModuleHandleExW (GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                                reinterpret_cast<LPCWSTR> (&getPluginDirectory), &h))
        {
            wchar_t buf[4096] = {};
            if (GetModuleFileNameW (h, buf, (DWORD) (sizeof (buf) / sizeof (buf[0]))) > 0)
                return juce::File (juce::String (buf)).getParentDirectory();
        }
       #endif
        return {};
    }

    juce::File searchIn (const juce::File& dir, const juce::StringArray& names)
    {
        if (dir == juce::File())
            return {};

        if (dir.isDirectory())
            for (const auto& n : names)
                if (auto f = dir.getChildFile (n); f.existsAsFile())
                    return f;

        return {};
    }
}

//==============================================================================
namespace
{
    // UI のノブ下に出す文字列。SliderAttachment がパラメータ自身の getText() を
    // textFromValueFunction に流し込むため、表示形式はパラメータ側で決める必要がある
    // (エディタで設定しても上書きされる)。ホストの表示にも同じ文字列が出る。
    juce::AudioParameterFloatAttributes percentAttributes()
    {
        return juce::AudioParameterFloatAttributes()
                 .withStringFromValueFunction ([] (float v, int)
                 {
                     return juce::String (juce::roundToInt (v * 100.0f)) + " %";
                 })
                 .withValueFromStringFunction ([] (const juce::String& t)
                 {
                     return juce::jlimit (0.0f, 1.0f, t.getFloatValue() / 100.0f);
                 });
    }

    juce::AudioParameterFloatAttributes decibelAttributes()
    {
        return juce::AudioParameterFloatAttributes()
                 .withStringFromValueFunction ([] (float v, int)
                 {
                     return juce::String (v, 1) + " dB";
                 })
                 .withValueFromStringFunction ([] (const juce::String& t)
                 {
                     return t.getFloatValue();
                 });
    }
}

//==============================================================================
juce::AudioProcessorValueTreeState::ParameterLayout DPDFNetVoiceProcessor::createLayout()
{
    using namespace juce;

    AudioProcessorValueTreeState::ParameterLayout layout;

    layout.add (std::make_unique<AudioParameterFloat> (
        ParameterID { params::ambience, 1 }, "Ambience",
        NormalisableRange<float> (0.0f, 1.0f, 0.001f), 0.5f, percentAttributes()));

    layout.add (std::make_unique<AudioParameterFloat> (
        ParameterID { params::voice, 1 }, "Voice",
        NormalisableRange<float> (-12.0f, 12.0f, 0.01f), 0.0f, decibelAttributes()));

    layout.add (std::make_unique<AudioParameterFloat> (
        ParameterID { params::voiceReverb, 1 }, "Voice Reverb",
        NormalisableRange<float> (0.0f, 1.0f, 0.001f), 0.0f, percentAttributes()));

    layout.add (std::make_unique<AudioParameterBool> (
        ParameterID { params::bypass, 1 }, "Bypass", false));

    return layout;
}

//==============================================================================
DPDFNetVoiceProcessor::DPDFNetVoiceProcessor()
    : AudioProcessor (BusesProperties()
                          .withInput  ("Input",  juce::AudioChannelSet::stereo(), true)
                          .withOutput ("Output", juce::AudioChannelSet::stereo(), true)),
      apvts (*this, nullptr, "PARAMS", createLayout())
{
    for (auto& row : bandLevels)
        for (auto& v : row)
            v.store (0.0f, std::memory_order_relaxed);
}

DPDFNetVoiceProcessor::~DPDFNetVoiceProcessor() = default;

//==============================================================================
juce::String DPDFNetVoiceProcessor::resolveModelFile() const
{
    // 品質優先 → 速度優先。
    // 2 出力版 (spec_ns 付き = Voice Reverb が使える) を優先し、無ければ通常版へ。
    const juce::StringArray names { "dpdfnet8_48khz_hr_2out.onnx",
                                    "dpdfnet2_48khz_hr_2out.onnx",
                                    "dpdfnet8_48khz_hr.onnx",
                                    "dpdfnet2_48khz_hr.onnx" };

    // 1) 環境変数 (ファイルでもディレクトリでもよい)
    const auto envValue = juce::SystemStats::getEnvironmentVariable ("DPDFNET_MODEL", {});
    if (envValue.isNotEmpty())
    {
        const juce::File f (envValue);
        if (f.existsAsFile())
            return f.getFullPathName();
        if (auto hit = searchIn (f, names); hit.existsAsFile())
            return hit.getFullPathName();
    }

    juce::Array<juce::File> dirs;

    // 2) プラグインバイナリの隣 (と VST3 bundle の上)
    if (auto pluginDir = getPluginDirectory(); pluginDir != juce::File())
    {
        dirs.add (pluginDir.getChildFile ("models"));
        auto up = pluginDir;
        for (int i = 0; i < 3; ++i)
        {
            up = up.getParentDirectory();
            dirs.add (up.getChildFile ("models"));
        }
    }

    // 3) ユーザー文書フォルダ
    dirs.add (juce::File::getSpecialLocation (juce::File::userDocumentsDirectory)
                  .getChildFile ("DPDFNetVoice").getChildFile ("models"));
    dirs.add (juce::File::getSpecialLocation (juce::File::userDocumentsDirectory)
                  .getChildFile ("OBS").getChildFile ("VST3").getChildFile ("models"));

    for (const auto& d : dirs)
        if (auto hit = searchIn (d, names); hit.existsAsFile())
            return hit.getFullPathName();

    return {};
}

//==============================================================================
void DPDFNetVoiceProcessor::allocateDsp (int numChannels)
{
    hop = 0;
    freqBins = 0;
    dspReady = false;

    channels.clear();
    channels.resize ((size_t) juce::jmax (1, numChannels));
}

void DPDFNetVoiceProcessor::prepareToPlay (double sampleRate, int /*samplesPerBlock*/)
{
    hostSampleRate = sampleRate;
    smoothedNoisyWeight = 0.0f;
    smoothedReverbBlend = 0.0f;

    if (std::abs (sampleRate - internalSampleRate) > 1.0)
    {
        // v0 は 48 kHz 専用。44.1 kHz はリサンプラ実装後に解禁する。
        status.store (Status::hostSampleRateNotSupported);
        dspReady = false;
        dereverbAvailable.store (false);
        setLatencySamples (0);
        return;
    }

    const int numCh = juce::jmax (1, getTotalNumOutputChannels());
    allocateDsp (numCh);

    // 先に ONNX Runtime を確定させる。失敗の理由 (System32 の古い ORT を掴んだ等) を
    // エディタに出せるよう、専用のステータスを持つ。
    if (! dpdfnet::ensureOrtLoaded())
    {
        status.store (Status::ortUnavailable);
        dereverbAvailable.store (false);
        setLatencySamples (0);
        return;
    }

    const auto modelPath = resolveModelFile();
    if (modelPath.isEmpty())
    {
        status.store (Status::modelNotFound);
        return;
    }

    const auto utf8Path = modelPath.toStdString();

    // 各チャンネルが独立した ORT セッションと状態を持つ (モデルはモノラル入力のため)
    for (auto& c : channels)
    {
        c.engine = std::make_unique<dpdfnet::DpdfNet>();
        if (! c.engine->load (utf8Path, 1))
        {
            channels.clear();
            status.store (Status::modelLoadFailed);
            setLatencySamples (0);
            return;
        }
    }

    const auto& meta = channels.front().engine->meta();
    hop      = meta.hop;
    freqBins = meta.freqBins;

    if (hop <= 0 || freqBins <= 0)
    {
        channels.clear();
        status.store (Status::modelLoadFailed);
        setLatencySamples (0);
        return;
    }

    const size_t specCount = (size_t) (2 * freqBins);

    for (auto& c : channels)
    {
        c.stft.prepare (meta.nFft, meta.winLen, meta.hop);

        c.frame.assign    (specCount, 0.0f);
        for (auto& ring : c.frameRing) ring.assign (specCount, 0.0f);
        c.outFull.assign  (specCount, 0.0f);
        c.outNs.assign    (specCount, 0.0f);
        c.blended.assign  (specCount, 0.0f);
        c.hopOut.assign   ((size_t) hop, 0.0f);
        c.step = 0;
    }

    inFifo.assign  ((size_t) channels.size(), std::vector<float> ((size_t) hop, 0.0f));
    outFifo.assign ((size_t) channels.size(), std::vector<float> ((size_t) hop, 0.0f));
    inFill = outFill = outRead = 0;

    voiceHpState.assign ((size_t) channels.size(), 0.0f);
    voiceHpPrevX.assign ((size_t) channels.size(), 0.0f);
    voiceHpCoef = std::exp (-juce::MathConstants<float>::twoPi * 1500.0f
                                / (float) internalSampleRate);
    voiceGain = 1.0f;

    meterLpACoef = std::exp (-juce::MathConstants<float>::twoPi * 300.0f / (float) internalSampleRate);
    meterLpBCoef = std::exp (-juce::MathConstants<float>::twoPi * 3000.0f / (float) internalSampleRate);
    for (auto& v : meterLpA) v = 0.0f;
    for (auto& v : meterLpB) v = 0.0f;
    for (auto& row : meterBandSmoothed) for (auto& v : row) v = 0.0f;

    dereverbAvailable.store (channels.front().engine->hasDereverbOutput());

    // レイテンシ補償: 実測値 (ANALYSIS.md §13 / tools/sim_stream.py)。詳細は kLatencyHops。
    setLatencySamples (kLatencyHops * hop);

    updateParameters();

    status.store (Status::ok);
    dspReady = true;
}

void DPDFNetVoiceProcessor::releaseResources()
{
    // モデルは保持したまま。次回 prepareToPlay で必要なら再構成する。
}

void DPDFNetVoiceProcessor::reset()
{
    for (auto& c : channels)
    {
        if (c.engine != nullptr)
            c.engine->resetState();

        c.stft.reset();
        std::fill (c.frame.begin(), c.frame.end(), 0.0f);
        for (auto& ring : c.frameRing) std::fill (ring.begin(), ring.end(), 0.0f);
        c.step = 0;
    }

    for (auto& f : inFifo)  std::fill (f.begin(), f.end(), 0.0f);
    for (auto& f : outFifo) std::fill (f.begin(), f.end(), 0.0f);
    inFill = outFill = outRead = 0;

    std::fill (voiceHpState.begin(), voiceHpState.end(), 0.0f);
    std::fill (voiceHpPrevX.begin(), voiceHpPrevX.end(), 0.0f);
}

//==============================================================================
bool DPDFNetVoiceProcessor::isBusesLayoutSupported (const BusesLayout& layouts) const
{
    const auto in  = layouts.getMainInputChannelSet();
    const auto out = layouts.getMainOutputChannelSet();

    if (in.isDisabled() || out.isDisabled())
        return false;

    if (in != out)
        return false;

    return in == juce::AudioChannelSet::mono() || in == juce::AudioChannelSet::stereo();
}

void DPDFNetVoiceProcessor::updateParameters()
{
    using namespace params;

    const float amb     = apvts.getRawParameterValue (ambience)->load();
    const float rev     = apvts.getRawParameterValue (voiceReverb)->load();
    const float voiceDb = apvts.getRawParameterValue (voice)->load();

    // Ambience は「残す環境ノイズ量」。公式オフライン実装の attenuation limit と同じ定義で
    // noisy 側の重み α = 10^(-L/20) を作る。
    const float targetWeight = attenuationToNoisyWeight (ambienceToAttenuationDb (amb));

    constexpr float k = 0.3f;
    smoothedNoisyWeight += k * (targetWeight - smoothedNoisyWeight);
    smoothedReverbBlend += k * (juce::jlimit (0.0f, 1.0f, rev) - smoothedReverbBlend);

    voiceGain = juce::Decibels::decibelsToGain (voiceDb);
}

void DPDFNetVoiceProcessor::runHop()
{
    updateParameters();

    for (size_t ci = 0; ci < channels.size(); ++ci)
    {
        auto& c = channels[ci];

        // 1) 解析: hop 個の入力 -> 1 フレームの複素スペクトル
        c.stft.analyse (inFifo[ci].data(), c.frame.data());

        // 2) モデル出力 (step k) は常に kModelLookaheadHops フレーム前の入力に対応する。
        //    その「同じ内容の」整列入力を参照したまま推論とブレンドを行い、
        //    リングへの書き込みは最後に行う (読むスロットは 4 ステップ前に書かれたもの)。
        const int slot = c.step & (kModelLookaheadHops - 1);
        const float* prevNoisy = c.frameRing[slot].data();

        // 3) 推論 (アロケーションなし)
        c.engine->processFrame (c.frame.data(), c.outFull.data(), c.outNs.data());

        // 4) スペクトル領域ブレンド
        //    beta : 残響を残す -> ns 経路 (マスクのみ)
        //    alpha: 環境ノイズを残す -> 整列入力を混ぜる (公式 attenuation limit と同じ定義)
        const float alpha = smoothedNoisyWeight;
        const float beta  = smoothedReverbBlend;
        const float oneMinusBeta = 1.0f - beta;

        for (int k = 0; k < 2 * freqBins; ++k)
        {
            const float dere = oneMinusBeta * c.outNs[(size_t) k] + beta * c.outFull[(size_t) k];
            c.blended[(size_t) k] = (1.0f - alpha) * dere + alpha * prevNoisy[k];
        }

        // 5) 合成。step 0/1 のモデル出力はプリヒストリ (入力前のゼロ) に対応するため
        //    合成しない。起動時の過渡を避けるためのもので、レイテンシには効かない
        //    (スキップを省くとその分だけ先頭の無音が減り、実遅延は 6 hop のまま。
        //     tools/sim_stream.py で確認)。
        bool got = false;
        if (c.step >= 2)
            got = c.stft.synthesise (c.blended.data(), c.hopOut.data());

        auto& dst = outFifo[ci];

        if (got)
        {
            float& hpState = voiceHpState[ci];
            float& hpPrev  = voiceHpPrevX[ci];

            for (int m = 0; m < hop; ++m)
            {
                const float x  = c.hopOut[(size_t) m];
                const float hp = voiceHpCoef * (hpState + x - hpPrev);
                hpState = hp;
                hpPrev  = x;

                // Voice = presence シェルフ (0 dB で完全なパススルー)
                dst[(size_t) m] = x + (voiceGain - 1.0f) * hp;
            }
        }
        else
        {
            std::fill (dst.begin(), dst.end(), 0.0f);
        }

        // 6) 次ステップのために現在フレームをリングへ
        std::memcpy (c.frameRing[slot].data(), c.frame.data(), c.frame.size() * sizeof (float));

        ++c.step;
    }
}

void DPDFNetVoiceProcessor::processBlock (juce::AudioBuffer<float>& buffer, juce::MidiBuffer&)
{
    juce::ScopedNoDenormals noDenormals;

    const int numSamples = buffer.getNumSamples();
    const int numCh = juce::jmin (buffer.getNumChannels(), (int) channels.size());

    if (! dspReady || numCh <= 0)
        return;   // パススルー (モデル未ロード / 48 kHz 以外)

    if (apvts.getRawParameterValue (params::bypass)->load() > 0.5f)
        return;

    for (int i = 0; i < numSamples; ++i)
    {
        // 入力取り込み -> 1 hop 貯まったら 1 ステップ回す
        for (int ch = 0; ch < numCh; ++ch)
            inFifo[(size_t) ch][(size_t) inFill] = buffer.getReadPointer (ch)[i];

        const bool hopJustCompleted = (++inFill == hop);

        // 出力書き出し。前の hop の出力を先に吐き切ってから、次の hop を回す。
        //
        // step s で完成した hop は出力グリッドの [(s+1)*hop, (s+2)*hop) を隙間なく
        // 使う (tools/sim_stream.py の start = (s+1)*hop と同じ規約)。したがって
        // 完了サンプルでも「前の hop の最後の 1 サンプル」を書かなければならない。
        // ここで書かないと hop 個目が毎回捨てられ、10 ms ごとに 1 サンプルが
        // ゼロになる (= 100 Hz の周期的なクリック。「バリバリ」の正体)。
        // このとき内容遅延は kLatencyHops * hop のまま変わらない。
        if (outFill > 0)
        {
            for (int ch = 0; ch < numCh; ++ch)
                buffer.getWritePointer (ch)[i] = outFifo[(size_t) ch][(size_t) outRead];

            if (--outFill == 0)
                outRead = 0;
            else
                ++outRead;
        }
        else
        {
            for (int ch = 0; ch < numCh; ++ch)
                buffer.getWritePointer (ch)[i] = 0.0f;
        }

        // runHop() は outFifo を上書きするので、必ず上の書き出しの後に呼ぶ。
        if (hopJustCompleted)
        {
            runHop();
            inFill = 0;
            outFill = hop;
            outRead = 0;
        }
    }

    updateMeters (buffer, numSamples);
}

void DPDFNetVoiceProcessor::updateMeters (const juce::AudioBuffer<float>& output, int numSamples)
{
    if (numSamples <= 0)
        return;

    const int numCh = juce::jmin (output.getNumChannels(), 2);

    constexpr float decay = 0.35f;

    for (int ch = 0; ch < numCh; ++ch)
    {
        const float* p = output.getReadPointer (ch);
        float& sA = meterLpA[ch];
        float& sB = meterLpB[ch];

        double sum[3] = { 0.0, 0.0, 0.0 };

        for (int i = 0; i < numSamples; ++i)
        {
            const float x = p[i];
            sA += (1.0f - meterLpACoef) * (x - sA);
            sB += (1.0f - meterLpBCoef) * (x - sB);

            const float low  = sA;
            const float mid  = sB - sA;
            const float high = x - sB;

            sum[0] += (double) low  * low;
            sum[1] += (double) mid  * mid;
            sum[2] += (double) high * high;
        }

        for (int b = 0; b < 3; ++b)
        {
            const float rms = (float) std::sqrt (sum[b] / (double) numSamples);
            float& sm = meterBandSmoothed[ch][b];
            sm = juce::jmax (rms, sm * (1.0f - decay));
            bandLevels[ch][b].store (sm, std::memory_order_relaxed);
        }
    }
}

//==============================================================================
juce::String DPDFNetVoiceProcessor::getStatusMessage() const
{
    switch (getStatus())
    {
        case Status::ok:
            return dereverbAvailable.load() ? "48 kHz / model loaded (2 outputs)"
                                            : "48 kHz / model loaded (1 output; Voice Reverb disabled)";
        case Status::hostSampleRateNotSupported:
            return "Host sample rate is not 48 kHz - set OBS audio to 48 kHz (passthrough for now)";
        case Status::modelNotFound:
            return "DPDFNet model not found - see README section 3";
        case Status::modelLoadFailed:
            return "DPDFNet model failed to load - see README section 3";
        case Status::ortUnavailable:
            return "ONNX Runtime unavailable: " + juce::String (dpdfnet::ortRuntimeDescription());
    }

    return {};
}

//==============================================================================
juce::AudioProcessorEditor* DPDFNetVoiceProcessor::createEditor()
{
    return new DPDFNetVoiceEditor (*this);
}

void DPDFNetVoiceProcessor::getStateInformation (juce::MemoryBlock& destData)
{
    if (auto xml = apvts.copyState().createXml())
        copyXmlToBinary (*xml, destData);
}

void DPDFNetVoiceProcessor::setStateInformation (const void* data, int sizeInBytes)
{
    if (auto xml = getXmlFromBinary (data, sizeInBytes))
        if (xml->hasTagName (apvts.state.getType()))
            apvts.replaceState (juce::ValueTree::fromXml (*xml));
}

//==============================================================================
juce::AudioProcessor* JUCE_CALLTYPE createPluginFilter()
{
    return new DPDFNetVoiceProcessor();
}
