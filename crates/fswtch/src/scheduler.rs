//! Background task scheduler.
//!
//! Wraps FreeSWITCH's `switch_scheduler` engine: schedule a closure to run at a future epoch
//! second, optionally repeating. The engine dispatches each task on its own thread and hands the
//! callback a `switch_scheduler_task` describing the run.
//!
//! This module is callback-based and mirrors [`crate::media`]: a `TaskHandler` trait is boxed, leaked
//! into a `*mut c_void`, and recovered inside a generic `unsafe extern "C"` trampoline that wraps
//! the user callback in `catch_unwind` so a panicking handler cannot unwind across the FFI boundary.
//!
//! # Memory ownership
//!
//! The boxed handler is stored as the task's `cmd_arg` and is *never* handed to FreeSWITCH's
//! `SSHF_FREE_ARG` path (which would call libc `free` on a Rust `Box`). It is reclaimed on the
//! scheduler thread when the task reaches its terminal run — i.e. when the handler leaves
//! `task.repeat()` at `0` (the default for one-shot tasks) or explicitly calls
//! [`Task::set_repeat`]`(0)`. Cancelling a still-pending repeating task via [`TaskHandle::drop`]
//! prevents any future dispatch and is race-free, but the boxed handler is not reclaimed in that
//! case; prefer stopping repetition from inside the handler for long-lived repeating tasks.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;

use crate::{Result, cstring, log_error, sys};

use crate::command::borrowed_cstr_to_str;

/// Reclaim the boxed [`TaskState`] when a task reaches its terminal run.
///
/// This is invoked from the scheduler thread after the handler has run and `task.repeat == 0`, so no
/// further dispatch of this `cmd_arg` can race with the drop.
///
/// # Safety
///
/// `user_data` must be the `Box::into_raw` pointer returned by [`spawn`] for a task that will not be
/// dispatched again, and must not have been reclaimed already.
unsafe fn reclaim_state<H>(user_data: *mut c_void) {
    if user_data.is_null() {
        return;
    }
    // SAFETY: `user_data` is the `TaskState<H>` allocated by `spawn` and is now terminal.
    let _ = unsafe { Box::from_raw(user_data.cast::<TaskState<H>>()) };
}

/// A borrowed view over the FreeSWITCH `switch_scheduler_task` handed to a [`TaskHandler`] callback.
///
/// Borrows storage owned by the scheduler engine for the duration of one callback invocation. `Task`
/// is `Copy`; pass it by value.
#[derive(Copy, Clone)]
pub struct Task {
    raw: NonNull<sys::switch_scheduler_task>,
}

impl Task {
    /// Wraps a scheduler task pointer for the duration of a FreeSWITCH callback.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `switch_scheduler_task` and remain valid while this wrapper is
    /// used.
    pub(crate) unsafe fn from_raw(raw: *mut sys::switch_scheduler_task) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw })
    }

    #[inline]
    pub(crate) fn as_ptr(self) -> *mut sys::switch_scheduler_task {
        self.raw.as_ptr()
    }

    /// The epoch second at which this run was scheduled to execute.
    pub(crate) fn runtime(self) -> sys::time_t {
        // SAFETY: `self.raw` is live for this callback.
        unsafe { self.raw.as_ref().runtime }
    }

    /// The epoch second at which the task was first created.
    pub fn created(self) -> i64 {
        // SAFETY: `self.raw` is live for this callback.
        unsafe { self.raw.as_ref().created }
    }

    /// The opaque caller-supplied command id passed to [`spawn`].
    pub fn cmd_id(self) -> u32 {
        // SAFETY: `self.raw` is live for this callback.
        unsafe { self.raw.as_ref().cmd_id }
    }

    /// The engine-assigned id of this task. Matches the value returned by [`TaskHandle::id`].
    pub fn task_id(self) -> u32 {
        // SAFETY: `self.raw` is live for this callback.
        unsafe { self.raw.as_ref().task_id }
    }

    /// The repeat interval, in seconds. A nonzero value reschedules the task to run again at
    /// `runtime + repeat`; `0` marks this run as terminal (the default for one-shot tasks).
    pub fn repeat(self) -> u32 {
        // SAFETY: `self.raw` is live for this callback.
        unsafe { self.raw.as_ref().repeat }
    }

    /// Sets the repeat interval. Set to a nonzero value to make the task recurring, or to `0` to
    /// stop after this run (which reclaims the handler).
    pub fn set_repeat(&mut self, repeat: u32) {
        // SAFETY: `self.raw` is live and mutably borrowed for this callback.
        unsafe { self.raw.as_mut().repeat = repeat };
    }

    /// The group tag supplied to [`spawn`]. Copies the scheduler-owned C string into an owned
    /// `String` so it outlives the callback.
    pub fn group(self) -> Option<String> {
        // SAFETY: `self.raw` is live; `group` is a stable C string owned by the scheduler for the
        // callback duration.
        let ptr = unsafe { self.raw.as_ref().group };
        // SAFETY: `ptr` is null or a valid C string for this callback.
        unsafe { borrowed_cstr_to_str(ptr.cast_const()) }.map(ToOwned::to_owned)
    }
}

