//! Native, account-free probe. Pass an explicit empty private CODEX_HOME.
//! Does not start a model turn or perform a login flow.
use runtime_engine::{CodexClient, CodexConfig};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: native_probe BINARY EMPTY_CODEX_HOME WORKSPACE".into());
    }
    let mut config = CodexConfig::new(&args[0], &args[1], &args[2]);
    config.timeout = std::time::Duration::from_secs(15);
    let mut client = CodexClient::spawn(config)?;
    let server = client.initialize()?;
    let account = client.account_read()?;
    println!(
        "{}",
        serde_json::json!({"server":server,"account":account,"model_turn_started":false})
    );
    client.shutdown();
    Ok(())
}
