#include "PluginEditor.h"

#include <cmath>

namespace
{
    // -----------------------------------------------------------------------
    // パレット (落ち着いたダーク + 1 色のアクセント)
    // -----------------------------------------------------------------------
    namespace col
    {
        const juce::Colour bgTop     { 0xff191e24 };
        const juce::Colour bgBottom  { 0xff0c0f12 };
        const juce::Colour panel     { 0xff1b2129 };
        const juce::Colour slot      { 0xff12171c };
        const juce::Colour stroke    { 0xff2b333d };
        const juce::Colour track     { 0xff242b34 };
        const juce::Colour text      { 0xffe8ecf1 };
        const juce::Colour textDim   { 0xff8b95a3 };
        const juce::Colour textFaint { 0xff5b6572 };
        const juce::Colour accent    { 0xff57cdbc };
        const juce::Colour accent2   { 0xff86ecdc };
        const juce::Colour warm      { 0xffe0a24b };
    }

    constexpr float kRadius   = 10.0f;
    constexpr int   kMargin   = 22;
    constexpr int   kGap      = 8;
    constexpr int   kHeaderH  = 44;
    constexpr int   kFooterH  = 36;
    constexpr int   kMeterH   = 46;
    constexpr int   kMeterGap = 5;

    /** ケタ間を少し開けた UI 用フォント。既定フォントのままだと「素の JUCE」に見える。 */
    juce::Font uiFont (float height, float kerning = 0.0f)
    {
        return juce::Font (juce::FontOptions (height).withKerningFactor (kerning));
    }

    /** 線形 RMS -> 表示用 0..1 (dB スケール, -60..0 dB)。 */
    float rmsToDisplay (float rms)
    {
        const float db = juce::Decibels::gainToDecibels (juce::jmax (rms, 1.0e-6f), -60.0f);
        return juce::jlimit (0.0f, 1.0f, (db + 60.0f) / 60.0f);
    }
}

//==============================================================================
// このプラグイン専用の LookAndFeel。
// 既定のロータリ + テキストボックス枠 + チェックボックスという「素の JUCE」の
// 見た目を捨て、ノブ (アーク + インジケータ)、値のテキスト、ピル型トグルを自前で描く。
//==============================================================================
class DpfnLookAndFeel : public juce::LookAndFeel_V4
{
public:
    DpfnLookAndFeel() = default;

    /** 値のテキストボックスを「枠なしの数値」に見せる。 */
    juce::Label* createSliderTextBox (juce::Slider& s) override
    {
        auto* l = juce::LookAndFeel_V4::createSliderTextBox (s);

        l->setFont (uiFont (13.0f, 0.02f));
        l->setColour (juce::Label::textColourId, col::accent);
        l->setColour (juce::Label::textWhenEditingColourId, col::text);
        l->setColour (juce::Label::backgroundColourId, juce::Colours::transparentBlack);
        l->setColour (juce::Label::backgroundWhenEditingColourId, col::slot);
        l->setColour (juce::Label::outlineColourId, juce::Colours::transparentBlack);
        l->setColour (juce::Label::outlineWhenEditingColourId, col::accent.withAlpha (0.6f));
        return l;
    }

