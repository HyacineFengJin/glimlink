// SPDX-License-Identifier: MIT
//! Synthetic native auth peer. No credentials or network operations.
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
};
fn send(value: Value) {
    println!("{value}");
    io::stdout().flush().unwrap();
}
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    if args.iter().any(|s| s == "app-server")
        || args.first().is_some_and(|s| s.starts_with("auth-"))
    {
        let scenario = args
            .iter()
            .find(|s| s.starts_with("auth-"))
            .map(String::as_str)
            .unwrap_or("auth-success");
        for line in io::stdin().lock().lines() {
            let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
            let id = &request["id"];
            match request["method"].as_str().unwrap_or("") {
                "initialize" => {
                    send(json!({"id":id,"result":{"userAgent":"synthetic-auth-native"}}))
                }
                "initialized" => (),
                "account/read" => send(
                    json!({"id":id,"result":{"account":if home.join("synthetic-authenticated").is_file(){json!({"type":"chatgpt","planType":"plus","email":"synthetic-private@example.invalid"})}else{Value::Null},"requiresOpenaiAuth":true}}),
                ),
                "account/login/start" => {
                    assert_eq!(request["params"]["type"], "chatgptDeviceCode");
                    let completion = json!({"method":"account/login/completed","params":{"loginId":"native-synthetic-login","success":scenario!="auth-failed","error":"SYNTHETIC_PRIVATE_TOKEN_MUST_NOT_LEAK"}});
                    if scenario == "auth-early" {
                        send(completion.clone());
                    }
                    send(
                        json!({"id":id,"result":{"type":"chatgptDeviceCode","loginId":"native-synthetic-login","userCode":"ABCD-EFGH","verificationUrl":if scenario=="auth-bad-url"{"https://auth.openai.com.evil/device"}else{"https://auth.openai.com/codex/device"}}}),
                    );
                    if scenario != "auth-wait" && scenario != "auth-bad-url" {
                        if scenario != "auth-failed" {
                            std::fs::write(home.join("synthetic-authenticated"), b"synthetic")
                                .unwrap();
                        }
                        if scenario != "auth-early" {
                            send(completion);
                        }
                    }
                }
                "account/login/cancel" => {
                    assert_eq!(request["params"]["loginId"], "native-synthetic-login");
                    send(json!({"id":id,"result":{"status":"canceled"}}));
                }
                _ => panic!("auth fixture received a non-auth command"),
            }
        }
    } else if args.iter().any(|s| s == "acp") {
        for line in io::stdin().lock().lines() {
            let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
            let id = &request["id"];
            match request["method"].as_str().unwrap_or("") {
                "initialize" => send(
                    json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":1,"agentCapabilities":{"_meta":{"antigravity":{"systemPrompt":{"append":true}}}},"authMethods":[{"id":"oauth-personal"}]}}),
                ),
                "session/new" => {
                    if home.join("synthetic-authenticated").is_file() {
                        send(
                            json!({"jsonrpc":"2.0","id":id,"result":{"sessionId":"synthetic-login-probe"}}),
                        );
                    } else {
                        send(
                            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"SYNTHETIC_PRIVATE_AUTH_DETAILS"}}),
                        );
                    }
                }
                _ => panic!("auth fixture received a non-auth ACP command"),
            }
        }
    } else {
        assert!(std::env::var_os("SSH_CONNECTION").is_some());
        assert_eq!(std::env::var("BROWSER").unwrap(), "/usr/bin/false");
        let mut termios = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::tcgetattr(0, &mut termios) }, 0);
        unsafe {
            libc::cfmakeraw(&mut termios);
        }
        assert_eq!(unsafe { libc::tcsetattr(0, libc::TCSANOW, &termios) }, 0);
        std::fs::write(
            home.join("synthetic-login-pid"),
            std::process::id().to_string(),
        )
        .unwrap();
        print!("Select login method:\r\n > 1. Google OAuth\r\n");
        io::stdout().flush().unwrap();
        let mut input = io::stdin().lock();
        let mut byte = [0];
        input.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [b'\r']);
        print!(
            "\x1b]8;id=synthetic;https://accounts.google.com/o/oauth2/auth?redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback&response_type=code&code_challenge_method=S256&state=synthetic&client_id=synthetic&code_challenge=synthetic\x07Open browser\x1b]8;;\x07\r\nPaste authorization code...\r\n"
        );
        io::stdout().flush().unwrap();
        let mut code = Vec::new();
        loop {
            input.read_exact(&mut byte).unwrap();
            if byte[0] == b'\r' {
                break;
            }
            code.push(byte[0]);
        }
        if code == b"synthetic-rejected-code" {
            print!("OAuth: token exchange failed: SYNTHETIC_PRIVATE_ERROR_TOKEN\r\n");
            io::stdout().flush().unwrap();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        assert_eq!(code, b"synthetic-once-code");
        std::fs::write(home.join("synthetic-authenticated"), b"synthetic").unwrap();
        print!("OAuth: authenticated successfully as SYNTHETIC_PRIVATE_ACCOUNT\r\n");
        io::stdout().flush().unwrap();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}
