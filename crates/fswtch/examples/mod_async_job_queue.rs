use std::{
    collections::HashMap,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::Duration,
};

static JOB_QUEUE: LazyLock<JobQueue> = LazyLock::new(JobQueue::start);
const MAX_JOB_RESULTS: usize = 4096;

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
    sender: Option<Sender<Job>>,
    results: Mutex<HashMap<u64, JobResult>>,
}

impl JobQueue {
    fn start() -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        let worker = thread::Builder::new()
            .name("fswtch-async-job-queue".to_owned())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    fswtch::log_info(
                        "mod_async_job_queue",
                        format!("worker processing job {}", job.id),
                    );
                    thread::sleep(Duration::from_millis(25));
                    let result = JobResult {
                        status: "done",
                        detail: format!("processed {} bytes", job.payload.len()),
                    };
                    let mut results = JOB_QUEUE
                        .results
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    prune_oldest_result(&mut results);
                    results.insert(job.id, result);
                }
            });

        let sender = match worker {
            Ok(_) => Some(sender),
            Err(error) => {
                fswtch::log_error(
                    "mod_async_job_queue",
                    format!("failed to start async job queue worker: {error}"),
                );
                None
            }
        };

        Self {
            next_id: AtomicU64::new(1),
            sender,
            results: Mutex::new(HashMap::new()),
        }
    }

    fn submit(&self, payload: String) -> Result<u64, &'static str> {
        let Some(sender) = &self.sender else {
            return Err("worker unavailable");
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut results = self
            .results
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_oldest_result(&mut results);
        results.insert(
            id,
            JobResult {
                status: "queued",
                detail: "waiting for worker".to_owned(),
            },
        );
        sender
            .send(Job { id, payload })
            .map_err(|_| "worker channel closed")?;
        Ok(id)
    }
}

fn prune_oldest_result(results: &mut HashMap<u64, JobResult>) {
    if results.len() < MAX_JOB_RESULTS {
        return;
    }
    if let Some(oldest) = results.keys().min().copied() {
        results.remove(&oldest);
    }
}

fswtch::api_callback! {
    fn submit_api(cmd, _session, stream) {
        fswtch::log_info("mod_async_job_queue", "rust_job_submit invoked");
        let Some(payload) = cmd else {
            fswtch::log_info("mod_async_job_queue", "missing job payload");
            let status = stream.write("usage: rust_job_submit <payload>\n");
            return fswtch::false_on_success(status);
        };

        match JOB_QUEUE.submit(payload) {
            Ok(id) => stream.write(&format!("job queued id={id}\n")),
            Err(error) => {
                fswtch::log_error("mod_async_job_queue", error);
                stream.write(&format!("job queue unavailable: {error}\n"))
            }
        }
    }
}

fswtch::api_callback! {
    fn status_api(cmd, _session, stream) {
        fswtch::log_info("mod_async_job_queue", "rust_job_status invoked");
        let Some(id) = cmd.and_then(|text| text.parse::<u64>().ok()) else {
            let status = stream.write("usage: rust_job_status <id>\n");
            return fswtch::false_on_success(status);
        };

        let results = JOB_QUEUE
            .results
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match results.get(&id) {
            Some(result) => stream.write(
                &format!(
                    "id={id} status={} detail={}\n",
                    result.status, result.detail
                ),
            ),
            None => stream.write(&format!("id={id} status=missing\n")),
        }
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_async_job_queue" {
        fswtch::log_info("mod_async_job_queue", "loading module");
        LazyLock::force(&JOB_QUEUE);
        module
            .api(
                "rust_job_submit",
                "queues background work without blocking FreeSWITCH API execution",
                "rust_job_submit <payload>",
                submit_api,
            )
            .and_then(|module| {
                module.api(
                    "rust_job_status",
                    "checks background job status",
                    "rust_job_status <id>",
                    status_api,
                )
            })
    }
}