    void drawRotarySlider (juce::Graphics& g, int x, int y, int width, int height,
                           float sliderPos, float rotaryStartAngle, float rotaryEndAngle,
                           juce::Slider& slider) override
    {
        const bool on = slider.isEnabled();
        const auto bounds = juce::Rectangle<int> (x, y, width, height).toFloat().reduced (3.0f);
        const float diameter = juce::jmin (bounds.getWidth(), bounds.getHeight());
        const float radius   = diameter * 0.5f;
        const auto  centre   = bounds.getCentre();

        const float lineW = juce::jmax (3.0f, radius * 0.095f);
        const float arcR  = radius - lineW * 0.5f;
        const float angle = rotaryStartAngle + sliderPos * (rotaryEndAngle - rotaryStartAngle);

        const auto stroke = juce::PathStrokeType (lineW,
                                                  juce::PathStrokeType::curved,
                                                  juce::PathStrokeType::rounded);

        // 外周のトラック
        {
            juce::Path p;
            p.addCentredArc (centre.x, centre.y, arcR, arcR, 0.0f,
                             rotaryStartAngle, rotaryEndAngle, true);
            g.setColour (col::track.withMultipliedAlpha (on ? 1.0f : 0.5f));
            g.strokePath (p, stroke);
        }

        // 値のアーク
        if (sliderPos > 0.0005f)
        {
            juce::Path p;
            p.addCentredArc (centre.x, centre.y, arcR, arcR, 0.0f,
                             rotaryStartAngle, angle, true);

            const float a = on ? 1.0f : 0.4f;
            const juce::ColourGradient grad (col::accent .withMultipliedAlpha (a), centre.x - arcR, centre.y,
                                             col::accent2.withMultipliedAlpha (a), centre.x + arcR, centre.y,
                                             false);
            g.setGradientFill (grad);
            g.strokePath (p, stroke);
        }

        // ノブ本体 (上から下への淡いグラデーションで立体感を出す)
        const float bodyR = arcR - lineW * 0.95f;

        if (bodyR > 3.0f)
        {
            const auto body = juce::Rectangle<float> (bodyR * 2.0f, bodyR * 2.0f).withCentre (centre);

            const float dim = on ? 1.0f : 0.78f;
            juce::ColourGradient fill (juce::Colour (0xff303a44).withMultipliedBrightness (dim), centre.x, centre.y - bodyR,
                                       juce::Colour (0xff15191e).withMultipliedBrightness (dim), centre.x, centre.y + bodyR, false);

            g.setGradientFill (fill);
            g.fillEllipse (body);
            g.setColour (juce::Colour (0xff39424d).withMultipliedAlpha (on ? 0.85f : 0.4f));
            g.drawEllipse (body.reduced (0.5f), 1.0f);
        }

        // インジケータ
        {
            const float rIn  = bodyR * 0.30f;
            const float rOut = bodyR * 0.84f;
            const float s = std::sin (angle), c = std::cos (angle);

            juce::Path p;
            p.startNewSubPath (centre.x + rIn * s,  centre.y - rIn * c);
            p.lineTo          (centre.x + rOut * s, centre.y - rOut * c);

            g.setColour (col::text.withMultipliedAlpha (on ? 1.0f : 0.35f));
            g.strokePath (p, juce::PathStrokeType (juce::jmax (2.0f, bodyR * 0.12f),
                                                   juce::PathStrokeType::curved,
                                                   juce::PathStrokeType::rounded));
        }
    }

    juce::Font getTextButtonFont (juce::TextButton&, int) override
    {
        return uiFont (11.0f, 0.16f);
    }

    void drawButtonBackground (juce::Graphics& g, juce::Button& b, const juce::Colour&,
                               bool highlighted, bool down) override
    {
        const auto r = b.getLocalBounds().toFloat().reduced (0.5f);
        const bool on = b.getToggleState();

        auto fill = on ? col::warm.withAlpha (0.14f) : col::panel;
        if (highlighted) fill = on ? col::warm.withAlpha (0.20f) : col::panel.brighter (0.07f);
        if (down)        fill = fill.brighter (0.06f);

        g.setColour (fill);
        g.fillRoundedRectangle (r, 6.0f);

        g.setColour (on ? col::warm.withAlpha (0.8f) : col::stroke);
        g.drawRoundedRectangle (r, 6.0f, 1.0f);
    }

    void drawButtonText (juce::Graphics& g, juce::TextButton& b, bool, bool) override
    {
        g.setFont (uiFont (11.0f, 0.16f));
        g.setColour (b.getToggleState() ? col::warm : col::textDim);
        g.drawFittedText (b.getButtonText(), b.getLocalBounds(), juce::Justification::centred, 1);
    }
};

