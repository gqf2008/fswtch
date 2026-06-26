//! Background task scheduler showcase.
//!
//! Demonstrates `fswtch`'s wrapper over FreeSWITCH's `switch_scheduler` engine: implement
//! [`fswtch::TaskHandler`], schedule the handler with [`fswtch::spawn`] backed by a
//! [`fswtch::TaskConfig`], and read the returned [`fswtch::TaskHandle`] id.
//!
//! The handler here increments a `LazyLock`-backed counter, and `run` flips the task into a
//! recurring one via [`fswtch::Task::set_repeat`] so the counter climbs on each tick.
//!
//! Load with `load mod_scheduler_task;` then, from `fs_cli`:
//! - `rust_scheduler_spawn`  — schedules a recurring counter task and writes its task id;
//! - `rust_scheduler_count`  — writes the current counter value.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

static LAST_TASK_ID: LazyLock<AtomicU32> = LazyLock::new(|| AtomicU32::new(0));

/// A scheduler handler that bumps the shared counter on every firing and reschedules itself
/// every second so the showcase keeps ticking.
struct CounterHandler;

impl fswtch::TaskHandler for CounterHandler {
    fn run(&mut self, task: &mut fswtch::Task) {
        let count = COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        let id = task.task_id();
        fswtch::log_info(
            "mod_scheduler_task",
            format_args!("scheduler task {id} fired, count={count}"),
        );
        // Reschedule one second out so the counter keeps climbing until the task is cancelled.
        task.set_repeat(1);
    }
}

fswtch::module_exports! {
    module = mod_scheduler_task,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn spawn_api(_cmd, _session, stream) {
        fswtch::log_info("mod_scheduler_task", "rust_scheduler_spawn invoked");

        let outcome: Result<String, String> = (|| {
            // `runtime == 0` fires immediately; `TaskConfig::new` leaks the static C strings once.
            let config = fswtch::TaskConfig::new(0, "rust counter", "rust_scheduler")
                .map_err(|e| format!("TaskConfig build failed: {e}"))?;
            let handle = fswtch::spawn(CounterHandler, config)
                .map_err(|e| format!("scheduler spawn failed: {e}"))?;
            let id = handle.id();
            LAST_TASK_ID.store(id, Ordering::Relaxed);
            // Detach the handle so the recurring task is not cancelled when it drops at end of scope.
            std::mem::forget(handle);
            Ok(format!("scheduled counter task id={id}\n"))
        })();

        match outcome {
            Ok(line) => stream.write(&line),
            Err(line) => stream.write(&format!("error: {line}\n")),
        }
    }
}

fswtch::api_callback! {
    fn count_api(_cmd, _session, stream) {
        fswtch::log_info("mod_scheduler_task", "rust_scheduler_count invoked");
        let count = COUNTER.load(Ordering::Relaxed);
        let last = LAST_TASK_ID.load(Ordering::Relaxed);
        stream.write(&format!("count={count} last_task_id={last}\n"))
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_scheduler_task" {
        fswtch::log_info("mod_scheduler_task", "loading module");
        // Ensure the FreeSWITCH scheduler thread is running before we schedule onto it.
        fswtch::start();
        module
            .api(
                "rust_scheduler_spawn",
                "schedules a recurring counter task on the FreeSWITCH scheduler",
                "rust_scheduler_spawn",
                spawn_api,
            )
            .and_then(|module| {
                module.api(
                    "rust_scheduler_count",
                    "prints the scheduler counter task firing count",
                    "rust_scheduler_count",
                    count_api,
                )
            })
    }
}
