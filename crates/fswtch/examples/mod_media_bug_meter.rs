use std::sync::atomic::{AtomicUsize, Ordering};

use fswtch::{
    MediaBugAction, MediaBugConfig, MediaBugContext, MediaBugFlags, MediaBugHandler, MediaFrame,
};

static COUNTERS: Counters = Counters::new();

fswtch::module_exports! {
    module = mod_media_bug_meter,
    load = switch_module_load,
}

const METER_APP: fswtch::ApplicationInfo = fswtch::ApplicationInfo::new(
    "fswtch_media_bug_meter",
    "Attaches a read/write-stream media bug and counts observed audio frames",
    "Rust media bug meter example",
    "fswtch_media_bug_meter",
);

struct Counters {
    bugs_attached: AtomicUsize,
    bugs_closed: AtomicUsize,
    read_frames_seen: AtomicUsize,
    write_frames_seen: AtomicUsize,
    read_audio_bytes_seen: AtomicUsize,
    write_audio_bytes_seen: AtomicUsize,
}

impl Counters {
    const fn new() -> Self {
        Self {
            bugs_attached: AtomicUsize::new(0),
            bugs_closed: AtomicUsize::new(0),
            read_frames_seen: AtomicUsize::new(0),
            write_frames_seen: AtomicUsize::new(0),
            read_audio_bytes_seen: AtomicUsize::new(0),
            write_audio_bytes_seen: AtomicUsize::new(0),
        }
    }
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
        COUNTERS.read_frames_seen.fetch_add(1, Ordering::Relaxed);
        COUNTERS
            .read_audio_bytes_seen
            .fetch_add(bytes, Ordering::Relaxed);
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
        COUNTERS.write_frames_seen.fetch_add(1, Ordering::Relaxed);
        COUNTERS
            .write_audio_bytes_seen
            .fetch_add(bytes, Ordering::Relaxed);
        MediaBugAction::Continue
    }

    fn on_close(&mut self, _ctx: &mut MediaBugContext<'_>) {
        fswtch::log_info("mod_media_bug_meter", "media bug closing");
        COUNTERS.bugs_closed.fetch_add(1, Ordering::Relaxed);
    }
}

fswtch::app_callback! {
    fn meter_app(session, _data) {
        fswtch::log_info("mod_media_bug_meter", "dialplan application invoked");
        let Some(session) = session else {
            fswtch::log_info("mod_media_bug_meter", "missing session");
            return;
        };

        let config = match MediaBugConfig::new(
            "fswtch_media_bug_meter",
            "read-write-stream",
            MediaBugFlags::READ_STREAM | MediaBugFlags::WRITE_STREAM | MediaBugFlags::NO_PAUSE,
        ) {
            Ok(config) => config,
            Err(error) => {
                fswtch::log_error(
                    "mod_media_bug_meter",
                    format!("invalid media bug config: {error}"),
                );
                return;
            }
        };
        let handler = MeterState {
            read_frames: 0,
            write_frames: 0,
            read_audio_bytes: 0,
            write_audio_bytes: 0,
        };

        match fswtch::attach_media_bug(session, config, handler) {
            Ok(_) => {
                COUNTERS.bugs_attached.fetch_add(1, Ordering::Relaxed);
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
        fswtch::log_info("mod_media_bug_meter", "fswtch_media_bug_meter_stats invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        stream.write(
            &format!(
                "attached={} closed={} read_frames={} write_frames={} read_audio_bytes={} write_audio_bytes={}\n",
                COUNTERS.bugs_attached.load(Ordering::Relaxed),
                COUNTERS.bugs_closed.load(Ordering::Relaxed),
                COUNTERS.read_frames_seen.load(Ordering::Relaxed),
                COUNTERS.write_frames_seen.load(Ordering::Relaxed),
                COUNTERS.read_audio_bytes_seen.load(Ordering::Relaxed),
                COUNTERS.write_audio_bytes_seen.load(Ordering::Relaxed)
            ),
        )
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_media_bug_meter" {
        fswtch::log_info("mod_media_bug_meter", "loading module");
        module
            .application(METER_APP, meter_app)
            .and_then(|module| {
                module.api(
                    "fswtch_media_bug_meter_stats",
                    "prints media bug meter counters",
                    "fswtch_media_bug_meter_stats",
                    stats_api,
                )
            })
    }
}