/// Receiver of scheduler callbacks.
///
/// Implement `run` to do work when the task fires. By default the task runs once and is then
/// reclaimed; call [`Task::set_repeat`] from inside `run` to make it recurring, or to stop a
/// repeating task by setting it back to `0`.
///
/// The handler is `'static` because it outlives the [`spawn`] call site — it is boxed and held by
/// the scheduler until the task terminates.
pub trait TaskHandler: 'static {
    /// Invoked once per scheduled firing of the task. Mutate `task` (e.g. via
    /// [`Task::set_repeat`]) to control recurrence.
    fn run(&mut self, task: &mut Task);
}

/// Behavioural flags passed to [`spawn`].
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TaskFlags(pub(crate) sys::switch_scheduler_flag_t);

impl TaskFlags {
    /// No special behaviour: the task runs on the scheduler thread, `cmd_arg` is owned by the
    /// caller (this crate), and the task is deleted after a terminal run. This is the correct
    /// default for Rust handlers.
    pub const NONE: Self = Self(sys::switch_scheduler_flag_enum_t_SSHF_NONE);

    /// Run the task on its own dedicated thread instead of the shared scheduler thread. Safe to use
    /// with [`TaskHandler`]; the trampoline remains race-free.
    pub const OWN_THREAD: Self = Self(sys::switch_scheduler_flag_enum_t_SSHF_OWN_THREAD);

    /// `SSHF_NO_DEL`: do not delete the task after a terminal run. Avoid for Rust handlers unless
    /// you also arrange manual cancellation via [`TaskHandle::cancel`], otherwise the boxed handler
    /// leaks.
    pub const NO_DEL: Self = Self(sys::switch_scheduler_flag_enum_t_SSHF_NO_DEL);

    /// `SSHF_FREE_ARG` is intentionally not exposed: it tells FreeSWITCH to `free()` the `cmd_arg`
    /// pointer, which would corrupt the Rust `Box` this crate stores there.
    pub(crate) const fn bits(self) -> sys::switch_scheduler_flag_t {
        self.0
    }
}

impl Default for TaskFlags {
    fn default() -> Self {
        Self::NONE
    }
}

impl std::ops::BitOr for TaskFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Configuration for a scheduled task. Mirrors the [`crate::media::MediaBugConfig`] style.
#[derive(Debug, Copy, Clone)]
pub struct TaskConfig {
    pub runtime: sys::time_t,
    pub desc: &'static std::ffi::CStr,
    pub group: &'static std::ffi::CStr,
    pub cmd_id: u32,
    pub flags: TaskFlags,
}

