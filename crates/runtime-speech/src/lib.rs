// SPDX-License-Identifier: MIT
//! On-demand, pure Rust CPU Whisper for bounded media windows. No background
//! service, automatic downloads, account credentials, Python or native codec.
mod model_files;
use candle_core::{Device, IndexOp, Tensor};
use candle_transformers::models::whisper::{self as whisper, quantized_model::Whisper};
pub use model_files::{ARTIFACTS, Artifact, ArtifactStatus, ModelStatus, install, status};
use serde::Serialize;
use std::path::Path;
use tokenizers::Tokenizer;
pub type Result<T> = std::result::Result<T, String>;
pub const MAX_WINDOW_MS: u64 = 10_000;
const MAX_GENERATED: usize = 128;
const LANGUAGES: &str = "en zh de es ru ko fr ja pt tr pl ca nl ar sv it id hi fi vi he uk el ms cs ro da hu ta no th ur hr bg lt la mi ml cy sk te fa lv bn sr az sl kn et mk br eu is hy ne mn bs kk sq sw gl mr pa si km sn yo so af oc ka be tg sd gu am yi lo uz fo ht ps tk nn mt sa lb my bo tl mg as tt haw ln ha ba jw su";
#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    pub model: &'static str,
    pub revision: &'static str,
    pub quantization: &'static str,
    pub text: String,
    pub language: Option<String>,
    pub language_confidence: Option<f64>,
    pub segments: Vec<Segment>,
    pub audio_ms: u64,
    pub input_sample_rate: u32,
    pub input_channels: u16,
    pub resampled_sample_rate: u32,
    pub avg_logprob: Option<f64>,
    pub no_speech_probability: Option<f64>,
    pub no_speech_detected: bool,
    pub truncated: bool,
    pub transcription_only: bool,
    pub environment_sounds_understood: bool,
    pub alignment: &'static str,
}
#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}
fn fail(_: impl std::fmt::Display) -> String {
    "speech inference failed".into()
}
fn token(t: &Tokenizer, text: &str) -> Result<u32> {
    t.token_to_id(text)
        .ok_or_else(|| "speech tokenizer missing required token".into())
}
/// Downmixes and bandlimits through a 32-tap, 1024-phase windowed-sinc kernel.
/// Input is interleaved normalized PCM; output is mono 16 kHz. It never asks a
/// system media process to decode or resample data.
pub fn mono_16khz(input: &[f32], rate: u32, channels: u16) -> Result<Vec<f32>> {
    if !(8000..=96000).contains(&rate)
        || !(1..=2).contains(&channels)
        || input.is_empty()
        || input.len() % usize::from(channels) != 0
    {
        return Err("invalid speech PCM rate/channels/frames".into());
    }
    let frames = input.len() / usize::from(channels);
    if frames as u64 * 1000 > u64::from(rate) * MAX_WINDOW_MS {
        return Err("speech window exceeds10 seconds; process bounded windows sequentially".into());
    }
    if input.iter().any(|s| !s.is_finite()) {
        return Err("speech PCM must be finite".into());
    }
    let mono = input
        .chunks_exact(usize::from(channels))
        .map(|samples| {
            samples.iter().map(|s| s.clamp(-1.0, 1.0)).sum::<f32>() / f32::from(channels)
        })
        .collect::<Vec<_>>();
    if rate == 16000 {
        return Ok(mono);
    }
    let len = usize::try_from((frames as u64 * 16000).div_ceil(u64::from(rate)))
        .map_err(|_| "resampled speech length overflow")?;
    let cutoff = (16000.0 / f64::from(rate)).min(1.0) * 0.94;
    let mut kernels = vec![0f32; 1024 * 32];
    for phase in 0..1024 {
        let frac = phase as f64 / 1024.0;
        let mut sum = 0f64;
        for tap in 0..32 {
            let x = tap as f64 - 15.0 - frac;
            let sinc = if x.abs() < 1e-12 {
                cutoff
            } else {
                (std::f64::consts::PI * x * cutoff).sin() / (std::f64::consts::PI * x)
            };
            let window = 0.5 + 0.5 * (std::f64::consts::PI * x / 16.0).cos();
            let weight = sinc * window;
            kernels[phase * 32 + tap] = weight as f32;
            sum += weight;
        }
        for tap in 0..32 {
            kernels[phase * 32 + tap] /= sum as f32;
        }
    }
    let mut out = Vec::with_capacity(len);
    for n in 0..len {
        let numerator = n as u64 * u64::from(rate);
        let index = (numerator / 16000) as i64;
        let phase = ((numerator % 16000) * 1024 / 16000) as usize;
        let mut sample = 0f32;
        for tap in 0..32 {
            let at = index + tap as i64 - 15;
            if at >= 0 && (at as usize) < mono.len() {
                sample += mono[at as usize] * kernels[phase * 32 + tap];
            }
        }
        out.push(sample.clamp(-1.0, 1.0));
    }
    Ok(out)
}
fn empty(rate: u32, channels: u16, audio_ms: u64) -> Transcript {
    Transcript {
        model: model_files::MODEL,
        revision: model_files::REVISION,
        quantization: "q80",
        text: String::new(),
        language: None,
        language_confidence: None,
        segments: vec![],
        audio_ms,
        input_sample_rate: rate,
        input_channels: channels,
        resampled_sample_rate: 16000,
        avg_logprob: None,
        no_speech_probability: None,
        no_speech_detected: true,
        truncated: false,
        transcription_only: true,
        environment_sounds_understood: false,
        alignment: "window_only_no_word_timestamps",
    }
}
fn probabilities(logits: &[f32]) -> Result<Vec<f64>> {
    let max = logits
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .reduce(f32::max)
        .ok_or("speech decoder has no finite logits")?;
    let weights = logits
        .iter()
        .map(|v| {
            if v.is_finite() {
                f64::from(*v - max).exp()
            } else {
                0.0
            }
        })
        .collect::<Vec<_>>();
    let total = weights.iter().sum::<f64>();
    if !total.is_finite() || total <= 0.0 {
        return Err("speech decoder probability error".into());
    }
    Ok(weights.into_iter().map(|v| v / total).collect())
}
/// Does not perform network requests. Install model artifacts explicitly first.
/// Model/tokenizer/tensors are scoped to this call and released before return.
pub fn transcribe(
    dir: &Path,
    input: &[f32],
    rate: u32,
    channels: u16,
    language: Option<&str>,
) -> Result<Transcript> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .thread_name(|i| format!("runtime-speech-{i}"))
        .build()
        .map_err(|_| "cannot create bounded speech CPU pool")?;
    pool.install(|| transcribe_inner(dir, input, rate, channels, language))
}
fn transcribe_inner(
    dir: &Path,
    input: &[f32],
    rate: u32,
    channels: u16,
    language: Option<&str>,
) -> Result<Transcript> {
    if language.is_some_and(|l| !LANGUAGES.split_whitespace().any(|code| code == l)) {
        return Err("unsupported speech language hint".into());
    }
    let mono = mono_16khz(input, rate, channels)?;
    let audio_ms = ((input.len() / usize::from(channels)) as u64 * 1000).div_ceil(u64::from(rate));
    let paths = model_files::checked_paths(dir)?;
    let energy = mono
        .iter()
        .map(|v| f64::from(*v) * f64::from(*v))
        .sum::<f64>()
        / mono.len() as f64;
    if energy < 1e-10 {
        return Ok(empty(rate, channels, audio_ms));
    }
    let bytes = std::fs::read(&paths[1]).map_err(|_| "cannot read speech config")?;
    let config: whisper::Config =
        serde_json::from_slice(&bytes).map_err(|_| "invalid speech config")?;
    if config.num_mel_bins != 80
        || config.d_model != 384
        || config.encoder_layers != 4
        || config.decoder_layers != 4
        || config.vocab_size != 51865
        || config.max_source_positions != 1500
        || config.max_target_positions != 448
    {
        return Err("speech config is not the pinned tiny model".into());
    }
    let tokenizer = Tokenizer::from_file(&paths[2]).map_err(|_| "cannot load speech tokenizer")?;
    let filter_bytes = std::fs::read(&paths[3]).map_err(|_| "cannot read speech mel filters")?;
    let filters = filter_bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect::<Vec<_>>();
    let mel = whisper::audio::pcm_to_mel(&config, &mono, &filters);
    let full_frames = mel.len() / 80;
    // The pinned encoder accepts a variable sequence. Avoid evaluating its
    // default 30-second padded attention for a <=10-second media window.
    let frames = ((mono.len().div_ceil(160) + 200).min(1200).max(200) + 1) / 2 * 2;
    let mut compact = Vec::with_capacity(80 * frames);
    for band in 0..80 {
        compact.extend_from_slice(&mel[band * full_frames..band * full_frames + frames]);
    }
    drop(mel);
    drop(mono);
    let device = Device::Cpu;
    let mel = Tensor::from_vec(compact, (1, 80, frames), &device).map_err(fail)?;
    let vb = candle_transformers::quantized_var_builder::VarBuilder::from_gguf(&paths[0], &device)
        .map_err(fail)?;
    let mut model = Whisper::load(&vb, config.clone()).map_err(fail)?;
    drop(vb);
    let features = model.encoder.forward(&mel, true).map_err(fail)?;
    drop(mel);
    let sot = token(&tokenizer, whisper::SOT_TOKEN)?;
    let eot = token(&tokenizer, whisper::EOT_TOKEN)?;
    let no_speech = whisper::NO_SPEECH_TOKENS
        .iter()
        .find_map(|s| tokenizer.token_to_id(s))
        .ok_or("speech tokenizer missing no-speech token")?;
    let initial = Tensor::new(&[sot], &device)
        .and_then(|x| x.unsqueeze(0))
        .map_err(fail)?;
    let initial = model
        .decoder
        .forward(&initial, &features, true)
        .map_err(fail)?;
    let first = model
        .decoder
        .final_linear(&initial)
        .and_then(|t| t.i(0))
        .and_then(|t| t.i(0))
        .and_then(|t| t.to_vec1::<f32>())
        .map_err(fail)?;
    let initial_prob = probabilities(&first)?;
    let no_speech_prob = *initial_prob
        .get(no_speech as usize)
        .ok_or("invalid no-speech token")?;
    let (language, language_confidence) = if let Some(l) = language {
        (l.to_owned(), None)
    } else {
        let candidates = LANGUAGES
            .split_whitespace()
            .map(|l| Ok((l, token(&tokenizer, &format!("<|{l}|>"))?)))
            .collect::<Result<Vec<_>>>()?;
        let logits = candidates
            .iter()
            .map(|(_, id)| {
                first
                    .get(*id as usize)
                    .copied()
                    .ok_or_else(|| String::from("invalid language token"))
            })
            .collect::<Result<Vec<_>>>()?;
        let probs = probabilities(&logits)?;
        let best = probs
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(n, _)| n)
            .ok_or("language detection failed")?;
        (candidates[best].0.to_owned(), Some(probs[best]))
    };
    let mut tokens = vec![
        sot,
        token(&tokenizer, &format!("<|{language}|>"))?,
        token(&tokenizer, whisper::TRANSCRIBE_TOKEN)?,
        token(&tokenizer, whisper::NO_TIMESTAMPS_TOKEN)?,
    ];
    let prefix = tokens.len();
    let mut sum = 0f64;
    let mut ended = false;
    for step in 0..MAX_GENERATED {
        let x = Tensor::new(tokens.as_slice(), &device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(fail)?;
        let y = model
            .decoder
            .forward(&x, &features, step == 0)
            .map_err(fail)?;
        let (_, len, _) = y.dims3().map_err(fail)?;
        let last = y.i((..1, len - 1..)).map_err(fail)?;
        let mut logits = model
            .decoder
            .final_linear(&last)
            .and_then(|t| t.i(0))
            .and_then(|t| t.i(0))
            .and_then(|t| t.to_vec1::<f32>())
            .map_err(fail)?;
        for id in &config.suppress_tokens {
            if let Some(v) = logits.get_mut(*id as usize) {
                *v = f32::NEG_INFINITY
            }
        }
        for (id, v) in logits.iter_mut().enumerate() {
            if id > eot as usize {
                *v = f32::NEG_INFINITY
            }
        }
        if step == 0 {
            for id in [220, eot] {
                if let Some(v) = logits.get_mut(id as usize) {
                    *v = f32::NEG_INFINITY
                }
            }
        }
        let next = logits
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_finite())
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(n, _)| n as u32)
            .ok_or("speech decoder empty token selection")?;
        let probs = probabilities(&logits)?;
        sum += probs[next as usize].max(1e-300).ln();
        tokens.push(next);
        if next == eot {
            ended = true;
            break;
        }
    }
    let generated = tokens.len() - prefix;
    let avg = sum / generated.max(1) as f64;
    let no_speech_detected =
        no_speech_prob > whisper::NO_SPEECH_THRESHOLD && avg < whisper::LOGPROB_THRESHOLD;
    let text = if no_speech_detected {
        String::new()
    } else {
        tokenizer
            .decode(&tokens[prefix..], true)
            .map_err(|_| "cannot decode speech tokens")?
            .trim()
            .to_owned()
    };
    let segments = if text.is_empty() {
        vec![]
    } else {
        vec![Segment {
            start_ms: 0,
            end_ms: audio_ms,
            text: text.clone(),
        }]
    };
    Ok(Transcript {
        model: model_files::MODEL,
        revision: model_files::REVISION,
        quantization: "q80",
        text,
        language: Some(language),
        language_confidence,
        segments,
        audio_ms,
        input_sample_rate: rate,
        input_channels: channels,
        resampled_sample_rate: 16000,
        avg_logprob: Some(avg),
        no_speech_probability: Some(no_speech_prob),
        no_speech_detected,
        truncated: !ended,
        transcription_only: true,
        environment_sounds_understood: false,
        alignment: "window_only_no_word_timestamps",
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stereo_resampling_preserves_a_tone_and_limits_output() {
        let input = (0..4800)
            .flat_map(|i| {
                let v = (i as f32 * std::f32::consts::TAU * 440.0 / 48000.0).sin() * 0.5;
                [v, v]
            })
            .collect::<Vec<_>>();
        let out = mono_16khz(&input, 48000, 2).unwrap();
        assert_eq!(out.len(), 1600);
        let rms = (out[32..1568].iter().map(|s| s * s).sum::<f32>() / 1536.0).sqrt();
        assert!((rms - 0.3535).abs() < 0.02);
        assert!(out.iter().all(|v| v.is_finite()));
    }
    #[test]
    fn rejects_unbounded_or_invalid_pcm_before_model_access() {
        assert!(mono_16khz(&vec![0.0; 160001], 16000, 1).is_err());
        assert!(mono_16khz(&[f32::NAN], 16000, 1).is_err());
        assert!(mono_16khz(&[0.0], 48000, 2).is_err());
        assert!(mono_16khz(&[0.0], 1000000, 1).is_err());
        assert!(
            transcribe(
                Path::new("/missing-test-model"),
                &[0.0],
                16000,
                1,
                Some("../zh")
            )
            .is_err()
        );
    }
    #[test]
    fn downmix_does_not_treat_stereo_as_twice_the_duration() {
        let out = mono_16khz(&[0.5, -0.5, 0.5, -0.5], 16000, 2).unwrap();
        assert_eq!(out, vec![0.0, 0.0]);
    }
}
