use std::{
    collections::HashMap,
    ffi::{CStr, c_char},
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::Duration,
};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};

static JOB_QUEUE: LazyLock<JobQueue> = LazyLock::new(JobQueue::start);

fswtch::module_exports! {
    module = mod_async_job_queue,
    load = switch_module_load,
}

#[derive(Debug, Clone)]
struct Job {
    id: u64,
    payload: String,
}

#[derive(Debug, Clone)]
struct JobResult {
    status: &'static str,
    detail: String,
}

struct JobQueue {
    next_id: AtomicU64,
    sender: Sender<Job>,
    results: Mutex<HashMap<u64, JobResult>>,
}

impl JobQueue {
    fn start() -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("fswtch-async-job-queue".to_owned())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    thread::sleep(Duration::from_millis(25));
                    let result = JobResult {
                        status: "done",
                        detail: format!("processed {} bytes", job.payload.len()),
                    };
                    JOB_QUEUE
                        .results
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(job.id, result);
                }
            })
            .expect("failed to start async job queue worker");

        Self {
            next_id: AtomicU64::new(1),
            sender,
            results: Mutex::new(HashMap::new()),
        }
    }

    fn submit(&self, payload: String) -> Result<u64, mpsc::SendError<Job>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.results
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                id,
                JobResult {
                    status: "queued",
                    detail: "waiting for worker".to_owned(),
                },
            );
        self.sender.send(Job { id, payload })?;
        Ok(id)
    }
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn submit_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    let Some(payload) = command_text(cmd) else {
        let status = write_response(stream, "usage: rust_job_submit <payload>\n");
        return if status == SUCCESS { FALSE } else { status };
    };

    match JOB_QUEUE.submit(payload) {
        Ok(id) => write_response(stream, &format!("job queued id={id}\n")),
        Err(_) => fswtch::GENERR,
    }
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn status_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    let Some(id) = command_text(cmd).and_then(|text| text.parse::<u64>().ok()) else {
        let status = write_response(stream, "usage: rust_job_status <id>\n");
        return if status == SUCCESS { FALSE } else { status };
    };

    let results = JOB_QUEUE
        .results
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match results.get(&id) {
        Some(result) => write_response(
            stream,
            &format!(
                "id={id} status={} detail={}\n",
                result.status, result.detail
            ),
        ),
        None => write_response(stream, &format!("id={id} status=missing\n")),
    }
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    LazyLock::force(&JOB_QUEUE);
    // SAFETY: The loader passes the module slot and pool, and the module name is static.
    let module = match unsafe { Module::create(module_interface, pool, c"mod_async_job_queue") } {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    for result in [
        // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
        unsafe {
            module.add_api(
                c"rust_job_submit",
                c"queues background work without blocking FreeSWITCH API execution",
                c"rust_job_submit <payload>",
                submit_api,
            )
        },
        // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
        unsafe {
            module.add_api(
                c"rust_job_status",
                c"checks background job status",
                c"rust_job_status <id>",
                status_api,
            )
        },
    ] {
        if let Err(error) = result {
            return error.0;
        }
    }

    SUCCESS
}

fn command_text(cmd: *const c_char) -> Option<String> {
    if cmd.is_null() {
        return None;
    }

    // SAFETY: FreeSWITCH passes a null-terminated command string when one is present.
    unsafe { CStr::from_ptr(cmd) }
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn write_response(stream: *mut sys::switch_stream_handle_t, text: &str) -> Status {
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    let Some(mut stream) = (unsafe { Stream::from_raw(stream) }) else {
        return FALSE;
    };

    match stream.write_str(text) {
        Ok(()) => SUCCESS,
        Err(error) => error.0,
    }
}
