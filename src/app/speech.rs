// SPDX-License-Identifier: MIT
//! Prepared PCM -> attributed speech text, inside the same media task.
use super::*;
use std::io::{Seek, SeekFrom};
/// Translate platform track language tags to Whisper language codes. Numeric
/// subtitle track IDs select a subtitle only and leave speech auto-detection on.
pub(super) fn normalize_speech_language(value: &str) -> Result<Option<String>> {
    let value = value.trim().to_ascii_lowercase();
    if value == "auto" || value.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    let value = value.strip_prefix("ai-").unwrap_or(&value);
    let code = value.split(['-', '_']).next().unwrap_or("");
    let code = match code {
        "cmn" | "zho" | "chi" => "zh",
        "eng" => "en",
        "jpn" => "ja",
        "kor" => "ko",
        other => other,
    };
    if !(2..=3).contains(&code.len()) || !code.bytes().all(|b| b.is_ascii_lowercase()) {
        return Err("speech language must be a language code, or auto".into());
    }
    Ok(Some(code.into()))
}
#[cfg(test)]
thread_local! {static TRANSCRIPTION_CALLS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
#[cfg(test)]
pub(super) fn reset_transcription_calls() {
    TRANSCRIPTION_CALLS.set(0)
}
#[cfg(test)]
pub(super) fn transcription_calls() -> usize {
    TRANSCRIPTION_CALLS.get()
}
pub fn model_directory(state: &Path) -> Result<PathBuf> {
    if let Some(configured) = std::env::var_os("AGENT_RUNTIME_SPEECH_DIR") {
        let configured = PathBuf::from(configured);
        if !configured.is_absolute() {
            return Err("speech model directory must be absolute".into());
        }
        return Ok(configured);
    }
    let state = if state.is_absolute() {
        state.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(state)
    };
    let path = state.join("models/whisper-tiny");
    Ok(path)
}
impl Runtime {
    pub(super) fn transcribe_prepared_audio(
        &self,
        id: &str,
        start_ms: u64,
        language: Option<&str>,
    ) -> Result<Value> {
        #[cfg(test)]
        TRANSCRIPTION_CALLS.set(TRANSCRIPTION_CALLS.get() + 1);
        let models = model_directory(&self.state)?;
        let status = runtime_speech::status(&models)?;
        if !status.installed {
            return Ok(
                json!({"status":"not_installed","available":false,"install_command":"agent-runtime speech install","automatic_download":false,"environment_sounds_understood":false}),
            );
        }
        let (_, resource, path) = self.resolve_any(id)?;
        if resource.mime != "audio/wav" {
            return Err("transcription requires Runtime-prepared PCM WAV".into());
        }
        let mut file = open_regular(&path, runtime_media::Limits::default().max_output_bytes)?;
        let mut header = [0u8; 44];
        file.read_exact(&mut header).map_err(|e| e.to_string())?;
        if &header[..4] != b"RIFF"
            || &header[8..12] != b"WAVE"
            || &header[12..16] != b"fmt "
            || &header[36..40] != b"data"
            || header[20..22] != [1, 0]
            || header[34..36] != [16, 0]
        {
            return Err("transcription accepts canonical PCM16 WAV output only".into());
        }
        let channels = u16::from_le_bytes([header[22], header[23]]);
        let rate = u32::from_le_bytes(header[24..28].try_into().unwrap());
        if !(1..=2).contains(&channels) || rate == 0 || rate > 96000 {
            return Err("invalid transcription PCM layout".into());
        }
        let data = u32::from_le_bytes(header[40..44].try_into().unwrap()) as usize;
        if data as u64 + 44 != file.metadata().map_err(|e| e.to_string())?.len() {
            return Err("prepared WAV size differs from header".into());
        }
        let chunk_frames = (u64::from(rate) * runtime_speech::MAX_WINDOW_MS / 1000) as usize;
        let chunk_bytes = chunk_frames * usize::from(channels) * 2;
        let mut remaining = data;
        let mut offset_frames = 0u64;
        let mut segments = Vec::new();
        let mut texts = Vec::new();
        let mut windows = Vec::new();
        let mut detected = language.map(str::to_owned);
        file.seek(SeekFrom::Start(44)).map_err(|e| e.to_string())?;
        while remaining > 0 {
            if windows.len() >= 6 {
                return Err("speech window count exceeds60s media budget".into());
            }
            let n = remaining.min(chunk_bytes);
            if n % (usize::from(channels) * 2) != 0 {
                return Err("incomplete WAV PCM frame".into());
            }
            let mut bytes = vec![0u8; n];
            file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
            let samples = bytes
                .chunks_exact(2)
                .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.)
                .collect::<Vec<_>>();
            drop(bytes);
            let transcript =
                runtime_speech::transcribe(&models, &samples, rate, channels, detected.as_deref())?;
            let mut result = serde_json::to_value(transcript).map_err(|e| e.to_string())?;
            let base = start_ms + offset_frames * 1000 / u64::from(rate);
            if detected.is_none() {
                detected = result
                    .get("language")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }
            if let Some(lines) = result.get_mut("segments").and_then(Value::as_array_mut) {
                for line in lines {
                    for key in ["start_ms", "end_ms"] {
                        if let Some(t) = line.get(key).and_then(Value::as_u64) {
                            line[key] = json!(base + t)
                        }
                    }
                    segments.push(line.clone())
                }
            }
            if let Some(text) = result
                .get("text")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                texts.push(text.to_owned())
            }
            windows.push(json!({"start_ms":base,"audio_ms":result["audio_ms"],"avg_logprob":result["avg_logprob"],"no_speech_probability":result["no_speech_probability"],"truncated":result["truncated"]}));
            offset_frames += n as u64 / (u64::from(channels) * 2);
            remaining -= n;
        }
        Ok(
            json!({"status":"ok","available":true,"text":texts.join(" "),"language":detected,"segments":segments,"windows":windows,"source_audio":id,"transcription_only":true,"environment_sounds_understood":false,"timing":"window_alignment_not_word_alignment","model":status}),
        )
    }
    pub(super) fn add_speech_to_media(&self, result: &mut Value, params: &Value) -> Result<()> {
        if params.get("transcribe") == Some(&json!(false)) {
            return Ok(());
        }
        let prepared = result
            .pointer("/audio/id")
            .and_then(Value::as_str)
            .map(|id| {
                (
                    id.to_owned(),
                    result
                        .get("actual_start_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                )
            })
            .or_else(|| {
                result
                    .pointer("/window/audio/resource/id")
                    .and_then(Value::as_str)
                    .map(|id| {
                        (
                            id.to_owned(),
                            result
                                .pointer("/window/audio/actual_start_ms")
                                .and_then(Value::as_u64)
                                .unwrap_or(0),
                        )
                    })
            });
        if let Some((id, start)) = prepared {
            let language = params
                .get("speech_language")
                .or_else(|| params.get("language"))
                .and_then(Value::as_str)
                .map(normalize_speech_language)
                .transpose()?
                .flatten();
            let transcript = self.transcribe_prepared_audio(&id, start, language.as_deref());
            result["speech"] = match transcript {
                Ok(v) => v,
                Err(error) => {
                    json!({"status":"failed","available":false,"error":error,"environment_sounds_understood":false})
                }
            };
        }
        Ok(())
    }
}
