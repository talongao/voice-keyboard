// 麦克风采集：把音频攒成 100ms 一块的 16-bit 单声道 PCM，交给页面 POST 给电脑。
//
// 为什么用 AudioWorklet 而不是 MediaRecorder：MediaRecorder 给的是压缩容器
// （webm/opus），电脑那边还得解封装解码——那会把 ffmpeg/libopus 拖进来，
// 违背"单文件、轻量"的底线。裸 PCM 局域网完全够用（48k×16bit ≈ 768 kbps）。
//
// 采样率由 AudioContext 定（页面请求 48000），这里不做重采样。

const CHUNK_FRAMES = 4800; // 100ms @ 48kHz

class MicChunker extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Int16Array(CHUNK_FRAMES);
    this.n = 0;
  }

  process(inputs) {
    const input = inputs[0];
    const ch = input && input[0];
    if (ch) {
      for (let i = 0; i < ch.length; i++) {
        let v = ch[i];
        if (v > 1) v = 1; else if (v < -1) v = -1;
        this.buf[this.n++] = v < 0 ? v * 0x8000 : v * 0x7fff;
        if (this.n >= CHUNK_FRAMES) {
          // 复制后转移，避免和下一块的写入打架
          const out = this.buf.slice(0);
          this.port.postMessage(out, [out.buffer]);
          this.n = 0;
        }
      }
    }
    return true; // 保持存活
  }
}

registerProcessor("mic-chunker", MicChunker);
