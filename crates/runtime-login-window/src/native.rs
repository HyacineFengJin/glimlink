// SPDX-License-Identifier: MIT
//! Thin Linux system API bindings, not a bundled web engine. All GTK/WebKit
//! objects and callbacks stay on the main thread; no native JavaScript bridge.
use runtime_control::{
    login_window::{MAX_WINDOW_FRAME, WindowErrorCode, WindowEvent, WindowRequest},
    write_json,
};
use std::{
    cell::Cell,
    ffi::{CStr, CString, c_char, c_int, c_uint, c_ulong, c_void},
    io::{Read, Write},
    os::fd::FromRawFd,
    ptr,
    time::{Duration, Instant},
};

type Object = *mut c_void;
type Callback = unsafe extern "C" fn();
#[link(name = "gtk-3")]
unsafe extern "C" {
    fn gtk_init_check(argc: *mut c_int, argv: *mut *mut *mut c_char) -> c_int;
    fn gtk_window_new(kind: c_int) -> Object;
    fn gtk_window_set_title(window: Object, title: *const c_char);
    fn gtk_window_set_default_size(window: Object, width: c_int, height: c_int);
    fn gtk_container_add(container: Object, widget: Object);
    fn gtk_widget_show_all(widget: Object);
    fn gtk_widget_destroy(widget: Object);
    fn gtk_main();
    fn gtk_main_quit();
    fn gtk_main_level() -> c_uint;
}
#[link(name = "gobject-2.0")]
unsafe extern "C" {
    fn g_signal_connect_data(
        instance: Object,
        signal: *const c_char,
        callback: Callback,
        data: Object,
        destroy: Object,
        flags: c_int,
    ) -> c_ulong;
    fn g_object_unref(object: Object);
}
#[link(name = "glib-2.0")]
unsafe extern "C" {
    fn g_timeout_add_full(
        priority: c_int,
        interval: c_uint,
        function: unsafe extern "C" fn(Object) -> c_int,
        data: Object,
        notify: Object,
    ) -> c_uint;
    fn g_source_remove(tag: c_uint) -> c_int;
}
#[link(name = "webkit2gtk-4.1")]
unsafe extern "C" {
    fn webkit_web_context_new_ephemeral() -> Object;
    fn webkit_web_view_new_with_context(context: Object) -> Object;
    fn webkit_web_view_load_uri(view: Object, uri: *const c_char);
    fn webkit_permission_request_deny(request: Object);
    fn webkit_file_chooser_request_cancel(request: Object);
    fn webkit_download_cancel(download: Object);
    fn webkit_navigation_policy_decision_get_navigation_action(decision: Object) -> Object;
    fn webkit_navigation_action_get_request(action: Object) -> Object;
    fn webkit_uri_request_get_uri(request: Object) -> *const c_char;
    fn webkit_response_policy_decision_is_main_frame_main_resource(decision: Object) -> c_int;
    fn webkit_response_policy_decision_get_request(decision: Object) -> Object;
    fn webkit_policy_decision_ignore(decision: Object);
}

fn emit(event: WindowEvent) -> bool {
    write_json(&mut std::io::stdout().lock(), &event).is_ok()
}
fn read_start() -> Result<WindowRequest, ()> {
    let fd = unsafe { libc::fcntl(0, libc::F_DUPFD_CLOEXEC, 3) };
    if fd < 0 {
        return Err(());
    }
    let mut input = unsafe { std::fs::File::from_raw_fd(fd) };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.checked_duration_since(Instant::now()).ok_or(())?;
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut poll, 1, remaining.as_millis().min(5000) as c_int) } <= 0 {
            return Err(());
        }
        let mut one = [0u8; 1];
        if input.read(&mut one).map_err(|_| ())? != 1 {
            return Err(());
        }
        bytes.push(one[0]);
        if bytes.len() > MAX_WINDOW_FRAME {
            return Err(());
        }
        if one[0] == b'\n' {
            break;
        }
    }
    let request: WindowRequest = serde_json::from_slice(&bytes).map_err(|_| ())?;
    request.validate().map_err(|_| ())?;
    Ok(request)
}

