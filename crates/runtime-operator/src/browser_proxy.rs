// SPDX-License-Identifier: MIT
//! Follow an already configured desktop proxy when Chromium cannot discover it.
//! This is operator-only: no global settings, Runtime routing or credentials are
//! changed. Native GNOME/KDE and explicit proxy environments remain browser-owned.
use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn quoted(value: &str) -> Option<&str> {
    value.trim().strip_prefix('\'')?.strip_suffix('\'')
}
fn endpoint(host: &str, port: &str) -> Option<Option<String>> {
    let host = quoted(host)?;
    let port: u16 = port.trim().parse().ok()?;
    if host.is_empty() || port == 0 {
        return Some(None);
    }
    let address = match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(ip)) => format!("[{ip}]"),
        Ok(ip) => ip.to_string(),
        Err(_)
            if host.len() <= 253
                && host.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label
                            .as_bytes()
                            .first()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && label
                            .as_bytes()
                            .last()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                }) =>
        {
            host.into()
        }
        _ => return None,
    };
    Some(Some(format!("{address}:{port}")))
}
fn bypass(value: &str) -> Option<String> {
    let value = value.trim().strip_prefix("@as ").unwrap_or(value.trim());
    let body = value.strip_prefix('[')?.strip_suffix(']')?;
    if body.trim().is_empty() {
        return Some(String::new());
    }
    let mut rows = Vec::new();
    for row in body.split(',') {
        let row = quoted(row)?;
        if rows.len() >= 64
            || row.is_empty()
            || row.len() > 256
            || !row
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".*-:/[]".contains(&b))
        {
            return None;
        }
        rows.push(row);
    }
    Some(rows.join(";"))
}
fn manual_arguments(mut read: impl FnMut(&str, &str) -> Option<String>) -> Option<Vec<String>> {
    let schema = "org.gnome.system.proxy";
    if read(schema, "mode")?.trim() != "'manual'" {
        return None;
    }
    let same = match read(schema, "use-same-proxy")?.trim() {
        "true" => true,
        "false" => false,
        _ => return None,
    };
    let http = endpoint(
        &read("org.gnome.system.proxy.http", "host")?,
        &read("org.gnome.system.proxy.http", "port")?,
    )?;
    let https = if same {
        http.clone()
    } else {
        endpoint(
            &read("org.gnome.system.proxy.https", "host")?,
            &read("org.gnome.system.proxy.https", "port")?,
        )?
    };
    let socks = if same {
        None
    } else {
        endpoint(
            &read("org.gnome.system.proxy.socks", "host")?,
            &read("org.gnome.system.proxy.socks", "port")?,
        )?
    };
    let mut servers = Vec::new();
    if let Some(http) = http {
        servers.push(format!("http=http://{http}"));
    }
    if let Some(https) = https {
        servers.push(format!("https=http://{https}"));
    }
    if let Some(socks) = socks {
        servers.push(format!("socks=socks5://{socks}"));
    }
    if servers.is_empty() {
        return None;
    }
    // Do not partially apply configuration when exclusion syntax is unknown.
    let exclusions = bypass(&read(schema, "ignore-hosts")?)?;
    let mut args = vec![format!("--proxy-server={}", servers.join(";"))];
    if !exclusions.is_empty() {
        args.push(format!("--proxy-bypass-list={exclusions}"));
    }
    Some(args)
}
fn settings(schema: &str, key: &str, deadline: Instant) -> Option<String> {
    if Instant::now() >= deadline {
        return None;
    }
    let mut child = Command::new("gsettings")
        .args(["get", schema, key])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let end = deadline.min(Instant::now() + Duration::from_millis(400));
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut bytes = Vec::new();
                child
                    .stdout
                    .take()?
                    .take(4097)
                    .read_to_end(&mut bytes)
                    .ok()?;
                return (bytes.len() <= 4096)
                    .then(|| String::from_utf8(bytes).ok())
                    .flatten();
            }
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}
fn native_desktop(desktop: &str) -> bool {
    desktop.split(':').any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            "gnome" | "kde" | "unity" | "cinnamon" | "mate"
        )
    })
}
pub(crate) fn arguments() -> Vec<String> {
    if !cfg!(target_os = "linux")
        || native_desktop(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
        || [
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "SOCKS_SERVER",
        ]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|v| !v.is_empty()))
    {
        return Vec::new();
    }
    let deadline = Instant::now() + Duration::from_millis(1500);
    manual_arguments(|schema, key| settings(schema, key, deadline)).unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn config(same: bool) -> impl FnMut(&str, &str) -> Option<String> {
        move |schema, key| {
            Some(match (schema, key) {
                (_, "mode") => "'manual'".into(),
                (_, "use-same-proxy") => same.to_string(),
                (_, "ignore-hosts") => "['localhost', '127.0.0.1', '10.0.0.0/8', '::1']".into(),
                ("org.gnome.system.proxy.http", "host") => "'127.0.0.1'".into(),
                ("org.gnome.system.proxy.http", "port") => "7897".into(),
                ("org.gnome.system.proxy.https", "host") => "'::1'".into(),
                ("org.gnome.system.proxy.https", "port") => "8080".into(),
                ("org.gnome.system.proxy.socks", "host") => "''".into(),
                ("org.gnome.system.proxy.socks", "port") => "0".into(),
                _ => return None,
            })
        }
    }
    #[test]
    fn system_proxy_preserves_shared_per_scheme_and_excluded_destinations() {
        assert_eq!(
            manual_arguments(config(true)).unwrap(),
            vec![
                "--proxy-server=http=http://127.0.0.1:7897;https=http://127.0.0.1:7897",
                "--proxy-bypass-list=localhost;127.0.0.1;10.0.0.0/8;::1"
            ]
        );
        assert!(manual_arguments(config(false)).unwrap()[0].contains("https=http://[::1]:8080"));
        for desktop in ["GNOME", "ubuntu:GNOME", "KDE", "X-Cinnamon:Cinnamon"] {
            assert!(native_desktop(desktop));
        }
        for desktop in ["niri", "sway", "Hyprland", ""] {
            assert!(!native_desktop(desktop));
        }
    }
    #[test]
    fn absent_disabled_unknown_and_credential_bearing_settings_are_not_overrides() {
        assert!(manual_arguments(|_, _| None).is_none());
        for mode in ["'none'", "'auto'", "'unknown'"] {
            let mut read = config(true);
            assert!(
                manual_arguments(|schema, key| if key == "mode" {
                    Some(mode.into())
                } else {
                    read(schema, key)
                })
                .is_none()
            );
        }
        for host in [
            "'user:password@host'",
            "'host;direct://'",
            "'http://host'",
            "'--argument'",
            "'host\\nother'",
        ] {
            assert!(endpoint(host, "7897").is_none());
        }
        assert!(endpoint("'127.0.0.1'", "65536").is_none());
        assert!(bypass("['localhost;*']").is_none());
        assert!(bypass("@as []").unwrap().is_empty());
    }
}
