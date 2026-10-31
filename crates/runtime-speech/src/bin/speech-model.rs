// SPDX-License-Identifier: MIT
//! Explicit model management and public-WAV acceptance harness.
use std::{io::Read, path::Path};
fn wav(path: &Path) -> Result<(Vec<f32>, u32, u16), String> {
    let meta = std::fs::metadata(path).map_err(|_| "WAV fixture missing")?;
    if !meta.is_file() || meta.len() > 4 * 1024 * 1024 {
        return Err("WAV fixture exceeds4MiB".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "WAV fixture open failed")?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "WAV fixture read failed")?;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("fixture must be RIFF WAV".into());
    }
    let mut at = 12;
    let mut fmt = None;
    let mut data = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let n = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let end = at
            .checked_add(8)
            .and_then(|p| p.checked_add(n))
            .filter(|p| *p <= bytes.len())
            .ok_or("invalid WAV fixture chunk")?;
        let chunk = &bytes[at + 8..end];
        if id == b"fmt " {
            if chunk.len() < 16 {
                return Err("short WAV fmt".into());
            }
            let read16 = |at| u16::from_le_bytes(chunk[at..at + 2].try_into().unwrap());
            let rate = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
            fmt = Some((read16(0), read16(2), rate, read16(14)));
        } else if id == b"data" {
            data = Some(chunk)
        }
        at = end + n % 2;
    }
    let (codec, channels, rate, bits) = fmt.ok_or("WAV fmt missing")?;
    if codec != 1 || bits != 16 || !(1..=2).contains(&channels) || !(8000..=96000).contains(&rate) {
        return Err("fixture only accepts PCM16 mono/stereo WAV".into());
    }
    let data = data.ok_or("WAV data missing")?;
    let frames = (data.len() / 2 / usize::from(channels)).min(rate as usize * 10);
    let samples = data[..frames * usize::from(channels) * 2]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes(b.try_into().unwrap()) as f32 / 32768.0)
        .collect();
    Ok((samples, rate, channels))
}
fn main() {
    let result = (|| -> Result<serde_json::Value, String> {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        if args.len() < 2 {
            return Err("usage: speech-model install|status DIRECTORY; speech-model transcribe-wav DIRECTORY PUBLIC_FIXTURE.wav [LANGUAGE]".into());
        }
        let dir = Path::new(&args[1]);
        match args[0].as_str() {
            "install" if args.len() == 2 => serde_json::to_value(runtime_speech::install(dir)?)
                .map_err(|_| "cannot serialize model status".into()),
            "status" if args.len() == 2 => serde_json::to_value(runtime_speech::status(dir)?)
                .map_err(|_| "cannot serialize model status".into()),
            "transcribe-wav" if (3..=4).contains(&args.len()) => {
                let (samples, rate, channels) = wav(Path::new(&args[2]))?;
                let started = std::time::Instant::now();
                let t = runtime_speech::transcribe(
                    dir,
                    &samples,
                    rate,
                    channels,
                    args.get(3).map(String::as_str),
                )?;
                let mut value = serde_json::to_value(t)
                    .map_err(|_| String::from("cannot serialize transcript"))?;
                let peak = std::fs::read_to_string("/proc/self/status")
                    .ok()
                    .and_then(|s| {
                        s.lines()
                            .find(|l| l.starts_with("VmHWM:"))
                            .and_then(|l| l.split_whitespace().nth(1))
                            .and_then(|v| v.parse::<u64>().ok())
                    });
                value["acceptance_metrics"] = serde_json::json!({"elapsed_ms":started.elapsed().as_millis(),"peak_rss_kib":peak,"container_memory_peak_bytes":std::fs::read_to_string("/sys/fs/cgroup/memory.peak").ok().and_then(|s|s.trim().parse::<u64>().ok())});
                Ok(value)
            }
            _ => Err("invalid speech-model management command".into()),
        }
    })();
    match result {
        Ok(v) => println!("{}", v),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1)
        }
    }
}
