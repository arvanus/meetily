# Speaker diarization for Meetily

This folder has a small script that runs
[pyannote](https://github.com/pyannote/pyannote-audio) on a meeting recording
and writes a `diarization.json` file. Meetily merges it into the existing
transcript: the text stays the same, and each segment gets a speaker label.

The app can run the script for you (**Speakers** button), using a local Python
environment you set up once. You can also run it by hand and import the file.

## How it works

Works best with **Separate audio tracks** turned on (Settings > Recordings),
where the microphone goes to the left channel and system audio to the right:

- **Right channel (system audio):** diarized with pyannote. Remote participants
  show up as `Speaker 1`, `Speaker 2`, ...
- **Left channel (microphone):** labelled `Me` where the mic is clearly louder
  than the system channel. No model involved, just an energy check.

Mono recordings are diarized as a whole: everyone, you included, shows up as
`Speaker N`.

For the cleanest result on stereo recordings, retranscribe the meeting first
(Enhance button). Retranscription transcribes each channel on its own, so
microphone segments are already marked as you, and diarization only has to
split the system segments.

## Setup

Requirements: Python 3.10+, `ffmpeg` on PATH (or pass `--ffmpeg`), and a
[Hugging Face](https://huggingface.co) account.

1. Accept the user conditions of
   [pyannote/speaker-diarization-community-1](https://huggingface.co/pyannote/speaker-diarization-community-1)
   and create an access token (Settings > Access Tokens, read access is enough).
2. Create a virtual environment in `~/.venvs/meetily-diarize` (the app looks
   for it there) and install PyTorch **before** the other requirements. For an
   NVIDIA GPU pick the CUDA build from
   [pytorch.org](https://pytorch.org/get-started/locally/), for example:

   ```powershell
   # Windows (PowerShell)
   python -m venv $env:USERPROFILE\.venvs\meetily-diarize
   $py = "$env:USERPROFILE\.venvs\meetily-diarize\Scripts\python.exe"
   & $py -m pip install torch torchaudio --index-url https://download.pytorch.org/whl/cu128
   & $py -m pip install -r requirements.txt
   ```

   ```bash
   # macOS / Linux
   python3 -m venv ~/.venvs/meetily-diarize
   ~/.venvs/meetily-diarize/bin/pip install torch torchaudio
   ~/.venvs/meetily-diarize/bin/pip install -r requirements.txt
   ```

   CPU works too, it is just slower. To use another Python, set
   `MEETILY_DIARIZE_PYTHON` to its executable.
3. Set `HF_TOKEN` as a user environment variable (Windows: "Edit environment
   variables for your account"; macOS / Linux: your shell profile), then
   restart Meetily so it sees it. The model is downloaded once and cached;
   after that it runs offline.

## Usage

### From the app

Open the meeting, click **Speakers** in the transcript toolbar, optionally
enter the number of speakers (exact, or "At most"), and click **Run
Diarization**. The app runs the
script (embedded in the app, no checkout needed), shows progress and imports
the result when it finishes. `diarization.json` is kept in the meeting folder.

The button shows up when the "Import Audio & Retranscribe" beta feature is on
(it is by default).

### From the CLI

```bash
meetily-cli diarize --last --num-speakers 4      # or --meeting <id>, --max-speakers 6
meetily-cli diarize --meeting <id> --file diarization.json   # import only
```

### By hand

1. In Meetily, open the meeting and click **Recording** to open its folder.
2. Run the script on that folder:

   ```bash
   python diarize.py "C:\Users\me\Documents\meetily-recordings\Team Sync 2026-10-17"
   ```

   It writes `diarization.json` next to the audio file.
3. Back in Meetily, click **Speakers**, then **Import File...** and pick the
   `diarization.json` file.

Running the import again with a new file is fine: segments are relabelled.
Segments already marked as you (`Me`) are kept when the file has channel
information (a stereo `diarization.json` from this script). An RTTM file or a
mono `diarization.json` relabels every segment.

### Options

| Option | Default | Description |
|---|---|---|
| `--mode auto\|stereo\|mono` | `auto` | `auto` picks stereo for 2 channel files |
| `--num-speakers N` | | Exact number of speakers (system channel only in stereo mode) |
| `--min-speakers N` / `--max-speakers N` | | Bounds on the number of speakers |
| `--device auto\|cpu\|cuda` | `auto` | Where pyannote runs |
| `--mic-ratio-db DB` | `6` | How much louder the mic must be than the system channel to count as you |
| `--mic-floor-db DB` | `-45` | Minimum mic level (dBFS) to count as speech |
| `--model NAME` | `pyannote/speaker-diarization-community-1` | Any pyannote pipeline (3.1 also works) |
| `--ffmpeg PATH` | `ffmpeg` | ffmpeg executable |
| `-o FILE` | `diarization.json` next to the audio | Output file |
| `--progress` | | Print `PROGRESS <percent> <message>` lines (used by the app) |

Knowing the number of speakers (`--num-speakers`) helps accuracy a lot. Without
it, pyannote tends to split one person into several speakers on long meetings.

## Other diarization tools

The import also accepts standard **RTTM** files, so any tool that writes RTTM
works (WhisperX, NeMo, sherpa-onnx, pyannote directly). RTTM has no channel
information, so it is treated like a mono recording.

`diarization.json` format, if you want to write it from another tool:

```json
{
  "version": 1,
  "turns": [
    { "start": 0.52, "end": 3.10, "speaker": "mic", "channel": "mic" },
    { "start": 3.40, "end": 9.87, "speaker": "SPEAKER_00", "channel": "system" }
  ]
}
```

Times are in seconds from the start of the audio file. `channel` is optional
(`mic` or `system`); leave it out for mono audio.

## Limitations

- **Headphones help.** Without them, remote voices leak into the microphone.
  The `Me` heuristic only counts frames where the mic is much louder than the
  system channel, so leakage usually stays unlabelled instead of mislabelled.
  Raise `--mic-ratio-db` if remote speech still gets marked as you.
- **Labels are per segment.** A transcript segment that covers a change of
  speaker gets the speaker who talks the most in it. Segments are not split.
- **Timing.** Diarization times are relative to the audio file and transcript
  times to the start of the recording. They match for retranscribed and
  imported meetings. For live transcripts they should also match; if labels
  look shifted, retranscribe the meeting and import again.
- Speaker labels are saved in the database only; `transcripts.json` in the
  meeting folder is not updated.

## Tests

The helpers (audio probing, energy heuristic, output) have unit tests that only
need numpy:

```bash
python -m unittest test_diarize.py
```