struct State {
    window: Object,
    local: String,
    deadline: Instant,
    closed: Cell<bool>,
    closing: Cell<bool>,
    failed: Cell<bool>,
}
impl State {
    fn fail(&self, code: WindowErrorCode) {
        if self.closed.get() || self.closing.get() {
            return;
        }
        self.failed.set(true);
        let _ = emit(WindowEvent::error(code));
        // SAFETY: main-thread GTK object, destroyed at most once; destroy sets closed.
        self.close();
    }
    fn close(&self) {
        if !self.closed.get() && !self.closing.replace(true) {
            unsafe { gtk_widget_destroy(self.window) };
        }
    }
}
unsafe extern "C" fn destroyed(_: Object, data: Object) {
    // SAFETY: boxed State outlives the GTK main loop and every registered source.
    let state = unsafe { &*(data as *const State) };
    state.closing.set(true);
    if !state.closed.replace(true) {
        let _ = emit(WindowEvent::closed());
    }
    if unsafe { gtk_main_level() } > 0 {
        unsafe { gtk_main_quit() };
    }
}
unsafe extern "C" fn tick(data: Object) -> c_int {
    let state = unsafe { &*(data as *const State) };
    if state.closed.get() {
        return 0;
    }
    if Instant::now() >= state.deadline {
        state.fail(WindowErrorCode::Expired);
        return 1;
    }
    let mut poll = libc::pollfd {
        fd: 0,
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    let status = unsafe { libc::poll(&mut poll, 1, 0) };
    if status < 0 {
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            return 1;
        }
        state.fail(WindowErrorCode::InvalidInput);
        return 1;
    }
    if status > 0 {
        // The single opening frame is the entire command protocol. Later bytes
        // are rejected; closing the owner's pipe closes its window.
        if poll.revents & libc::POLLIN != 0 {
            let mut byte = 0u8;
            let n = unsafe { libc::read(0, &mut byte as *mut u8 as Object, 1) };
            if n > 0 {
                state.fail(WindowErrorCode::InvalidInput);
                return 1;
            }
        }
        state.close();
    }
    1
}
unsafe extern "C" fn permission(_: Object, request: Object, _: Object) -> c_int {
    unsafe { webkit_permission_request_deny(request) };
    1
}
unsafe extern "C" fn files(_: Object, request: Object, _: Object) -> c_int {
    unsafe { webkit_file_chooser_request_cancel(request) };
    1
}
unsafe extern "C" fn download(_: Object, request: Object, _: Object) {
    unsafe { webkit_download_cancel(request) }
}
unsafe fn uri(request: Object) -> Option<String> {
    if request.is_null() {
        return None;
    }
    let value = unsafe { webkit_uri_request_get_uri(request) };
    if value.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .ok()
        .filter(|s| s.len() <= 16384)
        .map(str::to_owned)
}
unsafe extern "C" fn policy(_: Object, decision: Object, kind: c_int, data: Object) -> c_int {
    let state = unsafe { &*(data as *const State) };
    if state.closed.get() {
        unsafe { webkit_policy_decision_ignore(decision) };
        return 1;
    }
    if kind == 1 {
        unsafe { webkit_policy_decision_ignore(decision) };
        return 1;
    } // no popup windows
    if kind == 0 {
        let action = unsafe { webkit_navigation_policy_decision_get_navigation_action(decision) };
        let request = if action.is_null() {
            ptr::null_mut()
        } else {
            unsafe { webkit_navigation_action_get_request(action) }
        };
        let allowed = unsafe { uri(request) }
            .is_some_and(|url| crate::frame_navigation_allowed(&url, &state.local));
        if !allowed {
            unsafe { webkit_policy_decision_ignore(decision) };
            return 1;
        }
    } else if kind == 2
        && unsafe { webkit_response_policy_decision_is_main_frame_main_resource(decision) } != 0
    {
        let request = unsafe { webkit_response_policy_decision_get_request(decision) };
        if unsafe { uri(request) }.as_deref() != Some(state.local.as_str()) {
            unsafe { webkit_policy_decision_ignore(decision) };
            state.fail(WindowErrorCode::NavigationFailed);
            return 1;
        }
    }
    0 // let WebKit apply its normal supported response policy
}
unsafe extern "C" fn load_failed(
    _: Object,
    _: c_int,
    _: *const c_char,
    _: Object,
    data: Object,
) -> c_int {
    unsafe { &*(data as *const State) }.fail(WindowErrorCode::NavigationFailed);
    1
}

