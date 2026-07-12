# fswtch VAD-module Python brain

External **business brain** for the `mod_vad_bot` fswtch VAD module. Re-implements
ai-agent-seat's role (the brain / ASR-LLM-TTS orchestration) as a plain Python program
over ESL — fully decoupled from the voice media path.

## Architecture

```
┌──────────────────────────────────┐         ┌──────────────────────────────┐
│ FreeSWITCH  (mod_vad_bot .so)   │   ESL   │  Python brain (brain.py)     │
│  endpoint: bot IS the call leg  │ ◄─────► │  subscribe + send events     │
│  VAD local + media (read/write) │  events │  ASR / LLM / TTS (Pipeline)   │
└──────────────────────────────────┘         └──────────────────────────────┘
```

- **VAD module** (`crates/fswtch/examples/mod_vad_bot.rs`, an FS endpoint): VAD runs
  locally in `write_frame`; the bot is the call terminus. It ferries audio + VAD state
  as ESL events, and plays TTS back. Reusable, brain-agnostic.
- **Python brain** (this package): subscribes to VAD/uplink events, buffers utterances,
  runs the business pipeline, sends TTS back. Brain-agnostic module ↔ any brain.

## Event protocol

| event (ESL CUSTOM subclass) | dir | headers | body |
|---|---|---|---|
| `fswtch::vad` | VAD→brain | `Call-UUID` `Vad-State`(start-talking\|stop-talking) `Seq` | — |
| `fswtch::uplink_pcm` | VAD→brain | `Call-UUID` `Seq` `Sample-Rate` `Channels` `Bits-Per-Sample`(16) `Sample-Format`(S16LE) `Samples` | base64 S16LE PCM |
| `fswtch::downlink_pcm` | brain→VAD | `Target-UUID` `Sample-Rate` `Channels` `Bits-Per-Sample` `Sample-Format` | base64 S16LE TTS PCM |

`vad` fires only on speech boundaries; `uplink_pcm` fires on every talking-active frame.
On a start/stop frame both fire with the **same `Seq`**. The VAD module also flushes its
play queue on `start-talking` (barge-in: caller's speech stops any playing TTS).

## Run

```sh
# 1. load the VAD module in FreeSWITCH (no mimalloc → runtime load is safe):
fs_cli -x 'load mod_vad_bot'

# 2. dialplan bridges the caller to the bot:
#    <action application="bridge" data="fswtch_vad_bot/1000"/>
#    (the fswtch_vad_bot_test extension on 7782 already does this)

# 3. start the brain (from the repo root):
python3 -m brain.brain
#   → connects to FS ESL (127.0.0.1:8022, password ClueCon by default),
#     subscribes to fswtch::vad + fswtch::uplink_pcm, and is ready.

# 4. dial 7782, speak; the brain beeps back (StubPipeline). Barge-in: speak again
#    mid-beep → the VAD module flushes + the brain cancels the in-flight reply.
```

`--host / --port / --password` override the ESL endpoint.

## Plugging a real brain (ASR/LLM/TTS)

The default `StubPipeline` (in `pipeline.py`) just beeps — enough to prove the ESL
plumbing. To re-implement ai-agent-seat's real business, implement the `Pipeline`
interface:

```python
class MyPipeline(Pipeline):
    def process(self, pcm_i16: bytes, sample_rate: int, channels: int) -> bytes:
        # pcm_i16 = raw S16LE of the whole utterance. Return raw S16LE TTS PCM
        # at the same rate/channels.
        ...
```

- **audio_llm** (matches the Rust bot, `pipeline_mode=audio_llm`): POST the utterance
  PCM to the Doubao Responses API (`llm_base_url`/`llm_key`/`llm_model` from
  `ai-agent-seat.conf.xml`), parse the `speak(text)` tool call, then synthesize via
  Volcano TTS (`volcano_*` keys — a bidirectional websocket; port
  `tts_ws_codec.rs` + `providers/volcano_tts.rs`).
- **asr_llm_tts** (simpler): whisper ASR → text Doubao chat/completions → Volcano TTS.

Wire it in `brain.py`'s `main()` behind `--pipeline`.
