// SPDX-License-Identifier: MIT
//! Public synthetic-codec acceptance harness; not used by the media runtime.
use std::{
    fs::{File, OpenOptions},
    io::{BufReader, BufWriter, Write},
};
fn main() {
    let result = (|| -> Result<(), String> {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        if !(2..=3).contains(&args.len()) {
            return Err("usage: decode INPUT.silk OUTPUT.pcm [RATE]".into());
        }
        let rate = args
            .get(2)
            .map(|s| s.parse::<u32>().map_err(|_| "invalid rate"))
            .transpose()?
            .unwrap_or(24000);
        let input = File::open(&args[0]).map_err(|_| "input open failed")?;
        if input.metadata().map_err(|_| "input stat failed")?.len() > 2 * 1024 * 1024 {
            return Err("synthetic input exceeds2MiB".into());
        }
        let mut packets =
            runtime_silk::PacketReader::new(BufReader::new(input)).map_err(|e| e.to_string())?;
        let mut decoder = runtime_silk::Decoder::new(rate).map_err(|e| e.to_string())?;
        let mut output = BufWriter::new(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&args[1])
                .map_err(|_| "output already exists or unavailable")?,
        );
        let mut samples = 0u64;
        while let Some(packet) = packets.next_packet().map_err(|e| e.to_string())? {
            let pcm = decoder.decode_packet(&packet).map_err(|e| e.to_string())?;
            samples += pcm.len() as u64;
            if samples > u64::from(rate) * 120 {
                return Err("synthetic output exceeds120seconds".into());
            }
            for s in pcm {
                output
                    .write_all(&s.to_le_bytes())
                    .map_err(|_| "output write failed")?;
            }
        }
        output.flush().map_err(|_| "output flush failed")?;
        println!("{{\"sample_rate\":{rate},\"samples\":{samples},\"packets\":{},\"tencent\":{},\"codec\":\"skype_silk_v3\",\"native_codec\":false}}",packets.packets_read(),packets.is_tencent());
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
