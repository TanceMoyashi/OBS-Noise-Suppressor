#pragma once

#include <juce_audio_processors/juce_audio_processors.h>

namespace dpdfnet::params
{
    inline constexpr const char* ambience    = "ambience";     // 0..1  残す環境ノイズ量 (1 = 抑圧が弱い)
    inline constexpr const char* voice       = "voice";        // -12..+12 dB presence / gain
    inline constexpr const char* voiceReverb = "voiceReverb";  // 0..1  残す残響量 (1 = そのまま)
    inline constexpr const char* bypass      = "bypass";       // bool

    // Ambience -> 抑圧リミット [dB]。ノブ右回しで残響/暗騒音が増える。
    inline constexpr float minAttenuationDb = 6.0f;   // ambience == 1.0
    inline constexpr float maxAttenuationDb = 48.0f;  // ambience == 0.0

    inline float ambienceToAttenuationDb (float ambience01) noexcept
    {
        return juce::jmap (juce::jlimit (0.0f, 1.0f, ambience01), 1.0f, 0.0f,
                           minAttenuationDb, maxAttenuationDb);
    }

    // 公式 DPDFNet オフライン実装と同じ定義: noisy の重み = 10^(-L/20)
    inline float attenuationToNoisyWeight (float limitDb) noexcept
    {
        return juce::Decibels::decibelsToGain (-limitDb);
    }
}
