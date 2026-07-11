//! Showcase: `fswtch::Buffer` — the FIFO byte buffer wrapping FreeSWITCH's `switch_buffer_t`.
//!
//! Creates a 1 KiB dynamic buffer, writes a few payloads in, peeks the head without
//! consuming, then reads the data back out and reports the round-trip plus the
//! `inuse` / `len` / `freespace` accounting to the stream.
//!
//! Load the module (`mod_buffer_demo`) and from `fs_cli` run:
//!   - `fswtch_buffer_demo`        — full peek + read round-trip with accounting
//!   - `fswtch_buffer_toss`        — writes data, tosses the head, reads the remainder

fswtch::module_exports! {
    module = mod_buffer_demo,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn buffer_demo_api(_cmd, _session, stream) {
        fswtch::log_info("mod_buffer_demo", "fswtch_buffer_demo invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };

        // Build a 1 KiB dynamic FIFO buffer and push two payloads into the tail.
        let buffer = match fswtch::Buffer::new(1024) {
            Ok(buffer) => buffer,
            Err(error) => {
                return stream.write(&format!("buffer create failed: {error}\n"));
            }
        };

        if let Err(error) = buffer.write(b"hello, ") {
            return stream.write(&format!("buffer write failed: {error}\n"));
        }
        if let Err(error) = buffer.write(b"buffer!") {
            return stream.write(&format!("buffer write failed: {error}\n"));
        }

        // Peek the first 5 bytes without removing them — should read "hello".
        let mut peek_buf = [0u8; 5];
        let peeked = buffer.peek(&mut peek_buf);

        // Read every remaining byte out of the head.
        let mut read_buf = [0u8; 32];
        let read = buffer.read(&mut read_buf);

        stream.write(&format!(
            "wrote='hello, buffer!' peek({})='{}' read({})='{}' inuse={} len={} freespace={}\n",
            peeked,
            std::str::from_utf8(&peek_buf[..peeked]).unwrap_or("<bad utf8>"),
            read,
            std::str::from_utf8(&read_buf[..read]).unwrap_or("<bad utf8>"),
            buffer.inuse(),
            buffer.len(),
            buffer.freespace(),
        ))
    }
}

fswtch::api_callback! {
    fn buffer_toss_api(_cmd, _session, stream) {
        fswtch::log_info("mod_buffer_demo", "fswtch_buffer_toss invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };

        let buffer = match fswtch::Buffer::new(1024) {
            Ok(buffer) => buffer,
            Err(error) => {
                return stream.write(&format!("buffer create failed: {error}\n"));
            }
        };

        if let Err(error) = buffer.write(b"abcdef") {
            return stream.write(&format!("buffer write failed: {error}\n"));
        }

        // Discard the first 2 bytes from the head, then read the remainder.
        buffer.toss(2);

        let mut out = [0u8; 8];
        let read = buffer.read(&mut out);

        stream.write(&format!(
            "wrote='abcdef' toss(2) read({})='{}' inuse={} empty={}\n",
            read,
            std::str::from_utf8(&out[..read]).unwrap_or("<bad utf8>"),
            buffer.inuse(),
            buffer.is_empty(),
        ))
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_buffer_demo" {
        fswtch::log_info("mod_buffer_demo", "loading module");
        module
            .api(
                "fswtch_buffer_demo",
                "writes a FIFO buffer, peeks, and reads it back with accounting",
                "fswtch_buffer_demo",
                buffer_demo_api,
            )
            .and_then(|module| {
                module.api(
                    "fswtch_buffer_toss",
                    "writes a buffer, tosses the head, and reads the remainder",
                    "fswtch_buffer_toss",
                    buffer_toss_api,
                )
            })
    }
}
