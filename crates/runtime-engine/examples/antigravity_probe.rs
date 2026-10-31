//! Explicit native ACP metadata/auth-required probe. Never sends a model prompt.
use runtime_engine::{AntigravityClient, AntigravityConfig, ErrorKind};
use serde_json::json;
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        eprintln!("usage: antigravity_probe BINARY EMPTY_HOME WORKSPACE BRIDGE_BINARY");
        std::process::exit(2);
    }
    let mut config = AntigravityConfig::new(&args[0], &args[1], &args[2]);
    config.bridge_binary = args[3].clone().into();
    config.bridge_socket = std::path::Path::new(&args[1]).join("bridge.sock");
    config.developer_instructions =
        Some("Synthetic offline persona probe; no model task is authorized.".into());
    let result = (|| {
        let mut client = AntigravityClient::spawn(config)?;
        let info = client.initialize()?;
        let auth = match client.new_session(&mut || Ok(())) {
            Err(error) if error.kind == ErrorKind::Authentication => true,
            Err(error) => return Err(error),
            Ok(_) => false,
        };
        Ok::<_, runtime_engine::EngineError>(
            json!({"passed":auth,"initialize":info,"authentication_required":auth,"authenticate_called":false,"model_prompts":0,"network_requested":false}),
        )
    })();
    match result {
        Ok(value) => {
            println!("{value}");
            if value["passed"] != true {
                std::process::exit(1);
            }
        }
        Err(error) => {
            println!(
                "{}",
                json!({"passed":false,"error_kind":error.kind,"detail":error.detail,"model_prompts":0})
            );
            std::process::exit(1);
        }
    }
}