pub fn run() -> i32 {
    let request = match read_start() {
        Ok(value) => value,
        Err(()) => {
            emit(WindowEvent::error(WindowErrorCode::InvalidInput));
            return 1;
        }
    };
    // GTK/WebKit initialization deliberately happens only in this optional
    // client process. No configuration or fake User-Agent is injected.
    if unsafe { gtk_init_check(ptr::null_mut(), ptr::null_mut()) } == 0 {
        emit(WindowEvent::error(WindowErrorCode::NoDisplay));
        return 1;
    }
    let title = match CString::new(request.title) {
        Ok(value) => value,
        Err(_) => {
            emit(WindowEvent::error(WindowErrorCode::InvalidInput));
            return 1;
        }
    };
    let url = CString::new(request.url.as_str()).expect("validated local page has no NUL");
    let window = unsafe { gtk_window_new(0) };
    let context = unsafe { webkit_web_context_new_ephemeral() };
    if window.is_null() || context.is_null() {
        emit(WindowEvent::error(WindowErrorCode::InitFailed));
        return 1;
    }
    let view = unsafe { webkit_web_view_new_with_context(context) };
    if view.is_null() {
        unsafe {
            g_object_unref(context);
            gtk_widget_destroy(window)
        };
        emit(WindowEvent::error(WindowErrorCode::InitFailed));
        return 1;
    }
    let state = Box::new(State {
        window,
        local: request.url,
        deadline: Instant::now() + Duration::from_secs(request.timeout_secs),
        closed: Cell::new(false),
        closing: Cell::new(false),
        failed: Cell::new(false),
    });
    let data = &*state as *const State as Object;
    // SAFETY: signal names/signatures match GTK3/WebKitGTK4.1 public APIs. GTK
    // owns the child widget; WebView retains the ephemeral context. State stays
    // boxed through main-loop termination and source removal.
    unsafe {
        g_signal_connect_data(
            window,
            c"destroy".as_ptr(),
            std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(destroyed),
            data,
            ptr::null_mut(),
            0,
        );
        g_signal_connect_data(
            view,
            c"permission-request".as_ptr(),
            std::mem::transmute::<unsafe extern "C" fn(Object, Object, Object) -> c_int, Callback>(
                permission,
            ),
            data,
            ptr::null_mut(),
            0,
        );
        g_signal_connect_data(
            view,
            c"run-file-chooser".as_ptr(),
            std::mem::transmute::<unsafe extern "C" fn(Object, Object, Object) -> c_int, Callback>(
                files,
            ),
            data,
            ptr::null_mut(),
            0,
        );
        g_signal_connect_data(
            view,
            c"decide-policy".as_ptr(),
            std::mem::transmute::<
                unsafe extern "C" fn(Object, Object, c_int, Object) -> c_int,
                Callback,
            >(policy),
            data,
            ptr::null_mut(),
            0,
        );
        g_signal_connect_data(
            view,
            c"load-failed".as_ptr(),
            std::mem::transmute::<
                unsafe extern "C" fn(Object, c_int, *const c_char, Object, Object) -> c_int,
                Callback,
            >(load_failed),
            data,
            ptr::null_mut(),
            0,
        );
        g_signal_connect_data(
            context,
            c"download-started".as_ptr(),
            std::mem::transmute::<unsafe extern "C" fn(Object, Object, Object), Callback>(download),
            data,
            ptr::null_mut(),
            0,
        );
        gtk_window_set_title(window, title.as_ptr());
        gtk_window_set_default_size(window, 560, 680);
        gtk_container_add(window, view);
        gtk_widget_show_all(window);
        webkit_web_view_load_uri(view, url.as_ptr());
        g_object_unref(context);
    }
    let timer = unsafe { g_timeout_add_full(0, 100, tick, data, ptr::null_mut()) };
    if timer == 0 {
        state.fail(WindowErrorCode::InitFailed);
        return 1;
    }
    if !emit(WindowEvent::ready()) {
        unsafe {
            gtk_widget_destroy(window);
            g_source_remove(timer)
        };
        return 1;
    }
    unsafe {
        gtk_main();
        g_source_remove(timer)
    };
    if !state.closed.get() {
        unsafe { gtk_widget_destroy(window) }
    }
    let _ = std::io::stdout().flush();
    if state.failed.get() { 1 } else { 0 }
}
