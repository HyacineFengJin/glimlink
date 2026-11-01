// SPDX-License-Identifier: MIT
//! Keep the pinned Q8_0 vocabulary quantized. Only token rows or a bounded
//! projection tile become f32; the full 51,865 × 384 f32 table never exists.
use candle_core::{
    Device, Module, Result, Tensor,
    quantized::{GgmlDType, QTensor, ggml_file::qtensor_from_ggml},
};
use candle_transformers::quantized_var_builder::VarBuilder;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct TokenEmbedding {
    weight: Arc<QTensor>,
    rows: usize,
    width: usize,
}
impl TokenEmbedding {
    pub fn new(rows: usize, width: usize, vb: VarBuilder) -> Result<Self> {
        let weight = vb.get((rows, width), "weight")?;
        if weight.dtype() != GgmlDType::Q8_0
            || width % 32 != 0
            || !matches!(weight.device(), Device::Cpu)
        {
            candle_core::bail!("compact vocabulary requires CPU Q8_0 rows");
        }
        Ok(Self {
            weight,
            rows,
            width,
        })
    }
    fn tile(&self, start: usize, rows: usize) -> Result<Tensor> {
        if rows == 0 || start.checked_add(rows).is_none_or(|end| end > self.rows) {
            candle_core::bail!("vocabulary row outside pinned table");
        }
        let stride = self.width / 32 * 34;
        let bytes = self.weight.data()?;
        let bytes = bytes
            .get(start * stride..(start + rows) * stride)
            .ok_or_else(|| candle_core::Error::Msg("invalid Q8_0 row storage".into()))?;
        qtensor_from_ggml(GgmlDType::Q8_0, bytes, vec![rows, self.width], &Device::Cpu)?
            .dequantize(&Device::Cpu)
    }
    pub fn project(&self, x: &Tensor) -> Result<Tensor> {
        let (batch, tokens, width) = x.dims3()?;
        if width != self.width || batch == 0 || tokens == 0 {
            candle_core::bail!("invalid vocabulary projection shape");
        }
        let flat = x.reshape((batch * tokens, width))?;
        let mut pieces = Vec::with_capacity(self.rows.div_ceil(1024));
        for start in (0..self.rows).step_by(1024) {
            let weight = self.tile(start, (self.rows - start).min(1024))?;
            pieces.push(flat.matmul(&weight.t()?)?);
        }
        Tensor::cat(&pieces, 1)?.reshape((batch, tokens, self.rows))
    }
}
impl Module for TokenEmbedding {
    fn forward(&self, ids: &Tensor) -> Result<Tensor> {
        let (batch, tokens) = ids.dims2()?;
        let mut pieces = Vec::with_capacity(batch * tokens);
        for row in ids.to_vec2::<u32>()? {
            for id in row {
                pieces.push(self.tile(id as usize, 1)?);
            }
        }
        Tensor::cat(&pieces, 0)?.reshape((batch, tokens, self.width))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tiled_logits_and_selected_rows_match_full_f32_reference() -> Result<()> {
        let values = (0..2051 * 64)
            .map(|i| ((i * 17 % 101) as f32 - 50.) / 37.)
            .collect::<Vec<_>>();
        let full = Tensor::from_vec(values, (2051, 64), &Device::Cpu)?;
        let weight = Arc::new(QTensor::quantize(&full, GgmlDType::Q8_0)?);
        let expected = weight.dequantize(&Device::Cpu)?;
        let embedding = TokenEmbedding {
            weight,
            rows: 2051,
            width: 64,
        };
        let ids = Tensor::new(&[[2049u32, 0, 1025]], &Device::Cpu)?;
        let actual = embedding.forward(&ids)?.flatten_all()?.to_vec1::<f32>()?;
        let rows = expected.to_vec2::<f32>()?;
        assert_eq!(
            actual,
            [rows[2049].clone(), rows[0].clone(), rows[1025].clone()].concat()
        );
        let x = Tensor::from_vec(
            (0..128).map(|i| (i % 7) as f32 / 13.).collect::<Vec<_>>(),
            (1, 2, 64),
            &Device::Cpu,
        )?;
        let actual = embedding.project(&x)?.flatten_all()?.to_vec1::<f32>()?;
        let reference = x
            .reshape((2, 64))?
            .matmul(&expected.t()?)?
            .flatten_all()?
            .to_vec1::<f32>()?;
        assert!(
            actual
                .iter()
                .zip(reference)
                .all(|(a, b)| (a - b).abs() < 0.0001)
        );
        assert!(embedding.tile(2051, 1).is_err());
        Ok(())
    }
}
