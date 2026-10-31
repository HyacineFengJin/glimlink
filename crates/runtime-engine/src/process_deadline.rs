// SPDX-License-Identifier: MIT
//! A one-shot process-group deadline independent of client polling. The owner
//! must drop this guard before reaping the child, keeping its PID allocated
//! while the watchdog may signal it.
use crate::{EngineError, ErrorKind, Result};
use std::{
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};
pub(crate) struct ProcessDeadline {
    stop: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}
impl ProcessDeadline {
    pub(crate) fn new(pid: u32, timeout: Duration) -> Result<Self> {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let shared = stop.clone();
        let worker = thread::Builder::new()
            .name("native-login-deadline".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                let (lock, condition) = &*shared;
                let Ok(stopped) = lock.lock() else {
                    return;
                };
                let Ok((stopped, timed)) = condition.wait_timeout_while(stopped, timeout, |s| !*s)
                else {
                    return;
                };
                if !*stopped && timed.timed_out() {
                    #[cfg(target_os = "linux")]
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
            })
            .map_err(|_| EngineError::new(ErrorKind::Io, "cannot start native login deadline"))?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for ProcessDeadline {
    fn drop(&mut self) {
        let (lock, condition) = &*self.stop;
        if let Ok(mut stopped) = lock.lock() {
            *stopped = true;
            condition.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
