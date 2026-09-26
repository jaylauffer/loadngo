# Phoneme recognition owned by loadngo: plan

Status, 2026-09-27: **plan only. Nothing here is built.** Kimi's `--voice` mode works
today on a stopgap: Apple's on-device recognizer, wrapped in `loadngo-speech`. Jay's
direction is that loadngo itself should interpret phonemes rather than rely on system
code. This document is the plan for that. It is meant to be read before any work
starts, by Jay, by Claude, or by Kimi once she can take engineering tasks.

## What we have

- **Microphone capture:** `loadngo-audio-io`, with its own CoreAudio, ALSA and WASAPI
  backends.
- **The stopgap recognizer:** `loadngo-speech` (Apple Speech, on-device only) and
  Kimi's `--voice`. It has the wake word ("Kimi, ..." within the first three words),
  spoken replies, and the microphone paused while she speaks. Verified live on the Mac
  mini 2026-09-27 through the C920 microphone.
- **Evidence that this is a Neural Engine job:** while Apple's recognizer transcribes,
  the Neural Engine draws 0.4-0.6 W, against 0 W at idle (IOReport, measured
  2026-09-27). Small, always-on speech models are what the Neural Engine is good at.
  Kimi's 48B model is not (`METAL_COMPUTE_PLAN.md`).
- **The engines to run a model:**
  - `loadngo-coreml` (Neural Engine, dense products, Core ML protobuf written from
    Rust);
  - `loadngo-metal-compute` (GPU);
  - `loadngo-weights` (safetensors, bf16);
  - the proactor for completions.

## What "loadngo interprets phonemes" means

Sound in, phonemes out (IPA symbols such as `k i m i`), computed by loadngo code:

1. **Front end (ours, no model):**
   - resample the microphone to 16 kHz mono;
   - normalise;
   - detect voice activity from energy and zero crossings, so nothing runs during
     silence.
2. **Acoustic model (our engine, open weights):** a wav2vec2-style network trained to
   output phonemes. Its architecture:
   - a 7-layer convolutional feature encoder over the raw waveform;
   - a feature projection;
   - a convolutional position embedding;
   - 24 transformer layers (1024 wide, 16 heads);
   - a linear head over about 390 phoneme symbols plus a CTC blank.
3. **Decoding (ours):** CTC. Take the best symbol per 20 ms frame, merge repeats and
   drop blanks. This gives the phoneme sequence.
4. **Phonemes to meaning (ours):** three uses, in increasing difficulty.
   - **Wake word:** match `/kimi/` and its variants in phoneme space. This is more
     robust than matching the text "Kimi", which today arrives as "Kimmy" or "Kimi".
   - **Words:** a pronunciation dictionary and a beam search over phonemes to words.
   - **An untested idea:** hand the phoneme string to Kimi and let her read it. Large
     language models can often read IPA; whether a 3B-active model does it well enough
     is a measurement, not an assumption.

We cannot own the trained weights: training a phoneme model needs thousands of hours
of labelled speech and serious GPU time. As with Kimi, the plan is our own engine
running open weights.

## Candidate weights (to confirm before any download)

| Model | Size | Output | Notes |
|---|---|---|---|
| `facebook/wav2vec2-lv-60-espeak-cv-ft` | ~315M parameters, ~1.26 GB fp32 | espeak IPA phonemes | English-heavy training; phoneme CTC head |
| `facebook/wav2vec2-xlsr-53-espeak-cv-ft` | ~300M parameters, ~1.2 GB fp32 | espeak IPA phonemes, multilingual (53 languages) | better if Mandarin matters |

The licence of each checkpoint must be read before it is downloaded or used. The
download goes to Jarraya, which has about 315 GiB free.

## Where it runs

- **Neural Engine, by default.** About 300M parameters, run in short bursts after voice
  activity. Weights baked into a compiled Core ML program (the fast path measured in
  `NPU_ACCELERATION.md`), built by `loadngo-coreml`.
- **GPU fallback** through `loadngo-metal-compute`.
- **CPU reference.** Every accelerated path is checked against it, as with Kimi.

## Milestones and gates

| | Work | Gate |
|---|---|---|
| P0 | CPU reference: waveform to phonemes, weights loaded with `loadngo-weights` | Known sentences (recorded, and generated with the system voice) decode to the expected phonemes; phoneme error rate reported |
| P1 | Front end and voice activity detection on live microphone audio | No model work while silent (measured); utterance boundaries match speech |
| P2 | Neural Engine program via `loadngo-coreml`; GPU fallback | Same phonemes as the CPU reference; latency and Neural Engine power measured |
| P3 | Wake word in phoneme space; words via dictionary; the Kimi-reads-IPA test | Wake-word hit and false-alarm rates on recorded samples; word error rate compared with the Apple stopgap |
| P4 | Kimi `--voice` uses loadngo's recognizer; the Apple backend goes | Live conversation on the Mac mini, as verified for the stopgap |

## Good first tasks for Kimi

Once she can edit files and run tests (`METAL_COMPUTE_PLAN.md`, "M1, second stage", and
the tools plan), these come with exact reference answers:

- resampling to 16 kHz;
- energy-based voice activity detection;
- CTC greedy decoding;
- phoneme-string matching for the wake word;
- drag-and-drop in `archive_cas_browser`, which Jay deferred on 2026-09-27 to give
  her a chance at it.

## Decisions for Jay

- **Which checkpoint:** English-heavy or multilingual (Mandarin).
- **The download:** about 1.2 GB onto Jarraya, once the licence is read.
- **Phonemes to words:** a pronunciation dictionary, Kimi reading phonemes, or both
  measured.
