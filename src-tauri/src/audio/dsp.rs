//! 实时音频 DSP：变速 / 变调 / 重采样 / 增益。
//!
//! 设计要点
//! --------
//! * **变速不变调** + **变调不变速** 由 WSOLA（Waveform Similarity Overlap-Add）完成，
//!   之后再用采样率转换调整最终音高与时长：
//!   - 目标音高比 `P = 2^(semitones/12)`
//!   - 目标速度比 `S`
//!   - WSOLA 拉伸比 `T = P / S`（保持音高，只改时长）
//!   - 之后按 `R = P` 重采样（同时改音高与时长）
//!   - 合成后：音高 × P，时长 × (T / R) → 速度 × S ✔
//! * 全部为纯 Rust，无 C/C++ 依赖，Android 交叉编译无额外负担。
//! * 所有处理在**解码线程**完成，音频回调线程只做 ring buffer 搬运 + 音量，
//!   保证 callback 里没有内存分配（实时安全）。
//!
//! 文件级 `allow(dead_code)`：`sample_rate` / `is_identity` 是排查音质问题时的
//! 观察点，平时不参与主链路。

#![allow(dead_code)]

const WSOLA_FRAME_MS: f32 = 40.0;
const WSOLA_SEARCH_MS: f32 = 12.0;

/// 波形相似叠加时间拉伸器（流式，带跨块状态）。
///
/// 输入/输出都是交错 f32。立体声按「同一搜索偏移、同一淡化窗」处理，
/// 避免左右声道相位漂移。
pub struct Wsola {
    channels: usize,
    frame: usize,
    hop: usize,
    search: usize,
    /// 上一帧的尾部（hop 帧），作为下一帧的相似性参考 / 交叉淡化起点
    prev_tail: Vec<f32>,
    /// 尚未消费的输入样本（交错）
    pending: Vec<f32>,
    /// 输入读取位置（帧，相对 pending 起点，可为小数）
    read_pos: f64,
    /// 是否已经输出过第一帧
    primed: bool,
    sample_rate: u32,
}

impl Wsola {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let frame = ((sample_rate as f32 * WSOLA_FRAME_MS / 1000.0) as usize).max(256) & !1;
        let hop = frame / 2;
        let search = ((sample_rate as f32 * WSOLA_SEARCH_MS / 1000.0) as usize).max(64);
        Self {
            channels,
            frame,
            hop,
            search,
            prev_tail: Vec::new(),
            pending: Vec::new(),
            read_pos: 0.0,
            primed: false,
            sample_rate,
        }
    }

    pub fn reset(&mut self) {
        self.prev_tail.clear();
        self.pending.clear();
        self.read_pos = 0.0;
        self.primed = false;
    }

    /// 输入一块交错样本，按 `ratio`（输出长度 / 输入长度）返回拉伸后的样本。
    ///
    /// `ratio > 1` 变慢 / 拉长，`ratio < 1` 变快 / 缩短。
    pub fn process(&mut self, input: &[f32], ratio: f32) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        let ratio = ratio.clamp(0.25, 4.0) as f64;
        self.pending.extend_from_slice(input);

        let ch = self.channels;
        let frame_len = self.frame * ch;
        let hop_len = self.hop * ch;
        let mut out: Vec<f32> = Vec::with_capacity((input.len() as f64 * ratio) as usize + frame_len);

        loop {
            let analysis_frame = self.read_pos as usize;
            // 需要 frame + search 的余量才能做相似性搜索
            if (analysis_frame + self.frame + self.search) * ch > self.pending.len() {
                break;
            }

            let best = if self.primed {
                self.find_best_offset(analysis_frame)
            } else {
                analysis_frame
            };

            let start = best * ch;
            let frame_slice = &self.pending[start..start + frame_len];

            // 第一帧直接用原样，之后与上一帧尾部交叉淡化
            if self.primed && self.prev_tail.len() == hop_len {
                for i in 0..self.hop {
                    let w = i as f32 / self.hop as f32;
                    for c in 0..ch {
                        let idx = i * ch + c;
                        out.push(self.prev_tail[idx] * (1.0 - w) + frame_slice[idx] * w);
                    }
                }
            } else {
                out.extend_from_slice(&frame_slice[..hop_len]);
            }

            // 保存本帧尾部作为下一帧的参考
            self.prev_tail.clear();
            self.prev_tail.extend_from_slice(&frame_slice[hop_len..frame_len]);
            self.primed = true;

            self.read_pos += self.hop as f64 / ratio;

            // 回收 pending 前端，防止无限增长（保留 search 余量）
            let consumed = self.read_pos as usize;
            if consumed > self.frame * 4 {
                let drop_frames = consumed - self.frame * 2;
                let drop_samples = drop_frames * ch;
                self.pending.drain(..drop_samples);
                self.read_pos -= drop_frames as f64;
            }
        }

        out
    }

    /// 在 `analysis ± search` 内找与上一帧尾部最相似的偏移（归一化互相关）。
    fn find_best_offset(&self, analysis: usize) -> usize {
        let ch = self.channels;
        let hop_len = self.hop * ch;
        let tail = &self.prev_tail;
        if tail.len() != hop_len {
            return analysis;
        }

        let lo = analysis.saturating_sub(self.search).max(1);
        let hi = analysis + self.search;
        let mut best = analysis;
        let mut best_score = f32::NEG_INFINITY;

        // 每 4 帧采样一次，降低搜索开销（依然远高于感知需求）
        let stride = 4 * ch;
        let mut off = lo;
        while off <= hi {
            let start = off * ch;
            if start + hop_len > self.pending.len() {
                break;
            }
            let cand = &self.pending[start..start + hop_len];

            let mut dot = 0.0f32;
            let mut energy = 0.0f32;
            let mut i = 0;
            while i < hop_len {
                let v = cand[i];
                dot += v * tail[i];
                energy += v * v;
                i += stride;
            }
            let score = dot / (energy.sqrt() + 1e-6);

            if score > best_score {
                best_score = score;
                best = off;
            }
            off += 2;
        }
        best
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// 流式采样率转换（4 点 Catmull-Rom），跨块保留 3 帧历史，避免块边界爆音。
pub struct StreamResampler {
    channels: usize,
    /// 输入帧 / 输出帧
    step: f64,
    /// 当前读位置（以「历史 + 当前块」的帧坐标表示）
    pos: f64,
    /// 历史 3 帧
    hist: Vec<f32>,
    ratio: f64,
    from_rate: u32,
}

impl StreamResampler {
    pub fn new(from_rate: u32, to_rate: u32, channels: usize) -> Self {
        let ratio = to_rate as f64 / from_rate as f64;
        Self {
            channels,
            step: 1.0 / ratio,
            // buf = [hist(3 帧) + input]，从索引 1 开始读才能取到 p0 = buf[idx-1]
            pos: 1.0,
            hist: vec![0.0; 3 * channels],
            ratio,
            from_rate,
        }
    }

    /// 运行时改变目标采样率（变速/变调会用到）。不重置历史，避免爆音。
    pub fn set_target_rate(&mut self, to_rate: u32) {
        let ratio = to_rate as f64 / self.from_rate as f64;
        if (ratio - self.ratio).abs() > 1e-9 {
            self.ratio = ratio;
            self.step = 1.0 / ratio;
        }
    }

    pub fn is_identity(&self) -> bool {
        (self.ratio - 1.0).abs() < 1e-9
    }

    pub fn reset(&mut self) {
        self.pos = 1.0;
        self.hist.iter_mut().for_each(|v| *v = 0.0);
    }

    /// 输入交错样本，输出重采样后样本。
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        let ch = self.channels;
        let hist_frames = 3;
        let in_frames = input.len() / ch;

        // 拼成 [hist(3) + input]
        let mut buf = Vec::with_capacity((hist_frames + in_frames) * ch);
        buf.extend_from_slice(&self.hist);
        buf.extend_from_slice(input);

        let total_frames = hist_frames + in_frames;
        // 输出帧数：以 pending 的绝对位置推进
        let mut out = Vec::with_capacity(((in_frames as f64) * self.ratio + 4.0) as usize * ch);

        loop {
            let idx = self.pos as usize;
            if idx < 1 || idx + 3 > total_frames {
                break;
            }
            let frac = (self.pos - idx as f64) as f32;
            for c in 0..ch {
                let p0 = buf[(idx - 1) * ch + c];
                let p1 = buf[idx * ch + c];
                let p2 = buf[(idx + 1) * ch + c];
                let p3 = buf[(idx + 2) * ch + c];
                out.push(catmull_rom(p0, p1, p2, p3, frac));
            }
            self.pos += self.step;
        }

        // 保存最后 3 帧作为下一块历史，并把坐标原点移到该处
        let keep_from = total_frames.saturating_sub(3);
        self.hist.clear();
        self.hist
            .extend_from_slice(&buf[keep_from * ch..total_frames * ch]);
        self.pos -= keep_from as f64;
        if self.pos < 1.0 {
            self.pos = 1.0;
        }

        out
    }
}

