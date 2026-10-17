#!/usr/bin/env python3
"""Speaker diarization for Meetily recordings.

Runs pyannote speaker diarization on a meeting recording and writes a
diarization.json file that Meetily can merge into the transcript
("Speakers" button in the meeting details view).

Multitrack stereo recordings (microphone = left, system audio = right):
  - the right channel is diarized with pyannote (remote participants);
  - the left channel is labelled "mic" (you) where it clearly dominates the
    right channel, using a simple energy heuristic (no model needed).

Mono recordings are diarized as a whole.

Usage:
  python diarize.py <meeting folder or audio file> [options]

See README.md in this folder for setup instructions.
"""

import argparse
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

SAMPLE_RATE = 16000
DEFAULT_MODEL = "pyannote/speaker-diarization-community-1"
OUTPUT_NAME = "diarization.json"
AUDIO_CANDIDATES = ("audio.mp4", "audio.m4a", "audio.wav", "audio.mp3")
AUDIO_EXTENSIONS = (".mp4", ".m4a", ".wav", ".mp3", ".flac", ".ogg", ".aac", ".mkv", ".webm", ".wma")

MIC_SPEAKER = "mic"
MIC_CHANNEL = "mic"
SYSTEM_CHANNEL = "system"


def find_audio_file(folder):
    """Find the recording inside a meeting folder (same order as the app)."""
    for name in AUDIO_CANDIDATES:
        candidate = folder / name
        if candidate.is_file():
            return candidate
    for candidate in sorted(folder.iterdir()):
        if candidate.is_file() and candidate.suffix.lower() in AUDIO_EXTENSIONS:
            return candidate
    raise FileNotFoundError(f"No audio file found in {folder}")


def read_metadata(folder):
    """Read metadata.json from a meeting folder, or return an empty dict."""
    path = folder / "metadata.json"
    if not path.is_file():
        return {}
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        print(f"Warning: could not read {path}: {e}", file=sys.stderr)
        return {}


def resolve_input(input_path):
    """Return (audio_path, metadata) for a meeting folder or an audio file."""
    path = Path(input_path).expanduser().resolve()
    if path.is_dir():
        metadata = read_metadata(path)
        audio_name = metadata.get("audio_file")
        if audio_name and (path / audio_name).is_file():
            return path / audio_name, metadata
        return find_audio_file(path), metadata
    if path.is_file():
        return path, read_metadata(path.parent)
    raise FileNotFoundError(f"Input not found: {path}")


def parse_channel_count(ffmpeg_stderr):
    """Parse the channel count of the first audio stream from `ffmpeg -i` output."""
    match = re.search(r"Audio: .*?, \d+ Hz, ([^,\n]+)", ffmpeg_stderr)
    if not match:
        return None
    layout = match.group(1).strip()
    if layout == "mono":
        return 1
    if layout.startswith("stereo"):
        return 2
    channels = re.match(r"(\d+) channels", layout)
    if channels:
        return int(channels.group(1))
    return None


def probe_channels(audio_path, ffmpeg):
    result = subprocess.run(
        [ffmpeg, "-hide_banner", "-nostdin", "-i", str(audio_path)],
        capture_output=True,
        text=True,
        errors="replace",
    )
    return parse_channel_count(result.stderr)


def decode_audio(audio_path, channels, ffmpeg):
    """Decode audio to float32 PCM at 16 kHz. Returns an array (channels, samples)."""
    result = subprocess.run(
        [
            ffmpeg, "-nostdin", "-v", "error",
            "-i", str(audio_path),
            "-f", "f32le", "-acodec", "pcm_f32le",
            "-ac", str(channels), "-ar", str(SAMPLE_RATE),
            "-",
        ],
        capture_output=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"ffmpeg failed: {result.stderr.decode(errors='replace').strip()}")
    samples = np.frombuffer(result.stdout, dtype=np.float32)
    usable = len(samples) - (len(samples) % channels)
    return samples[:usable].reshape(-1, channels).T.copy()


def frames_to_intervals(mask, frame_seconds, max_gap, min_duration):
    """Convert a boolean frame mask into (start, end) intervals in seconds."""
    intervals = []
    start = None
    for i, active in enumerate(mask):
        if active and start is None:
            start = i
        elif not active and start is not None:
            intervals.append([start * frame_seconds, i * frame_seconds])
            start = None
    if start is not None:
        intervals.append([start * frame_seconds, len(mask) * frame_seconds])

    merged = []
    for interval in intervals:
        if merged and interval[0] - merged[-1][1] <= max_gap:
            merged[-1][1] = interval[1]
        else:
            merged.append(interval)
    return [(s, e) for s, e in merged if e - s >= min_duration]


