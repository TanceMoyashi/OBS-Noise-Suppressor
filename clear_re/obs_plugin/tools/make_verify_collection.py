#!/usr/bin/env python3
"""OBS 実機検証用のシーンコレクション（と専用プロファイル）を作る。

作るもの:

* `<scenes>/dpdfnet-verify.json`
  - シーン 1 つ (`verify`)
  - メディアソース 1 つ (`probe`): 指定 WAV をループ再生（音声のみ）
  - `probe` に **OBS Noise Suppressor フィルタ**を挿す（`--no-filter` で外せる）
  - グローバル音声デバイスは入れない（余計な音を混ぜないため）
* `<profiles>/dpdfnet-verify/basic.ini`
  - 48 kHz / Stereo（プラグインの前提）だけを固定した専用プロファイル

これを `run_obs_verify.ps1` から使う。生成物はユーザーの既存コレクションとは
別ファイルなので、消せば元に戻る。
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import uuid
from pathlib import Path

# OBS が保存する `prev_ver`（32.2.2）。互換のため合わせておく。
PREV_VER = 537001986
FILTER_ID = "dpdfnet_voice_filter"
FILTER_NAME = "OBS Noise Suppressor"

# フィルタの既定値（src/dsp/params.rs と一致させる）。
DEFAULT_SETTINGS = {
    "ambience": 0.5,
    "voice": 0.0,
    "voice_reverb": 0.0,
}


def apdata(*parts: str) -> Path:
    base = os.environ.get("APPDATA")
    if not base:
        raise SystemExit("APPDATA がありません（Windows で実行してください）")
    return Path(base).joinpath("obs-studio", *parts)


def hotkeys(*names: str) -> dict:
    return {n: [] for n in names}


def media_source(name: str, wav: Path, uuid_str: str) -> dict:
    return {
        "prev_ver": PREV_VER,
        "name": name,
        "uuid": uuid_str,
        "id": "ffmpeg_source",
        "versioned_id": "ffmpeg_source",
        "settings": {
            "color_range": 1,
            "local_file": str(wav).replace("\\", "/"),
            "looping": True,
            "restart_on_activate": True,
            "close_when_inactive": False,
            "clear_on_media_end": False,
        },
        "mixers": 255,
        "sync": 0,
        "flags": 0,
        "volume": 1.0,
        "balance": 0.5,
        "enabled": True,
        "muted": False,
        "push-to-mute": False,
        "push-to-mute-delay": 0,
        "push-to-talk": False,
        "push-to-talk-delay": 0,
        "hotkeys": hotkeys(
            "libobs.mute",
            "libobs.unmute",
            "libobs.push-to-mute",
            "libobs.push-to-talk",
            "MediaSource.Restart",
            "MediaSource.Play",
            "MediaSource.Pause",
            "MediaSource.Stop",
        ),
        "deinterlace_mode": 0,
        "deinterlace_field_order": 0,
        "monitoring_type": 0,
        "private_settings": {},
    }


def filter_entry(name: str = FILTER_NAME) -> dict:
    return {
        "prev_ver": PREV_VER,
        "name": name,
        "uuid": str(uuid.uuid4()),
        "id": FILTER_ID,
        "versioned_id": FILTER_ID,
        "settings": dict(DEFAULT_SETTINGS),
        "enabled": True,
    }


def scene_item(name: str, source_uuid: str, item_id: int) -> dict:
    return {
        "name": name,
        "source_uuid": source_uuid,
        "visible": True,
        "locked": False,
        "rot": 0.0,
        "scale_ref": {"x": 1920.0, "y": 1080.0},
        "align": 5,
        "bounds_type": 0,
        "bounds_align": 0,
        "bounds_crop": False,
        "crop_left": 0,
        "crop_top": 0,
        "crop_right": 0,
        "crop_bottom": 0,
        "id": item_id,
        "group_item_backup": False,
        "pos": {"x": 0.0, "y": 0.0},
        "pos_rel": {"x": 0.0, "y": 0.0},
        "scale": {"x": 1.0, "y": 1.0},
        "scale_rel": {"x": 1.0, "y": 1.0},
        "bounds": {"x": 1920.0, "y": 1080.0},
        "bounds_rel": {"x": 1.0, "y": 1.0},
        "scale_filter": "disable",
        "blend_method": "default",
        "blend_type": "normal",
        "show_transition": {"duration": 300},
        "hide_transition": {"duration": 300},
        "private_settings": {},
    }


def build_collection(name: str, wav: Path, with_filter: bool) -> dict:
    media_uuid = str(uuid.uuid4())
    media = media_source("probe", wav, media_uuid)
    if with_filter:
        media["filters"] = [filter_entry()]

    scene_uuid = str(uuid.uuid4())
    scene = {
        "prev_ver": PREV_VER,
        "name": "verify",
        "uuid": scene_uuid,
        "id": "scene",
        "versioned_id": "scene",
        "settings": {
            "custom_size": False,
            "id_counter": 1,
            "items": [scene_item("probe", media_uuid, 1)],
        },
        "mixers": 0,
        "sync": 0,
        "flags": 0,
        "volume": 1.0,
        "balance": 0.5,
        "enabled": True,
        "muted": False,
        "push-to-mute": False,
        "push-to-mute-delay": 0,
        "push-to-talk": False,
        "push-to-talk-delay": 0,
        "hotkeys": hotkeys(
            "libobs.mute",
            "libobs.unmute",
            "libobs.push-to-mute",
            "libobs.push-to-talk",
        ),
        "deinterlace_mode": 0,
        "deinterlace_field_order": 0,
        "monitoring_type": 0,
        "private_settings": {},
    }

    return {
        "current_program_scene": "verify",
        "current_scene": "verify",
        "current_transition": "Fade",
        "groups": [],
        "modules": {},
        "name": name,
        "preview_locked": False,
        "quick_transitions": [],
        "saved_projectors": [],
        "scaling_enabled": False,
        "scaling_level": -16,
        "scaling_off_x": 0.0,
        "scaling_off_y": 0.0,
        "scene_order": [{"name": "verify"}],
        "sources": [scene, media],
        "transition_duration": 300,
        "transitions": [],
        "virtual-camera": {"type2": 3},
        "canvases": [],
        "version": 2,
        "resolution": {"x": 1920, "y": 1080},
    }


PROFILE_INI = """[General]
Name={name}