impl TaskConfig {
    /// Builds a one-shot task config scheduled to fire at epoch second `runtime`.
    ///
    /// `desc` and `group` are stored as static C strings (leaked once), matching the
    /// [`crate::StaticCStr`] convention used by [`crate::media`]. `runtime == 0` fires the task
    /// immediately.
    pub fn new(
        runtime: i64,
        desc: impl crate::StaticCStr,
        group: impl crate::StaticCStr,
    ) -> Result<Self> {
        Ok(Self {
            runtime,
            desc: desc.into_static_cstr()?,
            group: group.into_static_cstr()?,
            cmd_id: 0,
            flags: TaskFlags::default(),
        })
    }

    pub const fn cmd_id(mut self, cmd_id: u32) -> Self {
        self.cmd_id = cmd_id;
        self
    }

    pub const fn flags(mut self, flags: TaskFlags) -> Self {
        self.flags = flags;
        self
    }
}

/// RAII handle to a scheduled task.
///
/// Holds the engine-assigned task id. Dropping the handle cancels any future dispatch of the task
/// via `switch_scheduler_del_task_id`; see the module docs for ownership semantics around boxed
/// handlers.
#[derive(Debug)]
pub struct TaskHandle {
    task_id: u32,
}

impl TaskHandle {
    /// The engine-assigned id of this task.
    pub fn id(&self) -> u32 {
        self.task_id
    }

    /// Cancels the task, removing it from the scheduler so it will not fire again. Returns the
    /// number of tasks removed (always `1` on success, `0` if the task had already terminated).
    ///
    /// This does not reclaim a still-pending repeating handler's box; see the module docs.
    pub fn cancel(&self) -> u32 {
        // SAFETY: `task_id` is a valid engine id obtained from `switch_scheduler_add_task`.
        unsafe { sys::switch_scheduler_del_task_id(self.task_id) }
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        // SAFETY: `task_id` was returned by `switch_scheduler_add_task`.
        unsafe { sys::switch_scheduler_del_task_id(self.task_id) };
    }
}

struct TaskState<H> {
    handler: H,
}

/// Schedules `handler` to run once at epoch second `config.runtime` (immediately when `runtime ==
/// 0`), returning an RAII [`TaskHandle`].
///
/// `config.desc` is a human-readable description, `config.group` tags the task for batch
/// cancellation via [`cancel_group`], and `config.flags` controls dispatch behaviour (use
/// [`TaskFlags::NONE`] for the default). The handler is boxed and held by the scheduler until the
/// task terminates.
///
/// For recurring tasks, call [`Task::set_repeat`] from inside [`TaskHandler::run`].
pub fn spawn<H>(handler: H, config: TaskConfig) -> Result<TaskHandle>
where
    H: TaskHandler,
{
    let state = Box::into_raw(Box::new(TaskState { handler }));
    let func: sys::switch_scheduler_func_t = Some(task_trampoline::<H>);
    let flags = config.flags.bits();

    // SAFETY: `config.desc` / `config.group` are valid static C strings, `func` is a valid callback,
    // and `state` is allocated and remains valid until the trampoline reclaims it on the task's
    // terminal run. `switch_scheduler_add_task` is documented to return the new task's id and has
    // no failure sentinel, so ownership of `state` transfers unconditionally on this call.
    let task_id = unsafe {
        sys::switch_scheduler_add_task(
            config.runtime,
            func,
            config.desc.as_ptr(),
            config.group.as_ptr(),
            config.cmd_id,
            state.cast(),
            flags,
        )
    };

    // `state` is owned by the scheduler now; the trampoline reclaims it on the terminal run. We
    // deliberately do NOT reclaim here: `add_task` documents no failure return, and reclaiming on
    // an undocumented sentinel would risk a double-free if the scheduler ever did register the task.
    Ok(TaskHandle { task_id })
}

