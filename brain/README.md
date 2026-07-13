# fswtch VAD-module Python brain

External **business brain** for the `mod_vad_bot` fswtch VAD module. Re-implements
ai-agent-seat's role (the brain / ASR-LLM-TTS orchestration) as a plain Python program
over ESL — fully decoupled from the voice media path.

## Architecture

The brain is an **outbound ESL** TCP server: FreeSWITCH connects to it per-call via
the dialplan `socket` application. Each call gets its own connection + handler thread
(no global state, no auto-reconnect).

```
┌──────────────────────────────────┐         ┌──────────────────────────────┐
│ FreeSWITCH  (mod_vad_bot .so)   │  per-call│  Python brain (TCP server)    │
│  dialplan: socket host:port full │ ───────► │  accept() → handle_call()     │
│  A-leg parks; brain bridges to   │  ESL     │  park + bridge fswtch_vad_bot │
│  fswtch_vad_bot/1000 (B-leg)     │ ◄──────► │  recv fswtch::vad/uplink_pcm  │
│  VAD local + media (read/write)  │  events  │  ASR / LLM / TTS (Pipeline)   │
└──────────────────────────────────┘         └──────────────────────────────┘
```

- **VAD module** (`crates/fswtch/examples/mod_vad_bot.rs`, an FS endpoint): VAD runs
  locally in `write_frame`; the bot is the call terminus. It ferries audio + VAD state
  as ESL events, and plays TTS back. Reusable, brain-agnostic.
- **Python brain** (this package): listens for outbound ESL connections, parks the call,
  bridges to `fswtch_vad_bot/1000`, subscribes to VAD/uplink events, buffers utterances,
  runs the business pipeline, sends TTS back. Brain-agnostic module ↔ any brain.

## Event protocol

| event (ESL CUSTOM subclass) | dir | headers | body |
|---|---|---|---|
| `fswtch::vad` | VAD→brain | `Call-UUID` `Vad-State`(start-talking\|stop-talking) `Seq` | — |
| `fswtch::uplink_pcm` | VAD→brain | `Call-UUID` `Seq` `Sample-Rate` `Channels`(1) `Bits-Per-Sample`(16) `Sample-Format`(S16LE) `Samples` | base64 S16LE PCM (whole segment, mono) |
| `fswtch::downlink_pcm` | brain→VAD | `Target-UUID` `Sample-Rate` `Channels` `Bits-Per-Sample` `Sample-Format` | base64 S16LE TTS PCM |

`vad` fires only on speech boundaries. `uplink_pcm` fires **once per utterance** (on
`stop-talking`) — the whole snapped segment (pre-roll onset recovery + trailing-silence
trim via `snap_segments`), mono. The VAD module also flushes its play queue on
`start-talking` (barge-in: caller's speech stops any playing TTS).

## Run

```sh
# 1. load the VAD module in FreeSWITCH:
fs_cli -x 'load mod_vad_bot'

# 2. dialplan: export APM switches, then socket to the brain.
#    (the fswtch_vad_bot_test extension on 7782 already does this)
#
#    <action application="export" data="FSWTCH_NS=12"/>
#    <action application="export" data="FSWTCH_AGC2=6"/>
#    <action application="socket" data="127.0.0.1:8084 full"/>

# 3. start the brain (from the repo root):
python3 -m brain.brain
#   → listens on 127.0.0.1:8084 for outbound ESL connections.

# 4. dial 7782; FS connects to the brain, which parks + bridges to
#    fswtch_vad_bot/1000. Speak; the brain beeps back (StubPipeline).
#    Barge-in: speak again mid-beep → the VAD module flushes + the brain
#    cancels the in-flight reply.
```

`--host / --port` override the listen address.

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