[Audio]
SampleRate=48000
ChannelSetup=Stereo

[Video]
BaseCX=1920
BaseCY=1080
OutputCX=1920
OutputCY=1080
FPSType=1
FPSCommon=30

[Output]
Mode=Advanced
"""


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--name", default="dpdfnet-verify")
    ap.add_argument("--wav", default=None, help="再生する WAV（既定: リポジトリの speech_tail_reverb_48k.wav）")
    ap.add_argument("--scenes-dir", default=None, help="既定: %%APPDATA%%\\obs-studio\\basic\\scenes")
    ap.add_argument("--profiles-dir", default=None, help="既定: %%APPDATA%%\\obs-studio\\basic\\profiles")
    ap.add_argument("--no-filter", action="store_true", help="フィルタ無しの参照用コレクションを作る")
    ap.add_argument("--no-profile", action="store_true", help="プロファイルを作らない")
    args = ap.parse_args()

    repo = Path(__file__).resolve().parent.parent  # obs_plugin/
    wav = Path(args.wav) if args.wav else repo.parent / "plugin" / "test_signals" / "speech_tail_reverb_48k.wav"
    if not wav.is_file():
        raise SystemExit(f"WAV が見つかりません: {wav}")

    scenes_dir = Path(args.scenes_dir) if args.scenes_dir else apdata("basic", "scenes")
    scenes_dir.mkdir(parents=True, exist_ok=True)
    out = scenes_dir / f"{args.name}.json"
    data = build_collection(args.name, wav, not args.no_filter)
    out.write_text(json.dumps(data, ensure_ascii=False, indent=4), encoding="utf-8")
    print(f"collection: {out}")
    print(f"  wav      : {wav}")
    print(f"  filter   : {'なし' if args.no_filter else FILTER_ID}")

    if not args.no_profile:
        profiles_dir = Path(args.profiles_dir) if args.profiles_dir else apdata("basic", "profiles")
        pdir = profiles_dir / args.name
        pdir.mkdir(parents=True, exist_ok=True)
        ini = pdir / "basic.ini"
        if not ini.exists():
            ini.write_text(PROFILE_INI.format(name=args.name), encoding="utf-8")
            print(f"profile   : {ini}")
        else:
            print(f"profile   : {ini} (既存)")

    return 0


if __name__ == "__main__":
    sys.exit(main())
