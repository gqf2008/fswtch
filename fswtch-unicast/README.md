# fswtch_unicast

FreeSWITCH endpoint module that bridges call media over a single raw-PCM UDP
socket — the single-channel UDP-media analogue of FreeSWITCH's built-in
`mod_unicast`.

## What it does

- Registers FreeSWITCH endpoint `fswtch_unicast`.
- When a call is bridged to `fswtch_unicast/<ip>:<port>`, the module creates a
  B-leg and opens a per-call UDP socket bound to a dynamic local port.
- Caller audio is sent as raw little-endian i16 PCM to `<ip>:<port>`.
- Raw PCM received from `<ip>:<port>` is played back toward the caller.
- One UDP socket per call, raw PCM in both directions. No framing, no
  signalling — the module is purely a media bridge.

## Build

```bash
cargo build -p fswtch-unicast --release
```

The output is a shared object at:

```
target/release/libfswtch_unicast.so
```

Copy or symlink it into your FreeSWITCH modules directory. The installed
basename (the `.so` filename **minus** its extension) **must** match the
module's exported interface symbol `fswtch_unicast_module_interface` — i.e.
install it as `fswtch_unicast.so`, **not** `mod_fswtch_unicast.so`.
FreeSWITCH's loader derives the `dlsym` lookup name as `<basename>_module_interface`
from the installed filename (it does not strip a `mod_` prefix), so a mismatched
name fails to load with no symbol found. The `cargo`/`[lib]` name and the
`module_exports! { module = … }` ident are both `fswtch_unicast` to match.

```bash
sudo cp target/release/libfswtch_unicast.so /usr/lib/freeswitch/mod/fswtch_unicast.so
```

## FreeSWITCH configuration

### Module autoload

Add to `autoload_configs/modules.conf.xml`:

```xml
<load module="fswtch_unicast"/>
```

### Dialplan example

```xml
<extension name="fswtch_unicast_demo">
  <condition field="destination_number" expression="^(10\d{2})$">
    <action application="bridge" data="fswtch_unicast/127.0.0.1:5000"/>
  </condition>
</extension>
```

Or bridge programmatically:

```
sendmsg
execute
bridge
fswtch_unicast/127.0.0.1:5000
```

## Peer contract

The peer at `<ip>:<port>` must:

1. Listen on **UDP** port `P` for raw PCM.
2. Optionally send raw little-endian i16 PCM back toward the caller.
3. Exchange raw little-endian i16 PCM at 8 kHz, 20 ms frames (160 samples per
   frame) over the UDP socket.

The module binds a dynamic local UDP source port. The peer can learn it by
observing the source address of incoming UDP packets. It is also logged by
FreeSWITCH (`outgoing_channel: created session <uuid> remote=<addr>`).

## Payload format

- No headers, no framing.
- Samples are 16-bit signed little-endian (`i16`).
- Default sample rate: 8 kHz.
- Default packetization: 20 ms mono (160 samples / 320 bytes per frame).

## Verification

1. Build and install the module.
2. Start FreeSWITCH and confirm `fswtch_unicast` loaded:
   ```
   fs_cli -x "show modules" | grep fswtch_unicast
   ```
3. Run a Python UDP echo server:
   ```python
   import socket
   s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
   s.bind(("0.0.0.0", 5000))
   while True:
       data, addr = s.recvfrom(2048)
       s.sendto(data, addr)  # loopback
   ```
4. Place a call bridged to `fswtch_unicast/127.0.0.1:5000`.
5. You should hear your own audio looped back.

## License

MIT — same as the `fswtch` workspace.
