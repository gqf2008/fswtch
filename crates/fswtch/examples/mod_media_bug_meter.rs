use std::sync::atomic::{AtomicUsize, Ordering};

use fswtch::{
    MediaBugAction, MediaBugConfig, MediaBugContext, MediaBugFlags, MediaBugHandler, MediaFrame,
};

static BUGS_ATTACHED: AtomicUsize = AtomicUsize::new(0);
static BUGS_CLOSED: AtomicUsize = AtomicUsize::new(0);
static READ_FRAMES_SEEN: AtomicUsize = AtomicUsize::new(0);
static WRITE_FRAMES_SEEN: AtomicUsize = AtomicUsize::new(0);
static READ_AUDIO_BYTES_SEEN: AtomicUsize = AtomicUsize::new(0);
static WRITE_AUDIO_BYTES_SEEN: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_media_bug_meter,
    load = switch_module_load,
}

#[derive(Debug)]
struct MeterState {
    read_frames: usize,
    write_frames: usize,
    read_audio_bytes: usize,
    write_audio_bytes: usize,
}

impl MediaBugHandler for MeterState {
    fn on_read(&mut self, _ctx: &mut MediaBugContext<'_>, frame: MediaFrame<'_>) -> MediaBugAction {
        let bytes = frame.data_len();
        self.read_frames += 1;
        self.read_audio_bytes += bytes;
        READ_FRAMES_SEEN.fetch_add(1, Ordering::Relaxed);
        READ_AUDIO_BYTES_SEEN.fetch_add(bytes, Ordering::Relaxed);
        MediaBugAction::Continue
    }

    fn on_write(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        frame: MediaFrame<'_>,
    ) -> MediaBugAction {
        let bytes = frame.data_len();
        self.write_frames += 1;
        self.write_audio_bytes += bytes;
        WRITE_FRAMES_SEEN.fetch_add(1, Ordering::Relaxed);
        WRITE_AUDIO_BYTES_SEEN.fetch_add(bytes, Ordering::Relaxed);
        MediaBugAction::Continue
    }

    fn on_close(&mut self, _ctx: &mut MediaBugContext<'_>) {
        fswtch::log_info("mod_media_bug_meter", "media bug closing");
        BUGS_CLOSED.fetch_add(1, Ordering::Relaxed);
    }
}

fswtch::app_callback! {
    fn meter_app(session, _data) {
        fswtch::log_info("mod_media_bug_meter", "dialplan application invoked");
        let Some(session) = session else {
            fswtch::log_info("mod_media_bug_meter", "missing session");
            return;
        };

        let config = MediaBugConfig::new(
            c"rust_media_bug_meter",
            c"read-write-stream",
            MediaBugFlags::READ_STREAM | MediaBugFlags::WRITE_STREAM | MediaBugFlags::NO_PAUSE,
        );
        let handler = MeterState {
            read_frames: 0,
            write_frames: 0,
            read_audio_bytes: 0,
            write_audio_bytes: 0,
        };

        match fswtch::attach_media_bug(session.as_ptr(), config, handler) {
            Ok(_) => {
                BUGS_ATTACHED.fetch_add(1, Ordering::Relaxed);
                fswtch::log_info("mod_media_bug_meter", "media bug attached");
            }
            Err(error) => fswtch::log_error(
                "mod_media_bug_meter",
                format!("failed to attach media bug: {error}"),
            ),
        }
    }
}

fswtch::api_callback! {
    fn stats_api(_cmd, _session, stream) {
        fswtch::log_info("mod_media_bug_meter", "rust_media_bug_meter_stats invoked");
        stream.write(
            &format!(
                "attached={} closed={} read_frames={} write_frames={} read_audio_bytes={} write_audio_bytes={}\n",
                BUGS_ATTACHED.load(Ordering::Relaxed),
                BUGS_CLOSED.load(Ordering::Relaxed),
                READ_FRAMES_SEEN.load(Ordering::Relaxed),
                WRITE_FRAMES_SEEN.load(Ordering::Relaxed),
                READ_AUDIO_BYTES_SEEN.load(Ordering::Relaxed),
                WRITE_AUDIO_BYTES_SEEN.load(Ordering::Relaxed)
            ),
        )
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for c"mod_media_bug_meter" {
        fswtch::log_info("mod_media_bug_meter", "loading module");
        module
            .application(
                c"rust_media_bug_meter",
                c"Attaches a read/write-stream media bug and counts observed audio frames",
                c"Rust media bug meter example",
                c"rust_media_bug_meter",
                meter_app,
            )
            .and_then(|module| {
                module.api(
                    c"rust_media_bug_meter_stats",
                    c"prints media bug meter counters",
                    c"rust_media_bug_meter_stats",
                    stats_api,
                )
            })
    }
}
