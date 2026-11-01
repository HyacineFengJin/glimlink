# runtime-speech

按需 Rust CPU 语音转写；宿主解码音频后直接调用，不需要 AI 编排模型下载、音频转换或分段流程。Whisper tiny 多语言模型、Candle 0.9.1 CPU、tokenizers 0.21.4 的 fancy-regex 路径；未启用 CUDA、MKL、Accelerate、Metal、Oniguruma、Python 或浏览器。

```rust,no_run
let models = std::path::Path::new("/state/models/whisper-tiny");
runtime_speech::install(models)?; // 显式管理命令；普通转写不会联网或自动安装
let result = runtime_speech::transcribe(models, &[0.0f32; 16000], 16000, 1, None)?;
# Ok::<(), String>(())
```

- `status` 逐个核对固定 SHA256/大小；`install` 只拉取已固定模型版本、tokenizer、配置与 mel filters。下载流式限长、每次跳转校验 HTTPS 域名、原子安装且不覆盖损坏的旧文件；没有账号材料。模型 revision 为 `02d8350e5402f18725eadb6101b4963d181b0b5e`，4 文件总计约42.3 MiB。
- 使用 `model-tiny-q80.gguf`。这个具体仓库的 q80 约39.9 MiB，而名为 q4k 的产物约128.6 MiB；不能仅由文件标签推断实际运行开销。
- 输入为已解码的 f32 interleaved PCM，8–96 kHz、单/双声道、至多10秒。Rust 先 downmix，再用32 tap/1024 phase windowed-sinc 重采样到16 kHz；不调用外部转码进程。较长音频由宿主串行分段。
- 每次调用创建独立2线程 Rayon pool，模型与 tensor、tokenizer、pool 都在返回前释放，不建立常驻服务/全局推理 pool。模型内部只计算当前窗口与2秒零填充的 encoder 上下文，避免对10秒窗口计算30秒 attention。
- 默认检测语种，支持明确的 `Some("zh")` / `Some("en")` 等提示。使用确定性 greedy 解码，最多128个生成 token；达到上限返回 `truncated=true`。保留语言概率、平均 log probability、no-speech probability，并对数字零/极低能量输入返回空文本。
- 结果是语音识别；`transcription_only=true`、`environment_sounds_understood=false`。segment 时间范围只是输入窗口，`alignment=window_only_no_word_timestamps`；不伪称有逐字时间轴、音乐/环境音理解或声纹身份。Tiny 的识别错误和噪声幻觉仍需消费者依据上下文判断。

管理二进制 `speech-model install|status DIRECTORY` 可独立工作；`transcribe-wav DIRECTORY PUBLIC_FIXTURE.wav [LANGUAGE]` 是接受 PCM16 WAV 的验收入口，自动取前10秒，报告进程 peak RSS 与容器 memory.peak。产品媒体流程应通过 Runtime 自己的解码器调用库接口。

本轮真实验收：Docker 384 MiB、2 CPU、无网转写公开 [Candle JFK WAV](https://huggingface.co/datasets/Narsil/candle-examples/blob/main/samples_jfk.wav) 的前10秒，正确识别英文且自然结束；约4.6秒，peak RSS约149.9 MiB，容器峰值约198.8 MiB。报告位于 `.runtime/reports/speech-jfk-q80-20261030.json`。这是该公开样本的测量，不是所有平台语音/所有模型共存场景的内存上限保证。

实现参考 [Candle 0.9.1 Whisper 示例](https://github.com/huggingface/candle/blob/0.9.1/candle-examples/examples/whisper/main.rs)、[量化 Whisper 模型](https://github.com/huggingface/candle/blob/0.9.1/candle-transformers/src/models/whisper/quantized_model.rs) 与 [固定模型仓库](https://huggingface.co/lmz/candle-whisper/tree/02d8350e5402f18725eadb6101b4963d181b0b5e)。依赖的版权及许可证见各上游包；Candle 为 MIT OR Apache-2.0，Whisper 模型为 MIT。

## 小内存推理

词表保留原 Q8_0 格式，只按需展开 token 行；输出投影每次最多展开1024行并进行 f32 乘法，避免常驻整张约76 MiB的浮点词表。不降低采样率、不缩短原有窗口，也不新增激活量化。编码后提前释放 encoder；decoder 只保留特征。

GGUF 载入的是拥有所有权的 CPU 数组，并非文件映射。Linux 下加载完成后对该只读模型文件给出 `POSIX_FADV_DONTNEED` 提示，减少文件缓存与权重内存双份占用；该建议可被内核忽略，后续窗口可能增加磁盘读取，不刷新全局缓存。

Candle 模型层的局部适配位于 `vendor/candle-whisper`，保留原 MIT OR Apache-2.0 及来源；独立分块实现 `src/token_embedding.rs` 为本项目 MIT 代码。数值测试对齐完整 f32 参考；中文、英文真实样本的转写与原实现逐字一致。完整容器内存包括文件缓存，须与进程 RSS/PSS 分开判断。

GNU/Linux 下推理调用结束并销毁 Rayon pool 后，调用 `malloc_trim` 归还分配器中未使用的页面；活跃的结果、缓存引用和工具状态不受影响。这是操作边界上的尽力回收，不增加后台线程。真实四轮大图/语音结束后的进程树PSS由约156 MiB降到18 MiB。
