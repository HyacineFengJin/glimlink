//! Explicit developer diagnostic. Raw authenticated responses stay in /state.
use md5::{Digest, Md5};
use platform_tieba::Session;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
fn shape(value: &Value, depth: u8) -> Value {
    match value {
        Value::Object(fields) if depth > 0 => Value::Object(
            fields
                .iter()
                .take(100)
                .map(|(k, v)| (k.clone(), shape(v, depth - 1)))
                .collect(),
        ),
        Value::Array(items) => {
            json!({"array_count":items.len(),"first_shape":items.first().map(|v|shape(v,depth.saturating_sub(1)))})
        }
        Value::String(s) => json!({"type":"string","empty":s.is_empty()}),
        Value::Number(_) => json!("number"),
        Value::Bool(_) => json!("bool"),
        Value::Null => Value::Null,
        _ => json!("object"),
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let kind = args.get(1).ok_or("provide reply or like")?;
    let endpoint = match kind.as_str() {
        "reply" => "replyme",
        "like" => "agreeme",
        _ => return Err("only failed reply/like reads are accepted".into()),
    };
    let session = Session::load(Path::new(
        args.get(2).ok_or("provide explicit private session path")?,
    ))?;
    let mut fields = BTreeMap::from([
        ("BDUSS", session.bduss().to_owned()),
        ("_client_version", "22.12.1.0".to_owned()),
        ("pn", "1".to_owned()),
    ]);
    let mut hash = Md5::new();
    for (key, value) in &fields {
        hash.update(format!("{key}={value}").as_bytes());
    }
    hash.update(b"tiebaclient!!!");
    fields.insert("sign", format!("{:x}", hash.finalize()));
    let body = fields
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    let api: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .proxy(None)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(25)))
        .build()
        .into();
    let mut response = api
        .post(format!("https://tiebac.baidu.com/c/u/feed/{endpoint}"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Cookie", &format!("BDUSS={}", session.bduss()))
        .send(body.as_bytes())?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("notification HTTP status {status}").into());
    }
    let mut raw = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 1024 * 1024 {
        return Err("response exceeds limit".into());
    }
    let dir = Path::new("/state/tieba-notification-diagnostics");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = dir.join(format!("{kind}-{nonce}.json"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(&raw)?;
    file.sync_all()?;
    let value: Value = serde_json::from_slice(&raw)?;
    let mut unusual = Vec::new();
    for key in ["reply_list", "at_list", "agree_list", "agreeme_list"] {
        if let Some(items) = value[key].as_array() {
            for (row, item) in items.iter().enumerate() {
                for field in [
                    "thread_id",
                    "post_id",
                    "quote_pid",
                    "time",
                    "unread",
                    "is_floor",
                    "replyer/id",
                ] {
                    if let Some(v) = item.pointer(&format!("/{field}")) {
                        if !v.is_null()
                            && !v.is_boolean()
                            && v.as_u64().is_none()
                            && v.as_str().and_then(|s| s.parse::<u64>().ok()).is_none()
                        {
                            unusual.push(json!({"row":row,"field":field,"shape":shape(v,0),"negative_sentinel":v.as_i64().is_some_and(|n|n<0)||v.as_str().is_some_and(|s|s.starts_with('-'))}));
                        }
                    }
                }
            }
        }
    }
    println!(
        "{}",
        json!({"kind":kind,"private_path":path,"shape":shape(&value,5),"unusual_numeric_fields":unusual})
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
