// SPDX-License-Identifier: MIT
//! Offline tests of the default AI task surface. Every reference and cached
//! page is synthetic and lives under this test's temporary state directory.
use agent_runtime::{
    app::Runtime,
    outbox::{Operation, Outbox, Status},
    private_state::{PrivateLocators, account_directory},
};
use base64::Engine;
use runtime_core::{
    Platform, Scope,
    content::ResourceRef,
    resources::{Budget, ResourceStore},
};
use runtime_engine::{ToolCall, ToolContent, ToolResult};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Lab(PathBuf);
impl Lab {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "client-workflow-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn state(&self) -> PathBuf {
        self.0.join("state")
    }
    fn stores(&self) -> (ResourceStore, PrivateLocators) {
        (
            ResourceStore::open(&self.state().join("resources"), Budget::default()).unwrap(),
            PrivateLocators::open(&self.state()).unwrap(),
        )
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn scope() -> Scope {
    Scope {
        platform: Platform::Bilibili,
        account: "fixture-owner".into(),
    }
}
fn call(runtime: &mut Runtime, name: &str, args: Value) -> ToolResult {
    runtime.dispatch_engine_tool(&ToolCall {
        thread_id: "offline-client".into(),
        turn_id: "fixture-turn".into(),
        call_id: format!("{name}-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        tool: name.into(),
        arguments: args,
    })
}
fn data(result: &ToolResult) -> Value {
    let text = result
        .content_items
        .iter()
        .find_map(|item| match item {
            ToolContent::InputText { text } => Some(text),
            _ => None,
        })
        .expect("task result text");
    serde_json::from_str(text).expect("task result must be structured JSON")
}
fn image_count(result: &ToolResult) -> usize {
    result
        .content_items
        .iter()
        .filter(|item| matches!(item, ToolContent::InputImage { .. }))
        .count()
}
fn resource(store: &ResourceStore, scope: &Scope, path: &Path, mime: &str) -> ResourceRef {
    store
        .import(
            scope,
            "workflow-fixture",
            mime,
            &mut File::open(path).unwrap(),
        )
        .unwrap()
}
fn png(path: &Path) {
    image::DynamicImage::new_rgb8(16, 16)
        .save_with_format(path, image::ImageFormat::Png)
        .unwrap();
}
fn gif(path: &Path) {
    let mut encoder = image::codecs::gif::GifEncoder::new(File::create(path).unwrap());
    for colour in [[255, 0, 0, 255], [0, 255, 0, 255]] {
        encoder
            .encode_frame(image::Frame::from_parts(
                image::RgbaImage::from_pixel(8, 8, image::Rgba(colour)),
                0,
                0,
                image::Delay::from_numer_denom_ms(100, 1),
            ))
            .unwrap();
    }
}

#[test]
fn comment_media_can_be_viewed_and_used_for_an_idempotent_reply_without_id_transport() {
    let lab = Lab::new();
    let (store, locators) = lab.stores();
    let owner = scope();
    let picture = lab.0.join("comment.png");
    png(&picture);
    let image = resource(&store, &owner, &picture, "image/png");
    // The source URI is deliberately forbidden. A cache miss cannot accidentally
    // turn this workflow test into a real HTTP request.
    let locator = platform_bilibili::MediaResource {
        url: "https://example.invalid/never-fetch.png".into(),
        backup_urls: vec![],
        expected_bytes: None,
        mime_type: None,
    };
    let media = locators
        .put(&owner, "bilibili-comment-media", &locator)
        .unwrap();
    locators
        .put(
            &owner,
            "task-media-cache",
            &json!({"schema":1,"asset":media,"kind":"bilibili-comment-media",
        "resource":image,"created_at":now()}),
        )
        .unwrap();
    let video = json!({"kind":"video","video":"BV17x411w7KC"});
    let video_ref = locators.put(&owner, "task-target", &video).unwrap();
    let page = json!({"comments":[{"rpid":7,"oid":170001,"root":0,"parent":0,"author":{"mid":42,"name":"fixture"},
        "message":"a picture reply","pictures":[{"asset":media,"kind":"image","fetched":false}]}]});
    let page = store
        .import(
            &owner,
            "cached-comment-page",
            "application/json",
            &mut serde_json::to_vec(&page).unwrap().as_slice(),
        )
        .unwrap();
    let continuation=locators.put(&owner,"task-discussion",&json!({"schema":1,"scope":owner,
        "target_ref":video_ref,"target":video,"order":"hot","position":{"kind":"cursor","value":null},
        "seen":["7"],"pending":[{"id":"7","resource":page.id,"pointer":"/comments/0"}],
        "context":{},"context_ref":null,"stopped":"platform_end","platform_has_more":false,"total_pages_read":1})).unwrap();
    let reply = json!({"kind":"bili_comment","video":"BV17x411w7KC","root":7,"parent":7});
    let message = "看到了喵[doge]";
    let mut operation = Operation::new(
        "cached-reply",
        owner.clone(),
        "42".into(),
        json!({"aid":170001,"bvid":"BV17x411w7KC","title":"fixture","owner_mid":42}),
        message,
    )
    .unwrap();
    operation.request = Some(
        json!({"root":7,"parent":7,"image_resource":image.id,"image_ref":media,"task_target":reply}),
    );
    let outbox = Outbox::open(&lab.state()).unwrap();
    outbox.insert(&operation).unwrap();
    outbox.begin_attempt(&mut operation).unwrap();
    operation.status = Status::Sent;
    outbox.finish(&operation).unwrap();
    operation.status = Status::Verified;
    operation.verification = Some(json!({"root":7,"parent":7,"message":message,"author_mid":42}));
    outbox.finish(&operation).unwrap();
    drop(store);
    drop(locators);
    let mut runtime = Runtime::open(&lab.state(), "public", None).unwrap();
    let discussion = call(
        &mut runtime,
        "social_discussion",
        json!({"continuation":continuation,"limit":1}),
    );
    assert!(discussion.success);
    let discussion = data(&discussion);
    assert_eq!(discussion["provider_pages_read"], 0);
    assert_eq!(discussion["media_downloaded"], false);
    let comment = &discussion["items"][0];
    let media_ref = comment["pictures"][0]["media_ref"].as_str().unwrap();
    let viewed = call(&mut runtime, "media_read", json!({"reference":media_ref}));
    assert!(viewed.success);
    assert_eq!(image_count(&viewed), 1);
    assert_eq!(data(&viewed)["download"]["performed"], false);
    let sent = call(
        &mut runtime,
        "social_send",
        json!({"target_ref":comment["target_ref"],"operation_id":"cached-reply",
        "message":message,"image_ref":media_ref}),
    );
    assert!(sent.success);
    assert_eq!(data(&sent)["status"], "verified");
    assert_eq!(data(&sent)["request"]["image_ref"], media_ref);
    assert_eq!(runtime.scope(Platform::Bilibili).account, "public");
    let saved = Outbox::open(&lab.state())
        .unwrap()
        .load("cached-reply", &owner)
        .unwrap();
    assert_eq!(saved.attempt_started_at, operation.attempt_started_at);
}

#[test]
fn short_animation_is_one_task_with_real_frames_and_an_explicit_single_pass_end() {
    let lab = Lab::new();
    let (store, _) = lab.stores();
    let file = lab.0.join("short.gif");
    gif(&file);
    let image = resource(&store, &scope(), &file, "application/octet-stream");
    drop(store);
    let mut runtime = Runtime::open(&lab.state(), "public", None).unwrap();
    let result = call(
        &mut runtime,
        "media_read",
        json!({"reference":image.id,"duration_ms":1000,"interval_ms":100}),
    );
    assert!(result.success);
    assert_eq!(image_count(&result), 2);
    let value = data(&result);
    assert_eq!(value["media_kind"], "gif");
    assert_eq!(value["window"]["end_of_animation"], true);
    assert_eq!(value["window"]["decoded_until_ms"], 200);
    assert_eq!(value["window"]["requested_end_ms"], 1000);
    assert_ne!(value["full_video_watched"], true);
    assert_eq!(value["audio_understanding"], false);
}

#[test]
fn advancing_past_media_end_is_terminal_not_a_claim_that_visual_input_was_prepared() {
    let lab = Lab::new();
    let (store, _) = lab.stores();
    let file = lab.0.join("short.gif");
    gif(&file);
    let image = resource(&store, &scope(), &file, "image/gif");
    drop(store);
    let mut runtime = Runtime::open(&lab.state(), "public", None).unwrap();
    let result = call(
        &mut runtime,
        "media_read",
        json!({"reference":image.id,"start_ms":250,"duration_ms":1000}),
    );
    let value = data(&result);
    assert_eq!(image_count(&result), 0);
    assert_eq!(value["status"], "end_of_media");
    assert_eq!(value["input_available"], false);
    assert_ne!(value["prepared"], true);
    assert_ne!(value["model_observed"], true);
}

#[test]
fn unsupported_sound_reports_structured_failure_without_fabricating_observation() {
    let lab = Lab::new();
    let (store, _) = lab.stores();
    let file = lab.0.join("voice.silk");
    fs::write(&file, b"#!SILK_V3\nnot-a-supported-decoder").unwrap();
    let audio = resource(&store, &scope(), &file, "audio/x-silk");
    drop(store);
    let mut runtime = Runtime::open(&lab.state(), "public", None).unwrap();
    let result = call(&mut runtime, "media_read", json!({"reference":audio.id}));
    assert!(!result.success);
    assert_eq!(image_count(&result), 0);
    let value = data(&result);
    assert_eq!(value["status"], "failed");
    assert!(value["error"].as_str().unwrap().contains("SILK"));
    assert_ne!(value["prepared"], true);
    assert_ne!(value["model_observed"], true);
    assert_ne!(value["audio_understanding"], true);
}

#[test]
fn an_expired_asset_cache_cannot_pass_old_pixels_as_a_new_successful_read() {
    let lab = Lab::new();
    let (store, locators) = lab.stores();
    let owner = scope();
    let file = lab.0.join("old-picture.png");
    png(&file);
    let picture = resource(&store, &owner, &file, "image/png");
    let source = platform_bilibili::MediaResource {
        url: "https://example.invalid/refresh-is-forbidden.png".into(),
        backup_urls: vec![],
        expected_bytes: None,
        mime_type: None,
    };
    let asset = locators
        .put(&owner, "bilibili-comment-media", &source)
        .unwrap();
    locators
        .put(
            &owner,
            "task-media-cache",
            &json!({"schema":1,"asset":asset,"kind":"bilibili-comment-media",
        "resource":picture,"created_at":now()-3600}),
        )
        .unwrap();
    drop(store);
    drop(locators);
    let mut runtime = Runtime::open(&lab.state(), "public", None).unwrap();
    let result = call(&mut runtime, "media_read", json!({"reference":asset}));
    // The old image exists and decodes, but an expired cache requires refresh.
    // Its deliberately forbidden URI is rejected locally before any HTTP.
    assert!(!result.success);
    assert_eq!(image_count(&result), 0);
    let value = data(&result);
    assert_eq!(value["status"], "failed");
    assert_ne!(value["prepared"], true);
    assert_eq!(runtime.scope(Platform::Bilibili).account, "public");
}

#[test]
fn a_large_preview_reports_its_actual_wire_dimensions() {
    let lab = Lab::new();
    let (store, _) = lab.stores();
    let file = lab.0.join("detailed.png");
    let pixels = image::RgbImage::from_fn(1024, 1024, |x, y| {
        let mut value = x.wrapping_mul(0x9e3779b1) ^ y.wrapping_mul(0x85ebca6b);
        value ^= value >> 16;
        value = value.wrapping_mul(0x7feb352d);
        value ^= value >> 15;
        image::Rgb([value as u8, (value >> 8) as u8, (value >> 16) as u8])
    });
    image::DynamicImage::ImageRgb8(pixels)
        .save_with_format(&file, image::ImageFormat::Png)
        .unwrap();
    let picture = resource(&store, &scope(), &file, "image/png");
    drop(store);
    let mut runtime = Runtime::open(&lab.state(), "public", None).unwrap();
    let result = call(
        &mut runtime,
        "media_read",
        json!({"reference":picture.id,"quality":"preview"}),
    );
    assert!(result.success);
    assert_eq!(image_count(&result), 1);
    let image = result
        .content_items
        .iter()
        .find_map(|item| match item {
            ToolContent::InputImage { image_url } => Some(image_url),
            _ => None,
        })
        .unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(image.strip_prefix("data:image/jpeg;base64,").unwrap())
        .unwrap();
    assert!(bytes.len() <= 64 * 1024);
    assert!(bytes.starts_with(b"\xff\xd8\xff"));
    assert_eq!(data(&result)["engine_input"]["detail_preserved"], false);
    assert!(
        data(&result)["engine_input"]["attachment_sources"][0]["width"]
            .as_u64()
            .unwrap()
            <= 480
    );
    assert_eq!(
        data(&result)["engine_input"]["attachment_errors"],
        json!([])
    );
}

#[test]
fn cached_image_quality_request_reports_its_pixel_limit_without_claiming_hd() {
    let lab = Lab::new();
    let (store, _) = lab.stores();
    let file = lab.0.join("preview.png");
    png(&file);
    let original = resource(&store, &scope(), &file, "image/png");
    drop(store);
    let mut rt = Runtime::open(&lab.state(), "public", None).unwrap();
    let result = call(
        &mut rt,
        "media_read",
        json!({"reference":original.id,"quality":"1080p"}),
    );
    assert!(result.success);
    let v = data(&result);
    assert_eq!(v["source_quality"]["request_met"], false);
    assert_eq!(v["source_quality"]["stream_reacquired"], false);
    assert_eq!(v["source_quality"]["actual"]["width"], 16);
    assert_eq!(v["source_quality"]["selection"], "existing_image_pixels");
    assert!(
        v["source_quality"]["note"]
            .as_str()
            .unwrap()
            .contains("frame_ref")
    );
    assert_eq!(v["engine_input"]["attachment_sources"][0]["width"], 16);
    assert_eq!(image_count(&result), 1);
}

#[test]
fn detail_png_is_sent_losslessly_and_oversize_detail_is_not_silently_shrunk() {
    let lab = Lab::new();
    let (store, _) = lab.stores();
    let mut refs = Vec::new();
    for height in [100, 1024] {
        let file = lab.0.join(format!("detail-{height}.png"));
        let image = image::RgbImage::from_fn(1024, height, |x, y| {
            let mut n = x.wrapping_mul(0x9e3779b1) ^ y.wrapping_mul(0x85ebca6b);
            n ^= n >> 16;
            n = n.wrapping_mul(0x7feb352d);
            n ^= n >> 15;
            image::Rgb([n as u8, (n >> 8) as u8, (n >> 16) as u8])
        });
        image.save(&file).unwrap();
        refs.push(resource(&store, &scope(), &file, "image/png"));
    }
    drop(store);
    let mut rt = Runtime::open(&lab.state(), "public", None).unwrap();
    let result = call(
        &mut rt,
        "media_read",
        json!({"reference":refs[0].id,"quality":"1080p","format":"png"}),
    );
    assert!(result.success);
    let v = data(&result);
    assert_eq!(v["engine_input"]["attachment_sources"][0]["width"], 1024);
    assert_eq!(v["engine_input"]["attachment_sources"][0]["height"], 100);
    assert_eq!(
        v["engine_input"]["attachment_sources"][0]["resized_for_wire"],
        false
    );
    let url = result
        .content_items
        .iter()
        .find_map(|v| match v {
            ToolContent::InputImage { image_url } => Some(image_url),
            _ => None,
        })
        .unwrap();
    let raw = base64::engine::general_purpose::STANDARD
        .decode(url.strip_prefix("data:image/png;base64,").unwrap())
        .unwrap();
    assert!(raw.len() > 64 * 1024);
    assert_eq!(image::load_from_memory(&raw).unwrap().width(), 1024);
    let denied = call(
        &mut rt,
        "media_read",
        json!({"reference":refs[1].id,"quality":"1080p","format":"png"}),
    );
    assert!(!denied.success);
    assert_eq!(image_count(&denied), 0);
    assert_eq!(data(&denied)["status"], "input_unavailable");
}

#[test]
fn an_expired_content_capability_cannot_be_reinterpreted_under_the_only_other_account() {
    let lab = Lab::new();
    let (_, locators) = lab.stores();
    let reference = "00000000000000000000000000000007";
    let path = lab
        .state()
        .join("locators")
        .join(format!("{reference}.json"));
    let record = json!({"scope":scope(),"kind":"task-target","value":{"kind":"video","video":"BV17x411w7KC"},
        "created_at":now()-2*24*60*60});
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    serde_json::to_writer(file, &record).unwrap();
    drop(locators);
    // Synthetic invalid sessions ensure even a regression into alias fallback
    // fails locally before any provider request can be constructed.
    for provider in ["bilibili", "tieba"] {
        let directory = account_directory(&lab.state(), provider, "other-account").unwrap();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("session.json"))
            .unwrap();
        serde_json::to_writer(file, &json!({})).unwrap();
    }
    let mut runtime = Runtime::open(&lab.state(), "public", None).unwrap();
    let result = call(&mut runtime, "social_open", json!({"reference":reference}));
    assert!(!result.success);
    let value = data(&result);
    assert_eq!(value["status"], "failed");
    assert!(value["error"].as_str().unwrap().contains("expired"));
    assert_eq!(runtime.scope(Platform::Bilibili).account, "public");
    assert_eq!(runtime.scope(Platform::Tieba).account, "public");
}
