# Clipy Local

Record your screen and talk. Clipy Local turns the recording into an AREC file
([AREC](https://arecformat.org/spec) is an open, plain-text format for
recordings that AI agents read) on your own machine: the transcript, the moments that matter and a screenshot
of each, ready for an AI agent like Claude Code, Codex or Cursor to read. No
account. With no model provider and an on-device transcription model, nothing
leaves your computer.

This repository is the open-source local edition of
[Clipy](https://clipy.online), the AI-native screen recorder. It holds the
packages that build an AREC on your machine and Clipy Local, the desktop app
for macOS and Windows. Want a share link anyone can watch, AI with no keys to
manage, search across all your recordings and team workspaces? That is the
full Clipy app, a separate download at
[clipy.online/download](https://clipy.online/download).

## Use it

The command line is the official Clipy CLI with `--local`. It bundles the
packages in this repository, so there is nothing else to install.

```bash
npm i -g @clipy/cli
clipy models download english-fast          # on-device speech, one-time download
clipy speech install                        # the engine that runs it (about 35 MB)
clipy record --local --max 120              # press Enter or Ctrl-C to stop
clipy context import demo.mov --local       # transcribes on this machine
clipy local status
```

To let a model pick spoken moments too, give it a provider and a key through
the environment (or save them with `clipy local setup --model-provider ...`):

```bash
export CLIPY_MODEL_PROVIDER=openrouter
export CLIPY_MODEL_API_KEY="<your key>"
clipy record --local
```

The last line each command prints is the path of `recording.arec`. Give that
path to your agent. `--local` also works on `clipy session start`, `clipy proof`
and `clipy record --url`.

## What is here

| Package | What it does |
|---|---|
| `packages/moments` | Picks the moments worth a screenshot. Rules on your machine find highlights, drawings, clicks and pauses; with a model key it also finds what you pointed at while talking ("this button", "it breaks here"). |
| `packages/config` | Settings: which model provider picks spoken moments and how recordings are transcribed, from a config file and env vars. |
| `packages/capture` | Records the screen with ffmpeg on macOS, Windows and Linux, with a pointer and click sidecar where the OS allows it. |
| `packages/transcribe` | Turns speech into timestamped segments: downloadable on-device models, or your own key for OpenAI, Groq, Deepgram, AssemblyAI, ElevenLabs or Gemini. |

## Clipy Local, the desktop app

`desktop/` is Clipy Local: a menu-bar app for macOS and a tray app for Windows
that records your screen, camera and voice and builds the same AREC with no
account. You build it yourself from this repository; the steps are in
[desktop/README.md](desktop/README.md).

## Drive it from an agent

On macOS, an agent can record through Clipy Local with the official CLI. Keep
Clipy Local running, then:

```bash
clipy sources --local --source mac-screen                  # displays and windows Clipy Local can see
clipy record --local --source mac-screen --for 60          # one take, up to 60 seconds
clipy record --local --source mac-screen --window "Safari" --for 30
```

For a take the agent starts, drives and stops itself:

```bash
clipy session start --local --source mac-screen
clipy mark "Opened the settings page"
clipy session stop --local                                 # waits for Clipy Local to finish
```

Each command ends by printing the path of the take's `recording.arec`, which
the agent then reads. `--json` prints the result as JSON instead. Clipy Local
keeps each take in its own folder, and nothing is uploaded. The first take an
agent starts asks for your consent in Clipy Local.

## Where your data goes

Moment selection works with no key at all. To add spoken cues, point it at any
OpenAI-compatible endpoint: OpenRouter, OpenAI, Google Gemini, Groq, or a model
running on your machine with Ollama or LM Studio.

| Choice | What leaves your machine |
|---|---|
| No model provider and on-device transcription | Nothing. |
| Ollama or LM Studio | Nothing: the model runs on your machine. |
| A model provider (OpenRouter, OpenAI, Gemini, Groq or a custom URL) | The transcript text, the labels of what you clicked, and a bounded sample of pointer and key events go to that provider, under its terms. Video and screenshots do not. With OpenRouter, requests also name the app (`HTTP-Referer: https://clipy.online`, `X-Title: Clipy`), as OpenRouter asks apps to. |
| On-device transcription | Nothing: the audio stays on your machine. |
| A transcription key (OpenAI, Groq, Deepgram, AssemblyAI, ElevenLabs or Gemini) | The audio goes to that provider, under its terms. |

## Contribute

Build, test and pull request steps are in [CONTRIBUTING.md](CONTRIBUTING.md).
Issues labelled `good first issue` are a good place to start. Report security
problems privately, as [SECURITY.md](SECURITY.md) describes.

```bash
pnpm install
pnpm test
```

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

The speech models are downloaded separately, only when you ask for them, and
keep their own licenses: "english-fast" is NVIDIA Parakeet TDT-CTC 110M
(CC-BY-4.0) and "multilingual" is OpenAI Whisper base (MIT), both as ONNX
exports from the sherpa-onnx project (Apache-2.0). The sherpa-onnx engine is
downloaded the same way, the first time on-device transcription needs it.