def mic_dominance_intervals(mic, system, sample_rate=SAMPLE_RATE, frame_seconds=0.1,
                            ratio_db=6.0, floor_db=-45.0, max_gap=0.5, min_duration=0.4):
    """Intervals where the microphone channel clearly dominates the system channel.

    A frame counts as "mic" when the mic level is above floor_db (dBFS) and at
    least ratio_db louder than the system channel. Higher ratio_db means fewer
    false "mic" labels when remote voices bleed into the microphone.
    """
    frame = max(1, int(round(sample_rate * frame_seconds)))
    count = min(len(mic), len(system)) // frame
    if count == 0:
        return []
    mic_frames = mic[: count * frame].reshape(count, frame)
    sys_frames = system[: count * frame].reshape(count, frame)
    eps = 1e-10
    mic_db = 20.0 * np.log10(np.sqrt(np.mean(mic_frames ** 2, axis=1)) + eps)
    sys_db = 20.0 * np.log10(np.sqrt(np.mean(sys_frames ** 2, axis=1)) + eps)
    mask = (mic_db >= floor_db) & (mic_db - sys_db >= ratio_db)
    return frames_to_intervals(mask, frame / sample_rate, max_gap, min_duration)


def load_pipeline(model, token, device):
    import torch
    from pyannote.audio import Pipeline

    try:
        pipeline = Pipeline.from_pretrained(model, token=token)
    except TypeError:
        # pyannote.audio 3.x
        pipeline = Pipeline.from_pretrained(model, use_auth_token=token)
    if pipeline is None:
        raise RuntimeError(
            f"Could not load {model}. Accept the model conditions on Hugging Face "
            "and pass a valid token (--hf-token or HF_TOKEN)."
        )

    if device == "auto":
        device = "cuda" if torch.cuda.is_available() else "cpu"
    pipeline.to(torch.device(device))
    print(f"Loaded {model} on {device}", file=sys.stderr)
    return pipeline


class Progress:
    """Prints machine readable progress lines for the Meetily app (--progress).

    Format: "PROGRESS <percent> <message>" on stderr. Disabled by default.
    The percentage never goes down, even if pyannote reports a step twice.
    """

    # pyannote pipeline steps, in order, with their share of the diarization range
    STEPS = ("segmentation", "speaker_counting", "embeddings", "discrete_diarization")

    def __init__(self, enabled):
        self.enabled = enabled
        self.last = None
        self.percent = 0

    def report(self, percent, message):
        if not self.enabled:
            return
        self.percent = max(self.percent, int(max(0, min(100, percent))))
        if (self.percent, message) == self.last:
            return
        self.last = (self.percent, message)
        print(f"PROGRESS {self.percent} {message}", file=sys.stderr, flush=True)

    def pipeline_hook(self, start, end, label):
        """Hook for pyannote pipelines, mapping its steps into [start, end] percent."""
        def hook(step_name, step_artifact, file=None, total=None, completed=None):
            index = self.STEPS.index(step_name) if step_name in self.STEPS else None
            if index is None:
                return
            fraction = min(1.0, completed / total) if total and completed is not None else 0.0
            share = (index + fraction) / len(self.STEPS)
            self.report(start + (end - start) * share, f"{label}: {step_name.replace('_', ' ')}")
        return hook


def diarize(pipeline, waveform, num_speakers=None, min_speakers=None, max_speakers=None, hook=None):
    """Run pyannote on a mono waveform. Returns a list of (start, end, label)."""
    import torch

    options = {}
    if num_speakers:
        options["num_speakers"] = num_speakers
    if min_speakers:
        options["min_speakers"] = min_speakers
    if max_speakers:
        options["max_speakers"] = max_speakers

    if hook is not None:
        options["hook"] = hook

    audio = {"waveform": torch.from_numpy(waveform[np.newaxis, :]), "sample_rate": SAMPLE_RATE}
    output = pipeline(audio, **options)

    # pyannote 4: prefer the exclusive (one speaker at a time) diarization, which
    # is meant for aligning with transcripts. pyannote 3 returns an Annotation.
    annotation = getattr(output, "exclusive_speaker_diarization", None)
    if annotation is None:
        annotation = getattr(output, "speaker_diarization", output)

    return [(segment.start, segment.end, label)
            for segment, _, label in annotation.itertracks(yield_label=True)]


def build_output(turns, audio_path, mode, model):
    turns = sorted(turns, key=lambda t: (t["start"], t["end"]))
    return {
        "version": 1,
        "generator": "meetily-diarize",
        "model": model,
        "audio_file": Path(audio_path).name,
        "mode": mode,
        "created_at": datetime.now(timezone.utc).isoformat(),
        "speakers": sorted({t["speaker"] for t in turns}),
        "turns": turns,
    }