//==============================================================================
DPDFNetVoiceEditor::DPDFNetVoiceEditor (DPDFNetVoiceProcessor& p)
    : AudioProcessorEditor (&p), proc (p)
{
    lnf = std::make_unique<DpfnLookAndFeel>();
    setLookAndFeel (lnf.get());

    setSize (620, 340);

    auto& state = proc.apvts;

    auto makeKnob = [&] (Knob& k, const juce::String& name, const juce::String& paramId,
                         const juce::String& tip)
    {
        k.slider.setSliderStyle (juce::Slider::RotaryHorizontalVerticalDrag);
        k.slider.setTextBoxStyle (juce::Slider::TextBoxBelow, false, 96, 18);
        k.slider.setColour (juce::Slider::textBoxTextColourId, col::accent);
        k.slider.setColour (juce::Slider::textBoxBackgroundColourId, juce::Colours::transparentBlack);
        k.slider.setColour (juce::Slider::textBoxOutlineColourId, juce::Colours::transparentBlack);
        k.slider.setColour (juce::Slider::textBoxHighlightColourId, col::accent.withAlpha (0.25f));
        k.slider.setTooltip (tip);

        // 表示形式 ("50 %" / "0.0 dB") はパラメータ側で決めている (createLayout を参照)。
        // SliderAttachment がパラメータの getText() をここに流し込む。

        k.label.setText (name, juce::dontSendNotification);
        k.label.setJustificationType (juce::Justification::centred);
        k.label.setFont (uiFont (10.5f, 0.20f));
        k.label.setColour (juce::Label::textColourId, col::textDim);
        k.label.setInterceptsMouseClicks (false, false);

        addAndMakeVisible (k.slider);
        addAndMakeVisible (k.label);

        k.attachment = std::make_unique<juce::AudioProcessorValueTreeState::SliderAttachment> (
            state, paramId, k.slider);

        k.slider.updateText();
    };

    makeKnob (ambience, "AMBIENCE", dpdfnet::params::ambience,
              "How much room noise to keep (left = maximum suppression)");
    makeKnob (voice, "VOICE", dpdfnet::params::voice,
              "Presence and level of the enhanced voice");
    makeKnob (reverb, "REVERB", dpdfnet::params::voiceReverb,
              "Late-reverberation suppression (needs a 2-output model)");

    bypassButton.setButtonText ("BYPASS");
    bypassButton.setClickingTogglesState (true);
    bypassButton.setColour (juce::TextButton::buttonColourId, juce::Colours::transparentBlack);
    addAndMakeVisible (bypassButton);
    bypassAttachment = std::make_unique<juce::AudioProcessorValueTreeState::ButtonAttachment> (
        state, dpdfnet::params::bypass, bypassButton);

    statusLabel.setJustificationType (juce::Justification::centredLeft);
    statusLabel.setColour (juce::Label::textColourId, col::textDim);
    statusLabel.setFont (uiFont (11.0f, 0.02f));
    statusLabel.setInterceptsMouseClicks (false, false);
    addAndMakeVisible (statusLabel);

    lastDereverbAvailable = ! proc.isDereverbAvailable();
    timerCallback();

    startTimerHz (24);
}

DPDFNetVoiceEditor::~DPDFNetVoiceEditor()
{
    stopTimer();
    setLookAndFeel (nullptr);
}

//==============================================================================
void DPDFNetVoiceEditor::timerCallback()
{
    statusLabel.setText (proc.getStatusMessage(), juce::dontSendNotification);

    const bool dereverb = proc.isDereverbAvailable();
    if (dereverb != lastDereverbAvailable)
    {
        lastDereverbAvailable = dereverb;
        reverb.slider.setEnabled (dereverb);
        reverb.label.setAlpha (dereverb ? 1.0f : 0.4f);
    }

    for (int b = 0; b < 3; ++b)
    {
        const float target = rmsToDisplay (juce::jmax (proc.getBandLevel (0, b),
                                                       proc.getBandLevel (1, b)));

        // 立ち上がりは即時、戻りは滑らかに
        const float cur = meterDisplay[b];
        meterDisplay[b] = target > cur ? target : cur + (target - cur) * 0.25f;
        meterPeak[b] = juce::jmax (meterDisplay[b], meterPeak[b] - 0.02f);
    }

    repaint();
}

