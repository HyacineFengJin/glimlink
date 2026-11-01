# runtime-media

### Source detail and region rereading

`media_watch` accepts `quality=preview|480p|720p|1080p` independently of output
`max_edge`. Bilibili requests that source quality and selects compatible AVC/AAC;
responses report actual source dimensions, available compatible variants and
`source_quality.request_met`. Account/source/decoder/download limits remain
explicit. Requesting a large output never enlarges a low-resolution source.

Every watched frame includes `frame_ref` and a ready `reread_with` call. For
example, `media_watch` with `action=inspect`, that `frame_ref`, `quality=1080p`,
and `region={x:0.5,y:0.7,width:0.5,height:0.3}` refetches/reuses the appropriate
source at the original frame time and reads its bottom-right area. `media_read`
also accepts this `frame_ref` directly. Regions use normalized coordinates of
the full source, not of an earlier crop. Ordinary still images support the same
region operation against their retained source pixels.

Inspection returns one sample within at most a one-second search window, with
the actual timestamp. `watch_ref` plus `start_ms` can inspect a session without
advancing its cursor or revision. Quality persists for normal start/next/seek;
an inspection override does not change session preferences. Stream caches bind
account, video, part and requested quality: a preview cache cannot satisfy HD.

Crops happen before resizing; video YUV-to-RGB conversion materializes only the
selected rectangle. H264 reference frames still require full source decoding.
Normal decoding keeps the 96 MiB conservative estimate allowance. Single-frame,
no-audio inspection allows an estimate up to 192 MiB with output count/time
restricted to one frame/one second; this is an admission estimate, not promised
RSS. Oversized sources/reference sets fail explicitly rather than silently
lowering quality. Existing byte/pixel/download limits still apply.

Inspection/crops default to lossless PNG; `format=jpeg` is explicit. Model detail
attachments share a 512 KiB raw-image allowance per reply and are never silently
shrunk. Exceeding it requests a smaller region, fewer frames or explicit JPEG.
Preview fitting remains compact, but `engine_input.attachment_sources` always
reports actual sent dimensions, MIME, bytes and `resized_for_wire`. This is
separate from prepared-frame geometry; supported engine wire frames are bounded
at 1 MiB. All paths remain Rust and contain no browser engine.

Rust 1.90 / edition 2024 的磁盘媒体处理库。库本身不联网、不访问账号、不调用外部进程；Runtime 先将实际媒体流式下载到有配额的文件，再按时间窗口调用本库。图像、音频、视频编解码均为 Rust，无浏览器内核、FFmpeg、OpenH264 或其他原生媒体库。

## 接口

- `pictures::probe_image(path, &Limits)`；`prepare_image(input, output, max_edge, OutputFormat::{Jpeg,Png}, &Limits)`。
- `audio::probe_audio(path, &Limits)`；`segment_audio_wav(input, output, start_ms, duration_ms, &Limits)`。
- `video::probe_video(path, &Limits)`；`sample_video_window(input, output_dir, start_ms, duration_ms, interval_ms, max_edge, &Limits)`。
- `animation::probe_gif(path, &Limits)`；`prepare_gif(input, output_dir, start_ms, duration_ms, interval_ms, max_edge, &Limits)`，输出目录由调用者创建。

参数路径均为 `&Path`，毫秒参数为 `u64`，缩放边长为 `u32`。返回独立的 serde 类型及 `MediaError`；不依赖 runtime-core。输出只创建新文件，Unix 权限为 `0600`，调用者应创建 `0700` 父目录。处理中失败会移除本次创建的部分输出。

## 已实现范围

图片使用 image-rs 的 JPEG、PNG、WebP、GIF 解码及 JPEG/PNG 输出，处理 EXIF 方向并按比例缩小，max_edge只是上限，不放大小图。图片入口对动画格式只读静态帧；动态WebP的时间采样尚未实现。

GIF的独立animation入口按时间顺序解码合成帧，不把全动画收集到内存，不执行无限循环。保留实际帧延迟、声明时长、循环标记、已解码进度及截断原因，按指定窗口输出JPEG。零延迟帧保留原值，并明确使用10 ms的取样策略，不假称浏览器播放语义。工作预算到达时返回truncated；首尾采样不代表连续观察了全部画面。原生小GIF不放大至max_edge，避免无意义的解码后内存和输出放大。

音频使用 Symphonia，启用 AAC、MP3、WAV/PCM、Ogg/Vorbis。AAC 实际验收为 AAC-LC、44.1 kHz 双声道。输出为原采样率、原声道数的 PCM16 WAV，不重采样。最多 96 kHz、双声道。SILK/AMR 明确返回 Unsupported；HE-AAC、Opus 等未启用/未支持编码也不会伪装成功。

视频使用固定 NihAV H.264 纯 Rust 解码器，范围为渐进式、8-bit、4:2:0、Baseline/Main/High，输出 JPEG。低清 B 站 DASH/fMP4 已实测。HEVC、AV1、隔行、高位深及加密媒体不在此范围。

fMP4 使用本库的小型磁盘样本索引读取 `tfhd`、`tfdt`、`trun`、有符号 composition offset、关键帧标志和线性 edit list。样本有效载荷始终从原文件按需读取，seek 从目标前的关键帧开始解码。普通非分片 MP4 保留 NihAV MOV 路径，尚无同等真实素材验收。非线性 edit list、负显示时间、首个 trun 无显式数据偏移等未覆盖封装会明确拒绝。

## 时间线与资源边界

每张图返回 `target_ms` 和 **实际** `actual_pts_ms`。窗口返回请求区间、实际解码首尾、解码帧数、采样间隔、缺失时间段和音频缺口。B 帧按照实际 PTS 落入采样区间。视频 sampler 不处理声音，`audio_processed=false`；应对同一范围的独立音轨调用 WAV 分段。少量采样帧只是画面证据，`visual_coverage=timestamped_samples_not_all_frames`，不能据此宣称看完。模型需要连续分段、可跳转回看，必要时提高采样密度，并结合真实音频/字幕理解。远端原生视频模型可以消费原媒体；本库不承诺在小 VPS 上运行视觉模型。

默认上限：输入 256 MiB，图像 4096 边长 / 800 万像素，单项解码预算 96 MiB，输出总计 32 MiB，窗口 60 秒，采样图 120 张。图片压缩输入另受 96 MiB 限制。MP4 在进入解码器前检查嵌套、box 数量、元数据大小、样本表、样本大小及 SPS；单个编码样本最多 8 MiB，索引最多 20 万样本。SPS 参考帧和估计帧缓冲开销也受预算约束。媒体库的内部开销不等同严格 RSS 上限，实际服务仍应使用 worker/容器内存硬限额。

YUV420p 当前使用 BT.601 limited-range 转 RGB；其他色彩矩阵/范围的精确色彩管理尚未实现。该边界影响颜色，不是只返回 URL、封面或字幕。

NihAV 固定来源与许可证见 [vendor/PROVENANCE.md](vendor/PROVENANCE.md)，真实执行证据见 [VALIDATION.md](../../docs/validation.md)。