def make_turn(start, end, speaker, channel=None):
    turn = {"start": round(float(start), 3), "end": round(float(end), 3), "speaker": str(speaker)}
    if channel:
        turn["channel"] = channel
    return turn


def write_output(data, output_path):
    output_path = Path(output_path)
    temp_path = output_path.with_name(f".{output_path.name}.tmp")
    temp_path.write_text(json.dumps(data, indent=2), encoding="utf-8")
    os.replace(temp_path, output_path)


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description="Speaker diarization for Meetily recordings")
    parser.add_argument("input", help="Meeting folder or audio file")
    parser.add_argument("-o", "--output", help=f"Output file (default: {OUTPUT_NAME} next to the audio)")
    parser.add_argument("--mode", choices=("auto", "stereo", "mono"), default="auto",
                        help="stereo = mic left / system right, mono = diarize the mixed audio")
    parser.add_argument("--model", default=DEFAULT_MODEL, help="pyannote pipeline on Hugging Face")
    parser.add_argument("--hf-token", default=None, help="Hugging Face token (default: HF_TOKEN env)")
    parser.add_argument("--device", choices=("auto", "cpu", "cuda"), default="auto")
    parser.add_argument("--ffmpeg", default="ffmpeg", help="Path to the ffmpeg executable")
    parser.add_argument("--num-speakers", type=int, help="Exact number of speakers (system channel in stereo mode)")
    parser.add_argument("--min-speakers", type=int)
    parser.add_argument("--max-speakers", type=int)
    parser.add_argument("--mic-ratio-db", type=float, default=6.0,
                        help="How much louder the mic must be than the system channel to count as you")
    parser.add_argument("--mic-floor-db", type=float, default=-45.0,
                        help="Minimum mic level (dBFS) to count as speech")
    parser.add_argument("--progress", action="store_true",
                        help="Print machine readable PROGRESS lines (used by the Meetily app)")
    return parser.parse_args(argv)


def main(argv=None):
    # Folder names may contain non ASCII characters; never fail on printing them
    if hasattr(sys.stderr, "reconfigure"):
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")

    args = parse_args(argv)
    progress = Progress(args.progress)
    audio_path, metadata = resolve_input(args.input)
    print(f"Audio: {audio_path}", file=sys.stderr)
    progress.report(0, "Decoding audio")

    channels = probe_channels(audio_path, args.ffmpeg)
    if channels is None:
        channels = int(metadata.get("channels") or 1)
    mode = args.mode
    if mode == "auto":
        mode = "stereo" if channels == 2 else "mono"
    if mode == "stereo" and channels != 2:
        raise SystemExit(f"Stereo mode needs a 2 channel file, found {channels} channel(s)")

    audio = decode_audio(audio_path, 2 if mode == "stereo" else 1, args.ffmpeg)
    duration = audio.shape[1] / SAMPLE_RATE
    print(f"Decoded {duration:.1f}s ({mode})", file=sys.stderr)

    progress.report(5, "Loading diarization model")
    token = args.hf_token or os.environ.get("HF_TOKEN")
    pipeline = load_pipeline(args.model, token, args.device)
    speaker_options = dict(num_speakers=args.num_speakers, min_speakers=args.min_speakers,
                           max_speakers=args.max_speakers)

    turns = []
    if mode == "stereo":
        mic, system = audio[0], audio[1]
        for start, end in mic_dominance_intervals(mic, system, ratio_db=args.mic_ratio_db,
                                                  floor_db=args.mic_floor_db):
            turns.append(make_turn(start, end, MIC_SPEAKER, MIC_CHANNEL))
        print(f"Mic channel: {len(turns)} turns", file=sys.stderr)
        hook = progress.pipeline_hook(10, 95, "System audio")
        for start, end, label in diarize(pipeline, system, hook=hook, **speaker_options):
            turns.append(make_turn(start, end, label, SYSTEM_CHANNEL))
    else:
        hook = progress.pipeline_hook(10, 95, "Audio")
        for start, end, label in diarize(pipeline, audio[0], hook=hook, **speaker_options):
            turns.append(make_turn(start, end, label))

    data = build_output(turns, audio_path, mode, args.model)
    output_path = Path(args.output) if args.output else audio_path.parent / OUTPUT_NAME
    write_output(data, output_path)
    print(f"Wrote {len(turns)} turns, speakers {data['speakers']} to {output_path}", file=sys.stderr)
    progress.report(100, "Done")
    return 0


if __name__ == "__main__":
    sys.exit(main())