//==============================================================================
void DPDFNetVoiceEditor::paint (juce::Graphics& g)
{
    const auto bounds = getLocalBounds().toFloat();

    // --- 背景 ---
    g.setColour (col::bgBottom);
    g.fillRect (bounds);

    {
        juce::ColourGradient bg (col::bgTop, bounds.getCentreX(), bounds.getY(),
                                 col::bgBottom, bounds.getCentreX(), bounds.getBottom(), false);
        g.setGradientFill (bg);
        g.fillRoundedRectangle (bounds, kRadius);

        // 上部のごく淡いアクセント
        juce::ColourGradient glow (col::accent.withAlpha (0.09f), bounds.getCentreX(), bounds.getY(),
                                   col::accent.withAlpha (0.0f),
                                   bounds.getCentreX(), bounds.getY() + bounds.getHeight() * 0.42f, false);
        g.setGradientFill (glow);
        g.fillRoundedRectangle (bounds, kRadius);
    }

    g.setColour (col::stroke.withAlpha (0.85f));
    g.drawRoundedRectangle (bounds.reduced (0.5f), kRadius, 1.0f);

    // --- ヘッダ ---
    {
        auto h = titleArea;

        g.setColour (col::text);
        g.setFont (uiFont (17.0f, 0.01f));
        g.drawFittedText ("DPDFNet Voice", h.removeFromTop (21), juce::Justification::centredLeft, 1);

        g.setColour (col::textFaint);
        g.setFont (uiFont (9.5f, 0.22f));
        g.drawFittedText (juce::String::fromUTF8 ("NEURAL DE-NOISE  \xc2\xb7  DE-REVERB  \xc2\xb7  48 kHz"),
                          h.removeFromTop (14), juce::Justification::centredLeft, 1);

        g.setColour (col::stroke.withAlpha (0.7f));
        g.fillRect ((float) titleArea.getX(),
                    (float) (titleArea.getBottom() + kGap / 2),
                    (float) titleArea.getWidth(), 1.0f);
    }

    // --- 3 バンドメータ ---
    {
        static const char* bandNames[] = { "LOW", "MID", "HIGH" };

        const int labelW = 40;
        const int rowH = (meterArea.getHeight() - 2 * kMeterGap) / 3;

        for (int b = 0; b < 3; ++b)
        {
            const int rowY = meterArea.getY() + b * (rowH + kMeterGap);

            auto lab = juce::Rectangle<int> (meterArea.getX(), rowY, labelW, rowH);
            auto track = juce::Rectangle<int> (meterArea.getX() + labelW, rowY,
                                               meterArea.getWidth() - labelW, rowH);
            track = track.withSizeKeepingCentre (track.getWidth(), juce::jmin (rowH, 10));

            g.setColour (col::textDim);
            g.setFont (uiFont (9.0f, 0.18f));
            g.drawFittedText (bandNames[b], lab, juce::Justification::centredRight, 1);

            auto t = track.toFloat();
            const float round = t.getHeight() * 0.5f;

            g.setColour (col::slot);
            g.fillRoundedRectangle (t, round);

            const float level = juce::jlimit (0.0f, 1.0f, meterDisplay[b]);
            if (level > 0.002f)
            {
                auto lit = t.withWidth (t.getWidth() * level);

                juce::ColourGradient grad (col::accent, t.getX(), 0.0f,
                                           col::warm,   t.getRight(), 0.0f, false);
                grad.addColour (0.62, col::accent2);
                g.setGradientFill (grad);
                g.fillRoundedRectangle (lit, round);
            }

            const float pk = juce::jlimit (0.0f, 1.0f, meterPeak[b]);
            if (pk > 0.004f)
            {
                g.setColour (col::text.withAlpha (0.7f));
                g.fillRoundedRectangle (t.getX() + t.getWidth() * pk - 1.0f,
                                        t.getY(), 2.0f, t.getHeight(), 1.0f);
            }

            g.setColour (col::stroke.withAlpha (0.55f));
            g.drawRoundedRectangle (t.reduced (0.5f), round, 1.0f);
        }
    }

    // --- ステータスドット (左端。すぐ右に statusLabel のテキストが並ぶ) ---
    {
        const bool ok = proc.getStatus() == DPDFNetVoiceProcessor::Status::ok;
        const juce::Point<float> c { (float) statusArea.getX() + 5.0f, (float) statusArea.getCentreY() };

        g.setColour ((ok ? col::accent : col::warm).withAlpha (ok ? 0.95f : 1.0f));
        g.fillEllipse (juce::Rectangle<float> (7.0f, 7.0f).withCentre (c));
    }
}

void DPDFNetVoiceEditor::resized()
{
    auto area = getLocalBounds().reduced (kMargin);

    titleArea = area.removeFromTop (kHeaderH);
    area.removeFromTop (kGap);

    auto footer = area.removeFromBottom (kFooterH);
    bypassButton.setBounds (footer.removeFromRight (104).withSizeKeepingCentre (104, 30));
    footer.removeFromRight (kGap);
    statusArea = footer;
    statusLabel.setBounds (statusArea.withTrimmedLeft (18));

    area.removeFromBottom (kGap);

    meterArea = area.removeFromBottom (kMeterH);
    area.removeFromBottom (kGap);

    auto knobRow = area;
    const int w = knobRow.getWidth() / 3;

    auto place = [&] (Knob& k)
    {
        auto cell = knobRow.removeFromLeft (w).reduced (14, 2);
        k.label.setBounds (cell.removeFromTop (18));
        k.slider.setBounds (cell);
    };

    place (ambience);
    place (voice);
    place (reverb);
}
