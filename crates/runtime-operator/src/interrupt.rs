// SPDX-License-Identifier: MIT
//! Process-local cancellation; the core has no signal or UI dependency on this.
use std::sync::atomic::{AtomicBool, Ordering};
pub(crate) static CANCELLED: AtomicBool = AtomicBool::new(false);
extern "C" fn received(_: libc::c_int) {
    CANCELLED.store(true, Ordering::Relaxed);
}
pub fn is_interrupted() -> bool {
    CANCELLED.load(Ordering::Relaxed)
}
pub struct Guard {
    previous: libc::sigaction,
}
pub fn install() -> std::result::Result<Guard, String> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = received as usize;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGINT, &action, &mut previous) } != 0 {
        return Err("cannot install operator cancellation handler".into());
    }
    Ok(Guard { previous })
}
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            libc::sigaction(libc::SIGINT, &self.previous, std::ptr::null_mut());
        }
    }
}
