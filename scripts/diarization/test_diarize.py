"""Unit tests for diarize.py helpers (numpy only, no pyannote needed).

Run: python -m unittest test_diarize.py
"""

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

import numpy as np

import diarize


class ParseChannelCountTest(unittest.TestCase):
    def test_stereo(self):
        stderr = "  Stream #0:0[0x1](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 128 kb/s"
        self.assertEqual(diarize.parse_channel_count(stderr), 2)

    def test_mono(self):
        stderr = "  Stream #0:0: Audio: pcm_s16le ([1][0][0][0] / 0x0001), 16000 Hz, mono, s16, 256 kb/s"
        self.assertEqual(diarize.parse_channel_count(stderr), 1)

    def test_numbered_channels(self):
        stderr = "  Stream #0:0: Audio: flac, 48000 Hz, 4 channels, s16"
        self.assertEqual(diarize.parse_channel_count(stderr), 4)

    def test_no_audio_stream(self):
        self.assertIsNone(diarize.parse_channel_count("Invalid data found when processing input"))


class IntervalsTest(unittest.TestCase):
    def test_frames_to_intervals_merges_gaps_and_drops_short(self):
        mask = [True] * 5 + [False] * 3 + [True] * 5 + [False] * 10 + [True] * 2
        intervals = diarize.frames_to_intervals(mask, 0.1, max_gap=0.5, min_duration=0.4)
        self.assertEqual(len(intervals), 1)
        start, end = intervals[0]
        self.assertAlmostEqual(start, 0.0)
        self.assertAlmostEqual(end, 1.3)

    def test_mic_dominance(self):
        sr = diarize.SAMPLE_RATE
        rng = np.random.default_rng(0)
        loud = (0.1 * rng.standard_normal(sr * 2)).astype(np.float32)
        quiet = (0.001 * rng.standard_normal(sr * 2)).astype(np.float32)
        silence = np.zeros(sr * 2, dtype=np.float32)

        # 0-2s: only mic talks, 2-4s: only system talks (with faint bleed in mic), 4-6s: silence
        mic = np.concatenate([loud, quiet, silence])
        system = np.concatenate([silence, loud, silence])

        intervals = diarize.mic_dominance_intervals(mic, system)
        self.assertEqual(len(intervals), 1)
        start, end = intervals[0]
        self.assertAlmostEqual(start, 0.0)
        self.assertAlmostEqual(end, 2.0)

    def test_mic_dominance_empty(self):
        empty = np.zeros(0, dtype=np.float32)
        self.assertEqual(diarize.mic_dominance_intervals(empty, empty), [])


class OutputTest(unittest.TestCase):
    def test_build_and_write_output(self):
        turns = [
            diarize.make_turn(5.0, 7.12345, "SPEAKER_01", diarize.SYSTEM_CHANNEL),
            diarize.make_turn(0.5, 3.0, diarize.MIC_SPEAKER, diarize.MIC_CHANNEL),
            diarize.make_turn(8.0, 9.0, "SPEAKER_00"),
        ]
        data = diarize.build_output(turns, Path("x/audio.mp4"), "stereo", "model")
        self.assertEqual(data["speakers"], ["SPEAKER_00", "SPEAKER_01", "mic"])
        self.assertEqual([t["start"] for t in data["turns"]], [0.5, 5.0, 8.0])
        self.assertEqual(data["turns"][1]["end"], 7.123)
        self.assertNotIn("channel", data["turns"][2])

        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "diarization.json"
            diarize.write_output(data, output)
            self.assertEqual(json.loads(output.read_text(encoding="utf-8")), data)
            self.assertEqual(sorted(p.name for p in Path(tmp).iterdir()), ["diarization.json"])


class ProgressTest(unittest.TestCase):
    def capture(self, fn):
        buffer = io.StringIO()
        with contextlib.redirect_stderr(buffer):
            fn()
        return buffer.getvalue().splitlines()

    def test_disabled_prints_nothing(self):
        progress = diarize.Progress(False)
        self.assertEqual(self.capture(lambda: progress.report(50, "x")), [])

    def test_report_dedupes_and_clamps(self):
        progress = diarize.Progress(True)

        def run():
            progress.report(5, "Loading")
            progress.report(5, "Loading")
            progress.report(150, "Done")

        self.assertEqual(self.capture(run), ["PROGRESS 5 Loading", "PROGRESS 100 Done"])

    def test_pipeline_hook_maps_steps(self):
        progress = diarize.Progress(True)
        hook = progress.pipeline_hook(10, 90, "Audio")

        def run():
            hook("segmentation", None)
            hook("embeddings", None, total=10, completed=5)
            hook("unknown_step", None)
            hook("discrete_diarization", None, total=1, completed=1)

        self.assertEqual(self.capture(run), [
            "PROGRESS 10 Audio: segmentation",
            "PROGRESS 60 Audio: embeddings",
            "PROGRESS 90 Audio: discrete diarization",
        ])

    def test_pipeline_hook_never_goes_back(self):
        progress = diarize.Progress(True)
        hook = progress.pipeline_hook(0, 100, "Audio")

        def run():
            hook("segmentation", None, total=10, completed=15)
            hook("segmentation", None)
            hook("speaker_counting", None)

        self.assertEqual(self.capture(run), [
            "PROGRESS 25 Audio: segmentation",
            "PROGRESS 25 Audio: speaker counting",
        ])


class ResolveInputTest(unittest.TestCase):
    def test_folder_uses_metadata_audio_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp)
            (folder / "audio.mp4").write_bytes(b"")
            (folder / "recording.wav").write_bytes(b"")
            (folder / "metadata.json").write_text(json.dumps({"audio_file": "recording.wav", "channels": 2}))
            audio, metadata = diarize.resolve_input(folder)
            self.assertEqual(audio.name, "recording.wav")
            self.assertEqual(metadata["channels"], 2)

    def test_folder_without_metadata(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp)
            (folder / "other.m4a").write_bytes(b"")
            audio, metadata = diarize.resolve_input(folder)
            self.assertEqual(audio.name, "other.m4a")
            self.assertEqual(metadata, {})

    def test_missing_input(self):
        with self.assertRaises(FileNotFoundError):
            diarize.resolve_input("does-not-exist-anywhere")


if __name__ == "__main__":
    unittest.main()