#[inline]
fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}

/// dB → 线性增益。
#[inline]
pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// 声道适配：把任意声道数转成目标声道数（交错 f32）。
pub fn adapt_channels(input: &[f32], from: usize, to: usize) -> Vec<f32> {
    if from == to {
        return input.to_vec();
    }
    let frames = input.len() / from.max(1);
    let mut out = Vec::with_capacity(frames * to);
    for f in 0..frames {
        let base = f * from;
        if from == 1 {
            let v = input[base];
            for _ in 0..to {
                out.push(v);
            }
        } else if to == 1 {
            let mut sum = 0.0;
            for c in 0..from {
                sum += input[base + c];
            }
            out.push(sum / from as f32);
        } else {
            for c in 0..to {
                out.push(input[base + (c % from)]);
            }
        }
    }
    out
}

/// 峰值归一化保护：避免增益叠加后削波（软限幅）。
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    if x > 1.0 {
        1.0 - (-x).exp() * 0.3
    } else if x < -1.0 {
        -1.0 + x.exp() * 0.3
    } else {
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsola_preserves_length_ratio() {
        let mut w = Wsola::new(48_000, 2);
        let input: Vec<f32> = (0..48_000 * 2).map(|i| (i as f32 * 0.01).sin()).collect();
        let out = w.process(&input, 2.0);
        // WSOLA 是流式的：需要先囤够 frame + search 才能输出，
        // 因此存在固定预热延迟（约 2.5 个 hop），不是比例误差。
        // 稳态下输出/输入严格等于 ratio。
        let ratio = out.len() as f64 / input.len() as f64;
        assert!(
            ratio > 1.85 && ratio < 2.02,
            "ratio={ratio:.3} len={} (fixed warm-up latency expected)",
            out.len()
        );
        // 再喂一块，比率应该回到 2.0 附近（预热已经完成）
        let out2 = w.process(&input, 2.0);
        let ratio2 = out2.len() as f64 / input.len() as f64;
        assert!(
            (ratio2 - 2.0).abs() < 0.06,
            "steady-state ratio={ratio2:.3} len={}",
            out2.len()
        );
    }

    #[test]
    fn resampler_doubles_rate() {
        let mut r = StreamResampler::new(24_000, 48_000, 1);
        let input: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let out = r.process(&input);
        assert!(out.len() > 1900, "len={}", out.len());
    }
}
