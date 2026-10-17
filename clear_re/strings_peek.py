"""Proper strings extraction for a PE/binary: ASCII + UTF-16LE.

The earlier out/strings_all.txt was produced with a loose regex and contains
mostly random bytes, so keyword counts from it are unreliable. This script
enforces a minimum printable run length and writes two clean dumps.
"""
import re
import sys
from pathlib import Path

SRC = Path(sys.argv[1] if len(sys.argv) > 1 else r"D:\Users\Syntac\Documents\OBS\VST3\Clear.vst3")
OUT = Path(r"D:\Users\Syntac\Documents\デフォルトプロジェクト\clear_re\out")
MIN = 6

KEYWORDS = [
    "supertone", "clear", "denoise", "de-noise", "dereverb", "de-reverb", "reverb",
    "ambien", "voice", "eco", "mono", "stereo", "onnx", "ort", "tensor", "model",
    "onnxruntime", "dsp", "stem", "separat", "neural", "infer", "gpu", "cuda",
    "directml", "dxgi", "opengl", "vulkan", "webview", "html", "json", "preset",
    "license", "activate", "vst3", "FUID", "Audio Module Class", "Processor Class",
    "Controller Class", "Component Handler", "Edit Controller", "juce", "vstgui",
    "iplug", "wdl", "qt", "libtorch", "fft", "resample", "48", "44100",
]

data = SRC.read_bytes()
print(f"{SRC.name}: {len(data):,} bytes")

ascii_re = re.compile(rb"[\x20-\x7e]{%d,}" % MIN)
utf16_re = re.compile((rb"(?:[\x20-\x7e]\x00){%d,}" % MIN))

ascii_hits = [m.group().decode("latin1") for m in ascii_re.finditer(data)]
utf16_hits = [m.group().decode("utf-16-le") for m in utf16_re.finditer(data)]

OUT.mkdir(parents=True, exist_ok=True)
(OUT / "strings_ascii.txt").write_text("\n".join(ascii_hits), encoding="utf-8")
(OUT / "strings_utf16.txt").write_text("\n".join(utf16_hits), encoding="utf-8")

print(f"ascii strings: {len(ascii_hits):,}   utf16 strings: {len(utf16_hits):,}")

all_hits = ascii_hits + utf16_hits
low = [h.lower() for h in all_hits]
print("\nkeyword hits (ascii+utf16):")
for kw in KEYWORDS:
    n = sum(1 for h in low if kw.lower() in h)
    if n:
        print(f"  {kw:<20} {n}")
