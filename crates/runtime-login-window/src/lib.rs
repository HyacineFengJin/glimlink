// SPDX-License-Identifier: MIT
//! The optional window is a client executable, never a Runtime dependency.
//! Keep pure navigation policy testable without GTK or any display server.
pub fn frame_navigation_allowed(uri: &str, local: &str) -> bool {
    if uri == local || uri == "about:blank" {
        return true;
    }
    let Some(rest) = uri.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.contains('@') || host.contains(':') {
        return false;
    }
    ["qq.com", "gtimg.com", "qcloud.com"]
        .iter()
        .any(|base| host == *base || host.ends_with(&format!(".{base}")))
        && !uri.chars().any(char::is_control)
        && uri.len() <= 16384
}

#[cfg(all(feature = "native-webview", target_os = "linux"))]
pub mod native;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allows_the_local_page_and_official_verification_frames_without_file_access_or_oauth() {
        let page = "http://127.0.0.1:1234/nonce";
        assert!(frame_navigation_allowed(page, page));
        assert!(frame_navigation_allowed(
            "https://turing.captcha.qcloud.com/path",
            page
        ));
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://127.0.0.1:22/",
            "https://qq.com.evil/",
            "https://qq.com@evil/",
            "https://accounts.google.com/o/oauth2/auth",
        ] {
            assert!(!frame_navigation_allowed(bad, page));
        }
    }
}
