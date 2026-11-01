// SPDX-License-Identifier: MIT
//! Explicit launcher defaults, containing connection metadata only.
use runtime_operator::Options;
use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct Connection {
    mode: &'static str,
    destination: Option<String>,
    runtime_binary: Option<String>,
    state: String,
    account: String,
}
#[derive(Default)]
pub struct Defaults(Option<Connection>);

impl Defaults {
    pub fn parse(mut args: Vec<String>) -> Result<Self, &'static str> {
        if args.is_empty() {
            return Ok(Self::default());
        }
        let mut seen = std::collections::HashSet::new();
        if args.len() > 8 || args.len() % 2 != 0 {
            return Err("Expected --ssh HOST --runtime PATH --state PATH --account ALIAS.");
        }
        for pair in args.chunks_exact(2) {
            if !matches!(
                pair[0].as_str(),
                "--ssh" | "--runtime" | "--state" | "--account"
            ) || !seen.insert(pair[0].clone())
                || pair[1].is_empty()
                || pair[1].len() > 4096
                || pair[1].chars().any(char::is_control)
            {
                return Err("Invalid operator launcher connection.");
            }
        }
        args.extend(["bilibili".into(), "capabilities".into()]);
        let options = Options::parse(args)
            .map_err(|_| "Invalid operator launcher connection.")?
            .ok_or("Missing operator launcher connection.")?;
        Ok(Self(Some(Connection {
            mode: if options.ssh.is_some() {
                "ssh"
            } else {
                "local"
            },
            destination: options.ssh,
            runtime_binary: options.runtime.map(|p| p.to_string_lossy().into_owned()),
            state: options.state.to_string_lossy().into_owned(),
            account: options.account,
        })))
    }
}

#[tauri::command]
pub fn launch_connection(defaults: tauri::State<'_, Defaults>) -> Option<Connection> {
    defaults.0.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<Defaults, &'static str> {
        Defaults::parse(args.iter().map(|s| s.to_string()).collect())
    }
    #[test]
    fn launcher_defaults_are_explicit_metadata_without_connecting() {
        assert!(parse(&[]).unwrap().0.is_none());
        let c = parse(&[
            "--ssh",
            "configured-host",
            "--runtime",
            "/opt/glimlink/runtime",
            "--state",
            "/state",
            "--account",
            "main",
        ])
        .unwrap()
        .0
        .unwrap();
        assert_eq!(c.mode, "ssh");
        assert_eq!(c.destination.as_deref(), Some("configured-host"));
        assert_eq!(c.runtime_binary.as_deref(), Some("/opt/glimlink/runtime"));
        assert_eq!(c.state, "/state");
        assert_eq!(c.account, "main");
    }
    #[test]
    fn launcher_rejects_secrets_duplicate_flags_and_ssh_options() {
        for args in [
            vec!["--secret-file", "/private/input"],
            vec!["--ssh", "-oProxyCommand=bad"],
            vec!["--state", "a", "--state", "b"],
            vec!["--ssh"],
            vec!["--account", "a\nb"],
        ] {
            assert!(parse(&args).is_err());
        }
    }
}
