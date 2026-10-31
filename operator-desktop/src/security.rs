// SPDX-License-Identifier: MIT
//! Management IPC belongs only to the packaged main document, never a login page.
use tauri::Url;

pub(crate) fn local_document(url: &Url) -> bool {
    url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && matches!(url.path(), "" | "/" | "/index.html")
        && ((url.scheme() == "tauri" && url.host_str() == Some("localhost"))
            || (matches!(url.scheme(), "http" | "https")
                && url.host_str() == Some("tauri.localhost")))
}

pub(crate) fn management_caller(webview: &str, window: &str, url: &Url) -> bool {
    webview == "main" && window == "main" && local_document(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_packaged_main_has_management_access() {
        for url in [
            "tauri://localhost/index.html",
            "http://tauri.localhost/",
            "https://tauri.localhost/index.html",
        ] {
            let url: Url = url.parse().unwrap();
            assert!(management_caller("main", "main", &url));
            assert!(!management_caller("x-login-1", "main", &url));
            assert!(!management_caller("main", "x-login-1", &url));
        }
        for url in [
            "https://x.com/",
            "https://tauri.localhost.evil/",
            "https://tauri.localhost:444/",
            "https://a@tauri.localhost/",
            "tauri://localhost/remote.html",
            "file:///index.html",
            "http://127.0.0.1/",
        ] {
            assert!(!management_caller("main", "main", &url.parse().unwrap()));
        }
    }
}
