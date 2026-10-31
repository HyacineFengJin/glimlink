# Watching test media

`watch-synthetic.mp4` is the 2-second 64×64 generated test pattern already embedded in `examples/validate_qq_attachments.rs`. It contains no private or platform video. It is retained as a decoder test fixture; no FFmpeg or native encoder is invoked by Runtime or its tests. The window test pairs it with generated PCM and cached synthetic subtitle cues.
