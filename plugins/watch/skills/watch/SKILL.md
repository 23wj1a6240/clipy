---
name: watch
description: Watch a YouTube video, a Loom link or a video file for the user and answer questions about it with timestamps. Use whenever the user shares a YouTube URL, a Loom share link or a path to a video file, or asks you to watch, summarize, take notes on or follow along with a video ("watch this", "what does this video say", "implement what this video shows").
argument-hint: "<YouTube URL | Loom link | path to a video file> [question]"
tags: [video, youtube, loom, transcript, arec, claude-code]
allowed-tools: Read, Bash(command -v clipy), Bash(clipy context import *), Bash(clipy context read *), Bash(clipy local status*), Bash(clipy doctor*), Bash(npx -y @clipy/cli@latest context import *), Bash(npx -y @clipy/cli@latest context read *), Bash(npx -y @clipy/cli@latest local status*), Bash(npx -y @clipy/cli@latest doctor*)
---

# Watch a video

The user wants you to watch a video and then answer their question about it.
Their input: `$ARGUMENTS`

If that is empty (you picked this skill up on your own), take the video and the
question from the user's latest message.

The first argument is the video: a YouTube URL, a Loom share link, or a path to
a video file on this machine. Everything after it is the question. If the path
has spaces, the user may have quoted it; treat the quoted part as the video. If
there is no argument at all, ask which video to watch and stop.

You watch the video through the Clipy CLI. It turns the video into an AREC
file, `recording.arec`: the transcript, the key moments and timestamps, in
plain Markdown, written to a folder on this machine. AREC is an open format
(https://arecformat.org). Then you read that file and answer.

## 1. Find the CLI

Run `command -v clipy`. If it prints a path, use `clipy` in the commands
below. If it prints nothing, use `npx -y @clipy/cli@latest` in place of
`clipy` (it needs Node.js 18.17 or later). Do not install anything globally
without asking; mention that `npm i -g @clipy/cli` makes later runs faster.

## 2. Import the video

### A YouTube URL or a Loom share link

```bash
clipy context import "<url>" --json
```

No Clipy account and no login. YouTube captions and Loom transcripts are
fetched on this machine, and nothing is sent to Clipy. Do not add `--sync`
unless the user asks to save the video to their Clipy library: `--sync` needs
a Clipy account and sends the transcript to Clipy.

Useful flags, only when they apply:

- `--language <code>` picks a caption language, for example `--language es`.
- `--transcript <file.vtt|file.srt|transcript.json>` uses captions the user
  already has.

### A video file on this machine

The file is transcribed on this machine by an on-device speech model. First
check that the model and the speech engine are installed:

```bash
clipy local status
```

Look at the "local models" and "local speech" lines. If the model is not
downloaded or the speech engine is not installed, tell the user this is a
one-time setup, say what it downloads, and **ask before running it**:

```bash
clipy models download english-fast   # on-device English speech model, about 104 MB
clipy speech install                 # the engine that runs it, about 35 MB
```

For a video that is not in English, offer `clipy models download multilingual`
(about 161 MB) followed by `clipy local setup --transcribe local-multilingual`.
Run these only after the user agrees. If they decline and have a caption file,
add `--transcript <file.vtt>` to the import instead.

Then import the file:

```bash
clipy context import "<path to the file>" --local --json
```

With `--local` the video, audio and screenshots stay on this machine and
nothing is uploaded to Clipy. It also picks the key moments and saves a
screenshot of each. (If the user has set their own model provider or
transcription key with `clipy local setup`, the transcript or audio goes to
that provider under its terms; the default uses neither.)

## 3. Read the result

With `--json` the command prints one JSON object.

- A URL import prints `contextPath`, the full path of `recording.arec`, and
  `bundlePath`, its folder.
- A `--local` file import prints `arec`, the full path of `recording.arec`,
  and `dir`, its folder, which also holds the screenshots under `frames/`.
- A failure prints `"ok": false` with `code`, `error` and `remediation`.

Read `recording.arec` with the Read tool. It is Markdown. When the question is
about something on screen and the AREC lists key moments with frames, read the
frame images it names. Check `warnings` in the JSON: a transcript gap or a
missing piece is reported there.

## 4. Answer

- If the user asked a question, answer it from the AREC and cite the moments
  that support it as timestamps, like `[03:12]`. Quote the speaker briefly
  when the exact words matter.
- If there was no question, summarize: one line on what the video is, the key
  points with timestamps, then any steps, decisions or action items.
- For a YouTube video, you may link a moment as
  `https://www.youtube.com/watch?v=<id>&t=<seconds>s`.
- Say so plainly when the transcript does not cover what was asked, for
  example something shown on screen but never said. Do not guess.
- End with the path of `recording.arec` so the user can reuse it.

## When something fails

Read `code` and `remediation` from the JSON and act on them:

| `code` | What to do |
|---|---|
| `no_captions` | The video has no usable captions. If the error lists other languages, re-run with `--language <code>`. Otherwise offer `--transcript <file.vtt>`, or, if the user has the video file, a `--local` import of it. |
| `invalid_url` | Only a single video works, not a channel, playlist, folder or search page. Ask for the video's own link. |
| `source_private` | The Loom video is private or password protected. Ask for a public share link. |
| `loom_unreachable` | Loom did not answer. Wait a moment and try once more. |
| `ytdlp_missing`, `ytdlp_download_403` | The YouTube helper is missing or out of date. Follow `remediation`, then run `clipy doctor`. |
| `ffmpeg_missing` | Install ffmpeg (`brew install ffmpeg` on macOS, `winget install Gyan.FFmpeg` on Windows, `sudo apt install ffmpeg` on Linux), then run `clipy doctor`. |
| `speech_engine_required` | Ask the user, then run `clipy speech install` and import again. |
| anything else | Run `clipy doctor` (or `clipy doctor --local` for a video file) and tell the user what it reports. |

Never retry the same failing command in a loop. Explain what happened in one
or two sentences and what the user can do next.
