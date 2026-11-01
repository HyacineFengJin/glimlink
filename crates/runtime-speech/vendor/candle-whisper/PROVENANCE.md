# Candle Whisper model adaptation

`quantized_model.rs` derives from Hugging Face Candle 0.9.1,
`candle-transformers/src/models/whisper/quantized_model.rs`, packaged Git revision
`cd96fa80da255e34f7b16b4ff98b6a31d557201b`. The original implementation is retained
under **MIT OR Apache-2.0**, not reclassified as original Glimlink code.

Changes: imports refer to the existing Candle dependencies; token embedding and
the tied vocabulary projection use Glimlink's separate `token_embedding.rs`.
The rest of the encoder/decoder architecture remains upstream code. The compact
projection dequantizes bounded tiles and uses f32 multiplication, preserving the
existing Q8_0 weights without introducing another activation quantization.

`LICENSE-MIT` and `LICENSE-APACHE` retain the upstream license texts, fetched from
the upstream repository on 2026-10-31. Candle's packaged Cargo metadata declares
the same dual license. Source: https://github.com/huggingface/candle

Glimlink's new `src/token_embedding.rs` implementation is MIT. It uses public
Candle tensor interfaces and does not copy the upstream model implementation.
