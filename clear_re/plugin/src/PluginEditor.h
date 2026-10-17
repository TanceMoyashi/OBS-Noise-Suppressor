#pragma once

#include <memory>

#include <juce_audio_processors/juce_audio_processors.h>

#include "PluginProcessor.h"

/** このエディタ専用の LookAndFeel。実体は PluginEditor.cpp にある。 */
class DpfnLookAndFeel;

class DPDFNetVoiceEditor : public juce::AudioProcessorEditor,
                          private juce::Timer
{
public:
    explicit DPDFNetVoiceEditor (DPDFNetVoiceProcessor&);
    ~DPDFNetVoiceEditor() override;

    void paint (juce::Graphics&) override;
    void resized() override;

private:
    void timerCallback() override;

    struct Knob
    {
        juce::Slider slider;
        juce::Label  label;
        std::unique_ptr<juce::AudioProcessorValueTreeState::SliderAttachment> attachment;
    };

    DPDFNetVoiceProcessor& proc;

    std::unique_ptr<DpfnLookAndFeel> lnf;

    Knob ambience, voice, reverb;

    juce::TextButton bypassButton;
    std::unique_ptr<juce::AudioProcessorValueTreeState::ButtonAttachment> bypassAttachment;

    juce::Label statusLabel;

    // paint() と resized() で共有する領域
    juce::Rectangle<int> titleArea, meterArea, statusArea;

    // メータ表示用 (0..1 の正規化レベル) とピークホールド
    float meterDisplay[3] { 0.0f, 0.0f, 0.0f };
    float meterPeak[3]    { 0.0f, 0.0f, 0.0f };
    bool lastDereverbAvailable = false;

    JUCE_DECLARE_NON_COPYABLE_WITH_LEAK_DETECTOR (DPDFNetVoiceEditor)
};
