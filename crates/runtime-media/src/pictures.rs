// SPDX-License-Identifier: MIT
use crate::{BoundedWriter, Limits, MediaError, Result};
use image::{ImageDecoder, ImageEncoder, ImageReader};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{File, OpenOptions},
    io::{BufReader, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageProbe {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub decoded_bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedImage {
    pub source: ImageProbe,
    pub width: u32,
    pub height: u32,
    pub path: PathBuf,
    pub mime: String,
    pub bytes: u64,
    pub source_region: crate::view::PixelRegion,
    pub oriented_source_width: u32,
    pub oriented_source_height: u32,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum OutputFormat {
    Jpeg,
    Png,
}
fn decoder(path: &Path, limits: &Limits) -> Result<(ImageProbe, impl ImageDecoder)> {
    let input_bytes = limits.check_file(path)?;
    if input_bytes > limits.max_decoded_bytes {
        return Err(MediaError::Limit("compressed image bytes".into()));
    }
    let mut reader = ImageReader::new(BufReader::new(File::open(path)?))
        .with_guessed_format()
        .map_err(MediaError::Io)?;
    let format = reader
        .format()
        .ok_or_else(|| MediaError::Unsupported("unknown image signature".into()))?;
    let mut l = image::Limits::default();
    l.max_image_width = Some(limits.max_dimension);
    l.max_image_height = Some(limits.max_dimension);
    l.max_alloc = Some(limits.max_decoded_bytes);
    reader.limits(l.clone());
    let mut dec = reader
        .into_decoder()
        .map_err(|e| MediaError::Decode(e.to_string()))?;
    let (width, height) = dec.dimensions();
    let bytes = dec.total_bytes();
    limits.check_image(width, height, bytes)?;
    dec.set_limits(l)
        .map_err(|e| MediaError::Limit(e.to_string()))?;
    Ok((
        ImageProbe {
            width,
            height,
            format: format!("{format:?}"),
            decoded_bytes: bytes,
        },
        dec,
    ))
}
pub fn probe_image(path: &Path, limits: &Limits) -> Result<ImageProbe> {
    decoder(path, limits).map(|(p, _)| p)
}
pub fn prepare_image(
    input: &Path,
    output: &Path,
    max_edge: u32,
    format: OutputFormat,
    limits: &Limits,
) -> Result<PreparedImage> {
    prepare_image_view(
        input,
        output,
        max_edge,
        crate::view::ViewOptions {
            region: None,
            format,
        },
        limits,
    )
}
pub fn prepare_image_view(
    input: &Path,
    output: &Path,
    max_edge: u32,
    view: crate::view::ViewOptions,
    limits: &Limits,
) -> Result<PreparedImage> {
    if max_edge == 0 || max_edge > limits.max_dimension {
        return Err(MediaError::Limit("image resize edge".into()));
    }
    let (source, mut dec) = decoder(input, limits)?;
    let orientation = dec
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image =
        image::DynamicImage::from_decoder(dec).map_err(|e| MediaError::Decode(e.to_string()))?;
    image.apply_orientation(orientation);
    let (oriented_source_width, oriented_source_height) = (image.width(), image.height());
    let source_region = view.pixels(image.width(), image.height())?;
    if view.region.is_some() {
        image = image.crop_imm(
            source_region.x,
            source_region.y,
            source_region.width,
            source_region.height,
        );
    }
    let edge = max_edge.min(image.width().max(image.height()));
    let image = image.thumbnail(edge, edge);
    write_image(&image, output, view.format, limits).map(|(bytes, mime)| PreparedImage {
        source,
        width: image.width(),
        height: image.height(),
        path: output.into(),
        mime,
        bytes,
        source_region,
        oriented_source_width,
        oriented_source_height,
    })
}
pub(crate) fn write_image(
    image: &image::DynamicImage,
    output: &Path,
    format: OutputFormat,
    limits: &Limits,
) -> Result<(u64, String)> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(output)?;
    let mut writer = BoundedWriter {
        inner: file,
        bytes: 0,
        limit: limits.max_output_bytes,
    };
    let result = match format {
        OutputFormat::Jpeg => {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 82).encode_image(image)
        }
        OutputFormat::Png => {
            let pixels = image.to_rgba8();
            image::codecs::png::PngEncoder::new(&mut writer).write_image(
                pixels.as_raw(),
                pixels.width(),
                pixels.height(),
                image::ExtendedColorType::Rgba8,
            )
        }
    };
    if let Err(e) = result {
        drop(writer);
        let _ = std::fs::remove_file(output);
        return Err(MediaError::Decode(e.to_string()));
    }
    writer.flush()?;
    Ok((
        writer.bytes,
        match format {
            OutputFormat::Jpeg => "image/jpeg",
            OutputFormat::Png => "image/png",
        }
        .into(),
    ))
}