/// Cancels every scheduled task whose group tag matches `group`. Returns the number of tasks
/// removed.
pub fn cancel_group(group: impl AsRef<str>) -> Result<u32> {
    let group = cstring(group)?;
    // SAFETY: `group` is a valid C string for the call.
    let removed = unsafe { sys::switch_scheduler_del_task_group(group.as_ptr()) };
    Ok(removed)
}

/// Starts the FreeSWITCH scheduler engine. Must be called before [`spawn`]. Idempotent per the
/// engine; typically started once during module load.
pub fn start() {
    // SAFETY: No preconditions beyond a live FreeSWITCH core.
    unsafe { sys::switch_scheduler_task_thread_start() };
}

/// Stops the FreeSWITCH scheduler engine. Typically called during module shutdown.
pub fn stop() {
    // SAFETY: No preconditions beyond a live FreeSWITCH core.
    unsafe { sys::switch_scheduler_task_thread_stop() };
}

/// Generic trampoline adapting a [`TaskHandler`] to FreeSWITCH's `switch_scheduler_func_t`.
///
/// # Safety
///
/// FreeSWITCH must call this with the `task` it owns for the duration of the callback and the
/// `cmd_arg` pointer supplied by [`spawn`], which must be the boxed [`TaskState`] allocated there.
/// It must not invoke the callback again for a `cmd_arg` after this function has reclaimed it.
// SAFETY: FreeSWITCH passes the same task/cmd_arg pair registered by `spawn`.
unsafe extern "C" fn task_trampoline<H>(task: *mut sys::switch_scheduler_task)
where
    H: TaskHandler,
{
    if task.is_null() {
        return;
    }

    // SAFETY: FreeSWITCH passes a live task pointer for the callback duration.
    let Some(mut task_view) = (unsafe { Task::from_raw(task) }) else {
        return;
    };

    let user_data = {
        // SAFETY: `task` is live; `cmd_arg` is the pointer we stored in `spawn`.
        unsafe { (*task).cmd_arg }
    };
    if user_data.is_null() {
        return;
    }

    // SAFETY: `user_data` is the `TaskState<H>` pointer passed to `switch_scheduler_add_task`.
    let state = unsafe { &mut *user_data.cast::<TaskState<H>>() };

    let result = catch_unwind(AssertUnwindSafe(|| state.handler.run(&mut task_view)));
    if result.is_err() {
        log_error("scheduler", "task callback panicked");
    }

    // Read the live repeat interval after the handler ran: a handler may have called
    // `task.set_repeat(..)`. `repeat == 0` marks this run as terminal.
    let terminal = task_view.repeat() == 0;

    if terminal {
        // SAFETY: the task will not be dispatched again, so the box can be reclaimed on this
        // (scheduler) thread without racing a future callback.
        unsafe { reclaim_state::<H>(user_data) };
    }
}

#[cfg(all(test, feature = "live_fs"))]
mod tests {
    use super::*;

    #[derive(Default)]
    struct CountHandler {
        runs: u32,
    }

    impl TaskHandler for CountHandler {
        fn run(&mut self, _task: &mut Task) {
            self.runs += 1;
        }
    }

    #[test]
    fn flags_bitor_combines() {
        let combined = TaskFlags::NONE | TaskFlags::OWN_THREAD;
        assert_eq!(
            combined.bits(),
            sys::switch_scheduler_flag_enum_t_SSHF_OWN_THREAD
        );
    }

    #[test]
    fn config_builder_sets_fields() {
        let desc = "probe";
        let group = "tests";
        let config = TaskConfig::new(0, desc, group)
            .expect("static cstr")
            .cmd_id(7)
            .flags(TaskFlags::OWN_THREAD);

        assert_eq!(config.cmd_id, 7);
        assert_eq!(
            config.flags.bits(),
            sys::switch_scheduler_flag_enum_t_SSHF_OWN_THREAD
        );
    }

    #[test]
    fn flags_default_is_none() {
        assert_eq!(TaskFlags::default(), TaskFlags::NONE);
    }
}
