// SPDX-License-Identifier: MIT
use crate::{Options, Result};
use runtime_control::console::{Command, MAX_REQUEST, MAX_RESPONSE, Request, Response};
use serde_json::Value;
use std::{
    io::{BufReader, Read, Write},
    process::Child,
    sync::mpsc,
    thread,
    time::Duration,
};
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
pub fn request(options: &Options, command: Command) -> Result<Value> {
    let mut configured = options.clone();
    configured.ssh_batch = true;
    let options = &configured;
    let request = Request {
        version: 1,
        id: 1,
        command,
    };
    let bytes = serde_json::to_vec(&request).map_err(|_| "control_encode_failed")?;
    if bytes.len() + 1 > MAX_REQUEST {
        return Err("control_frame_limit".into());
    }
    let mut child = Running(
        crate::launch_management(options)?
            .spawn()
            .map_err(|_| "control_unavailable")?,
    );
    let mut input = child.0.stdin.take().ok_or("control_unavailable")?;
    input
        .write_all(&bytes)
        .and_then(|_| input.write_all(b"\n"))
        .and_then(|_| input.flush())
        .map_err(|_| "control_unavailable")?;
    drop(input);
    let stdout = child.0.stdout.take().ok_or("control_unavailable")?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let r = BufReader::new(stdout)
            .take((MAX_RESPONSE + 1) as u64)
            .read_to_end(&mut bytes);
        let _ = sender.send((r, bytes));
    });
    let received = receiver.recv_timeout(Duration::from_secs(25));
    let _ = child.0.kill();
    let _ = child.0.wait();
    let _ = reader.join();
    let (io, bytes) = received.map_err(|_| "control_timeout")?;
    io.map_err(|_| "control_unavailable")?;
    if bytes.len() > MAX_RESPONSE {
        return Err("control_response_limit".into());
    }
    let reply: Response = serde_json::from_slice(&bytes).map_err(|_| "invalid_control_response")?;
    if reply.version != 1 || reply.id != 1 {
        return Err("invalid_control_response".into());
    }
    if !reply.ok {
        return Err(reply.error.unwrap_or("control_request_failed".into()));
    }
    reply.result.ok_or("invalid_control_response".into())
}
pub fn worker(options: &Options, start: bool) -> Result<Value> {
    let mut configured = options.clone();
    configured.ssh_batch = true;
    let options = &configured;
    worker_reply(crate::launch_worker(options, start)?)
}
pub fn worker_status(options: &Options) -> Result<Value> {
    let mut configured = options.clone();
    configured.ssh_batch = true;
    worker_reply(crate::launch_worker_status(&configured)?)
}
pub fn worker_memory(options: &Options) -> Result<Value> {
    let mut configured = options.clone();
    configured.ssh_batch = true;
    worker_reply(crate::launch_worker_memory(&configured)?)
}
fn worker_reply(mut command: std::process::Command) -> Result<Value> {
    let mut child = Running(command.spawn().map_err(|_| "control_unavailable")?);
    drop(child.0.stdin.take());
    let stdout = child.0.stdout.take().ok_or("control_unavailable")?;
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut b = Vec::new();
        let r = stdout.take(4097).read_to_end(&mut b);
        let _ = tx.send((r, b));
    });
    let result = rx.recv_timeout(Duration::from_secs(45));
    let _ = child.0.kill();
    let _ = child.0.wait();
    let _ = reader.join();
    let (io, b) = result.map_err(|_| "control_timeout")?;
    io.map_err(|_| "control_unavailable")?;
    if b.len() > 4096 {
        return Err("control_response_limit".into());
    }
    let v: Value = serde_json::from_slice(&b).map_err(|_| "worker_control_unavailable")?;
    if v["ok"] != true {
        return Err("worker_control_unavailable".into());
    }
    Ok(v)
}
