use runtime_media::{
    Limits, MediaError,
    audio::{probe_audio, segment_audio_wav},
    pictures::{OutputFormat, prepare_image, probe_image},
    video::{probe_video, sample_video_window},
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "runtime-media-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn path(&self, n: &str) -> PathBuf {
        self.0.join(n)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn actual_png_jpeg_webp_gif_decode_and_resize() {
    let dir = Temp::new();
    let rgb = image::RgbImage::from_fn(64, 32, |x, y| {
        image::Rgb([(x * 4) as u8, (y * 8) as u8, 128])
    });
    for format in [
        image::ImageFormat::Png,
        image::ImageFormat::Jpeg,
        image::ImageFormat::WebP,
        image::ImageFormat::Gif,
    ] {
        let path = dir.path(&format!("input.{format:?}"));
        image::DynamicImage::ImageRgb8(rgb.clone())
            .save_with_format(&path, format)
            .unwrap();
        let probe = probe_image(&path, &Limits::default()).unwrap();
        assert_eq!((probe.width, probe.height), (64, 32));
        let output = dir.path(&format!("{format:?}.jpg"));
        let result =
            prepare_image(&path, &output, 16, OutputFormat::Jpeg, &Limits::default()).unwrap();
        assert_eq!((result.width, result.height), (16, 8));
        assert!(result.bytes > 0);
        assert_eq!(image::open(output).unwrap().width(), 16);
        let native = prepare_image(
            &path,
            &dir.path(&format!("{format:?}-native.jpg")),
            4096,
            OutputFormat::Jpeg,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!((native.width, native.height), (64, 32));
    }
}

#[test]
fn region_reads_original_pixels_before_resize_and_reports_source_geometry() {
    use runtime_media::{
        pictures::prepare_image_view,
        view::{Region, ViewOptions},
    };
    let dir = Temp::new();
    let input = dir.path("source.png");
    image::RgbImage::from_fn(120, 80, |x, y| image::Rgb([x as u8, y as u8, 200]))
        .save(&input)
        .unwrap();
    let region = Region {
        x: 0.5,
        y: 0.,
        width: 0.5,
        height: 1.,
    };
    let output = dir.path("crop.png");
    let p = prepare_image_view(
        &input,
        &output,
        1920,
        ViewOptions {
            region: Some(region),
            format: OutputFormat::Png,
        },
        &Limits::default(),
    )
    .unwrap();
    assert_eq!((p.width, p.height), (60, 80));
    assert_eq!(p.source_region.x, 60);
    assert_eq!(
        image::open(output).unwrap().to_rgb8().get_pixel(0, 0).0,
        [60, 0, 200]
    );
    assert!(
        Region {
            x: 0.8,
            y: 0.,
            width: 0.5,
            height: 1.
        }
        .pixels(120, 80)
        .is_err()
    );
    assert!(
        Region {
            x: f64::NAN,
            y: 0.,
            width: 0.5,
            height: 1.
        }
        .validate()
        .is_err()
    );
}

#[test]
fn video_crop_matches_full_source_pixels_without_jpeg_or_upscaling() {
    use runtime_media::{
        video::sample_video_window_view,
        view::{Region, ViewOptions},
    };
    let dir = Temp::new();
    let path = dir.path("synthetic.mp4");
    fs::write(
        &path,
        include_bytes!("../../../tests/fixtures/watch-synthetic.mp4"),
    )
    .unwrap();
    let full = sample_video_window_view(
        &path,
        &dir.path("full"),
        0,
        1000,
        1000,
        1920,
        &ViewOptions {
            region: None,
            format: OutputFormat::Png,
        },
        &Limits::default(),
    )
    .unwrap();
    let crop = sample_video_window_view(
        &path,
        &dir.path("crop"),
        0,
        1000,
        1000,
        1920,
        &ViewOptions {
            region: Some(Region {
                x: 0.5,
                y: 0.5,
                width: 0.5,
                height: 0.5,
            }),
            format: OutputFormat::Png,
        },
        &Limits::default(),
    )
    .unwrap();
    let f = &full.samples[0];
    let c = &crop.samples[0];
    assert_eq!((c.width, c.height), (32, 32));
    assert_eq!(c.source_width, 64);
    assert_eq!(c.mime, "image/png");
    let expected = image::open(&f.path)
        .unwrap()
        .crop_imm(32, 32, 32, 32)
        .to_rgb8();
    assert_eq!(image::open(&c.path).unwrap().to_rgb8(), expected);
}
#[test]
fn image_limits_and_partial_output_cleanup() {
    let dir = Temp::new();
    let input = dir.path("image.png");
    image::DynamicImage::new_rgb8(128, 64).save(&input).unwrap();
    let mut limits = Limits::default();
    limits.max_pixels = 100;
    assert!(matches!(
        prepare_image(
            &input,
            &dir.path("oversize.png"),
            32,
            OutputFormat::Png,
            &limits
        ),
        Err(MediaError::Limit(_))
    ));
    limits = Limits::default();
    limits.max_output_bytes = 10;
    let output = dir.path("truncated.jpg");
    assert!(prepare_image(&input, &output, 32, OutputFormat::Jpeg, &limits).is_err());
    assert!(!output.exists());
    fs::write(dir.path("bad.png"), b"\x89PNG\r\n\x1a\ninvalid").unwrap();
    assert!(probe_image(&dir.path("bad.png"), &limits).is_err());
}
fn wav(rate: u32, seconds: u32) -> Vec<u8> {
    let count = rate * seconds;
    let bytes = count * 2;
    let mut out = Vec::new();
    out.extend(b"RIFF");
    out.extend((bytes + 36).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(rate.to_le_bytes());
    out.extend((rate * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(bytes.to_le_bytes());
    for i in 0..count {
        let sample = (((i * 97) % 20000) as i16) - 10000;
        out.extend(sample.to_le_bytes());
    }
    out
}
#[test]
fn wav_probe_seek_crop_and_permission() {
    let dir = Temp::new();
    let input = dir.path("source.wav");
    fs::write(&input, wav(8000, 3)).unwrap();
    let output = dir.path("segment.wav");
    let probe = probe_audio(&input, &Limits::default()).unwrap();
    assert_eq!(probe.sample_rate, 8000);
    assert_eq!(probe.channels, 1);
    let segment = segment_audio_wav(&input, &output, 1000, 1000, &Limits::default()).unwrap();
    assert_eq!(segment.frames, 8000);
    assert_eq!(segment.bytes, 16044);
    assert!(segment.complete);
    assert_eq!(segment.actual_start_ms, 1000);
    let original = fs::read(input).unwrap();
    let cut = fs::read(&output).unwrap();
    assert_eq!(&cut[44..], &original[44 + 16000..44 + 32000]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(output).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
#[test]
fn empty_native_voice_and_audio_bounds() {
    let dir = Temp::new();
    for signature in [b"#!SILK_V3".as_slice(), b"\x02#!SILK_V3"] {
        let path = dir.path("voice.bin");
        fs::write(&path, signature).unwrap();
        let probe = probe_audio(&path, &Limits::default()).unwrap();
        assert!(probe.codec.starts_with("silk-v3"));
        assert_eq!(probe.sample_rate, 24000);
        let output = dir.path("empty-silk.wav");
        assert!(segment_audio_wav(&path, &output, 0, 20, &Limits::default()).is_err());
        assert!(!output.exists());
    }
    let empty_amr = dir.path("empty.amr");
    fs::write(&empty_amr, b"#!AMR\n").unwrap();
    assert_eq!(
        probe_audio(&empty_amr, &Limits::default())
            .unwrap()
            .duration_ms,
        Some(0)
    );
    let path = dir.path("large.wav");
    fs::write(&path, wav(8000, 1)).unwrap();
    let mut limits = Limits::default();
    limits.max_output_bytes = 100;
    assert!(matches!(
        segment_audio_wav(&path, &dir.path("out.wav"), 0, 500, &limits),
        Err(MediaError::Limit(_))
    ));
    assert!(!dir.path("out.wav").exists());
}
fn atom(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = ((data.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend(kind);
    out.extend(data);
    out
}
#[test]
fn hostile_mp4_tables_and_truncated_boxes_are_rejected() {
    let dir = Temp::new();
    let limits = Limits::default();
    for data in [
        vec![0, 0, 0, 100, b'm', b'o', b'o', b'v'],
        atom(
            b"moov",
            &atom(b"stsz", &[0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255]),
        ),
        atom(
            b"moof",
            &atom(b"traf", &atom(b"trun", &[0, 0, 2, 0, 255, 255, 255, 255])),
        ),
    ] {
        let path = dir.path("bad.mp4");
        fs::write(&path, data).unwrap();
        assert!(probe_video(&path, &limits).is_err());
    }
    assert!(matches!(
        sample_video_window(
            &dir.path("bad.mp4"),
            &dir.path("frames"),
            u64::MAX,
            1,
            1000,
            480,
            &limits
        ),
        Err(MediaError::Limit(_))
    ));
}
