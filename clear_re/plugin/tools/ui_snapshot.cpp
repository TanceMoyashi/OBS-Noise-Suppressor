// 開発用: エディタをヘッドレスで描画して PNG に出す (見た目の確認用、配布物には含めない)。
//
//   .\build\Release\dpdfnet_ui_snapshot.exe ui.png
//
// OBS を起動しなくてもレイアウト・配色・フォントを確認できる。
#include <iostream>

#include <juce_gui_basics/juce_gui_basics.h>

#include "PluginEditor.h"
#include "PluginProcessor.h"

int main (int argc, char** argv)
{
    juce::ScopedJuceInitialiser_GUI gui;

    const auto out = juce::File::getCurrentWorkingDirectory()
                         .getChildFile (argc > 1 ? argv[1] : "ui_snapshot.png");

    DPDFNetVoiceProcessor proc;
    proc.setPlayConfigDetails (2, 2, 48000.0, 512);
    proc.prepareToPlay (48000.0, 512);

    std::unique_ptr<juce::AudioProcessorEditor> ed (proc.createEditor());
    ed->setSize (620, 340);
    ed->setVisible (true);

    // メータを動かすために音を流す (内容はレイアウト確認には影響しない)
    juce::AudioBuffer<float> buf (2, 512);
    juce::MidiBuffer midi;

    for (int n = 0; n < 40; ++n)
    {
        for (int i = 0; i < 512; ++i)
        {
            const float t = (float) (n * 512 + i) / 48000.0f;
            const float v = 0.30f * std::sin (juce::MathConstants<float>::twoPi * 220.0f * t);
            buf.setSample (0, i, v);
            buf.setSample (1, i, v);
        }

        proc.processBlock (buf, midi);
    }

    // エディタのタイマ (24 Hz) を数回走らせてメータとステータスを反映させる
    juce::MessageManager::getInstance()->runDispatchLoopUntil (300);

    auto img = ed->createComponentSnapshot (ed->getLocalBounds(), true, 2.0f);

    juce::PNGImageFormat png;
    if (auto stream = std::unique_ptr<juce::FileOutputStream> (out.createOutputStream()))
        png.writeImageToStream (img, *stream);

    std::cout << "status : " << proc.getStatusMessage() << std::endl;
    std::cout << "wrote  : " << out.getFullPathName() << std::endl;
    return 0;
}
