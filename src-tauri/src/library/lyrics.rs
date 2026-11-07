//! LRC 歌词解析（普通 + 逐字）。
//!
//! 支持：
//! * `[mm:ss.xx]` / `[mm:ss.xxx]` / `[mm:ss]`
//! * 一行多个时间标签（`[00:12.00][00:45.00]副歌`）
//! * 逐字标签 `<mm:ss.xx>字`
//! * 元信息 `[ti:]` `[ar:]` `[al:]` `[offset:]` `[by:]`
//!
//! 解析后统一成「按开始时间排序 + 每行有 end_ms」的结构，
//! 前端不需要再做任何时间推断。

use crate::models::{LyricKind, LyricLine, LyricWord, Lyrics};

/// 解析 LRC 文本。`fallback_duration_ms` 用于给最后一行补 end_ms。
pub fn parse_lrc(text: &str, fallback_duration_ms: u64) -> Lyrics {
    let mut meta_title = None;
    let mut meta_artist = None;
    let mut meta_album = None;
    let mut offset_ms: i64 = 0;

    // (start_ms, raw_text)
    let mut raw_lines: Vec<(u64, String)> = Vec::new();
    let mut has_word_by_word = false;

    for line in text.lines() {
        let line = line.trim_end_matches(['\r', '\u{feff}']);
        if line.trim().is_empty() {
            continue;
        }

        let mut rest = line;
        let mut times: Vec<u64> = Vec::new();

        // 连续解析行首的 [..]
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end) = stripped.find(']') else { break };
            let inside = &stripped[..end];
            let after = &stripped[end + 1..];

            if let Some(ms) = parse_timestamp(inside) {
                times.push(ms);
                rest = after;
            } else {
                // 元信息
                if let Some((k, v)) = inside.split_once(':') {
                    let v = v.trim();
                    match k.to_ascii_lowercase().as_str() {
                        "ti" => meta_title = Some(v.to_string()),
                        "ar" => meta_artist = Some(v.to_string()),
                        "al" => meta_album = Some(v.to_string()),
                        "offset" => {
                            if let Ok(n) = v.parse::<i64>() {
                                offset_ms = n;
                            }
                        }
                        _ => {}
                    }
                }
                rest = after;
            }
        }

        if times.is_empty() {
            continue;
        }

        let content = rest.trim().to_string();
        if content.is_empty() {
            continue;
        }

        if content.contains('<') && looks_like_word_tags(&content) {
            has_word_by_word = true;
        }

        for t in times {
            raw_lines.push((t, content.clone()));
        }
    }

    raw_lines.sort_by_key(|(t, _)| *t);

    // 应用 offset（正数表示歌词整体延后）
    let apply = |t: u64| -> u64 {
        let v = t as i64 + offset_ms;
        v.max(0) as u64
    };

    let mut lines: Vec<LyricLine> = Vec::with_capacity(raw_lines.len());
    let mut i = 0;
    while i < raw_lines.len() {
        let (raw_start, content) = raw_lines[i].clone();
        let start = apply(raw_start);
        let words = parse_words(&content, start);
        let text = strip_word_tags(&content);

        // 同一时间戳的紧邻下一行按「翻译」处理。
        // 网易云的 tlyric 就是一份独立的同时间轴 LRC，插件把它接在原文后面，
        // 排序后天然形成「原文一行 + 译文一行」的结构。
        // 文本相同则不算翻译——那是重复的原文。
        let mut translation = None;
        if let Some((next_start, next_content)) = raw_lines.get(i + 1) {
            if *next_start == raw_start {
                let next_text = strip_word_tags(next_content);
                if !next_text.is_empty() && next_text != text {
                    translation = Some(next_text);
                    i += 1; // 消费掉译文行
                }
            }
        }

        lines.push(LyricLine {
            start_ms: start,
            end_ms: 0,
            text,
            words,
            translation,
        });
        i += 1;
    }

    // 补 end_ms
    let total = if fallback_duration_ms > 0 {
        fallback_duration_ms
    } else {
        lines.last().map(|l| l.start_ms + 8000).unwrap_or(0)
    };
    for i in 0..lines.len() {
        let next = lines.get(i + 1).map(|l| l.start_ms).unwrap_or(total);
        lines[i].end_ms = next.max(lines[i].start_ms + 200);
        // 逐字行也要给最后一个字补 end
        let line_end = lines[i].end_ms;
        if let Some(last) = lines[i].words.last_mut() {
            if last.end_ms == 0 || last.end_ms > line_end {
                last.end_ms = line_end;
            }
        }
    }

    Lyrics {
        kind: if has_word_by_word {
            LyricKind::WordByWord
        } else {
            LyricKind::Plain
        },
        title: meta_title,
        artist: meta_artist,
        album: meta_album,
        offset_ms,
        lines,
    }
}

fn looks_like_word_tags(s: &str) -> bool {
    // <mm:ss.xx>
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'<' {
            let rest = &s[i + 1..];
            if let Some(end) = rest.find('>') {
                if parse_timestamp(&rest[..end]).is_some() {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// `mm:ss.xx` / `mm:ss.xxx` / `mm:ss`
fn parse_timestamp(s: &str) -> Option<u64> {
    let (mm, rest) = s.split_once(':')?;
    let mm: u64 = mm.trim().parse().ok()?;
    let (ss, frac) = match rest.split_once(['.', ':']) {
        Some((ss, frac)) => (ss, Some(frac)),
        None => (rest, None),
    };
    let ss: u64 = ss.trim().parse().ok()?;
    let frac_ms = match frac {
        Some(f) => {
            let f = f.trim();
            let digits: String = f.chars().take(3).collect();
            let n: u64 = digits.parse().ok()?;
            match digits.len() {
                1 => n * 100,
                2 => n * 10,
                _ => n,
            }
        }
        None => 0,
    };
    Some(mm * 60_000 + ss * 1000 + frac_ms)
}

/// 解析 `<t>word<t>word` 形式的逐字歌词。
fn parse_words(content: &str, line_start: u64) -> Vec<LyricWord> {
    if !looks_like_word_tags(content) {
        return Vec::new();
    }
    let mut words = Vec::new();
    let mut current_time: Option<u64> = None;
    let mut buf = String::new();
    let mut chars = content.char_indices().peekable();

    while let Some((i, ch)) = chars.next() {
        if ch == '<' {
            let rest = &content[i + 1..];
            if let Some(end) = rest.find('>') {
                if let Some(ms) = parse_timestamp(&rest[..end]) {
                    if let Some(t) = current_time {
                        if !buf.is_empty() {
                            words.push(LyricWord {
                                start_ms: t,
                                end_ms: ms,
                                text: buf.clone(),
                            });
                            buf.clear();
                        }
                    }
                    current_time = Some(ms);
                    // 跳过已解析的标签
                    for _ in 0..(end + 1) {
                        chars.next();
                    }
                    continue;
                }
            }
        }
        buf.push(ch);
    }

    if let Some(t) = current_time {
        if !buf.is_empty() {
            words.push(LyricWord {
                start_ms: t,
                end_ms: 0, // 由调用方补
                text: buf,
            });
        }
    }

    if words.is_empty() {
        return words;
    }
    // 第一个字之前如果还有时间，对齐到行首
    if words[0].start_ms > line_start + 100 {
        words[0].start_ms = line_start;
    }
    words
}

fn strip_word_tags(content: &str) -> String {
    if !content.contains('<') {
        return content.to_string();
    }
    let mut out = String::with_capacity(content.len());
    let mut in_tag = false;
    for ch in content.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// 有翻译歌词时按时间轴合并（容差 400ms）。
pub fn merge_translation(main: &mut Lyrics, translation: &Lyrics) {
    for line in main.lines.iter_mut() {
        let best = translation
            .lines
            .iter()
            .min_by_key(|t| (t.start_ms as i64 - line.start_ms as i64).unsigned_abs());
        if let Some(t) = best {
            let diff = (t.start_ms as i64 - line.start_ms as i64).unsigned_abs();
            if diff <= 400 {
                line.translation = Some(t.text.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_lrc() {
        let lrc = "[ti:测试]\n[ar:歌手]\n[00:01.00]第一句\n[00:03.50]第二句\n";
        let y = parse_lrc(lrc, 10_000);
        assert_eq!(y.lines.len(), 2);
        assert_eq!(y.lines[0].start_ms, 1000);
        assert_eq!(y.lines[1].start_ms, 3500);
        assert_eq!(y.lines[0].end_ms, 3500);
        assert_eq!(y.kind, LyricKind::Plain);
    }

    #[test]
    fn parses_multi_timestamp() {
        let lrc = "[00:01.00][00:05.00]重复\n";
        let y = parse_lrc(lrc, 10_000);
        assert_eq!(y.lines.len(), 2);
        assert_eq!(y.lines[1].start_ms, 5000);
    }

    #[test]
    fn parses_word_by_word() {
        let lrc = "[00:01.00]<00:01.00>你<00:01.50>好<00:02.00>世界\n";
        let y = parse_lrc(lrc, 5_000);
        assert_eq!(y.kind, LyricKind::WordByWord);
        assert_eq!(y.lines[0].text, "你好世界");
        assert_eq!(y.lines[0].words.len(), 3);
        assert_eq!(y.lines[0].words[0].end_ms, 1500);
    }

    #[test]
    fn applies_offset() {
        let lrc = "[offset:-500]\n[00:01.00]x\n";
        let y = parse_lrc(lrc, 5_000);
        assert_eq!(y.lines[0].start_ms, 500);
    }

    #[test]
    fn merges_translation_on_same_timestamp() {
        // 网易云的 tlyric 是独立的一份同时间轴 LRC，插件把它接在原文后面
        let lrc = "[00:01.00]Hello\n[00:01.00]你好\n[00:03.00]World\n";
        let y = parse_lrc(lrc, 10_000);
        assert_eq!(y.lines.len(), 2);
        assert_eq!(y.lines[0].text, "Hello");
        assert_eq!(y.lines[0].translation.as_deref(), Some("你好"));
        assert_eq!(y.lines[1].text, "World");
        assert_eq!(y.lines[1].translation, None);
    }

    #[test]
    fn repeated_same_text_is_not_translation() {
        // 合唱段落里同时间戳重复原文，不能被当成译文吃掉
        let lrc = "[00:01.00]啊\n[00:01.00]啊\n";
        let y = parse_lrc(lrc, 5_000);
        assert_eq!(y.lines.len(), 2);
        assert!(y.lines[0].translation.is_none());
        assert!(y.lines[1].translation.is_none());
    }

    #[test]
    fn translation_survives_sorting() {
        // 插件是「整份原文 + 整份译文」拼在一起的，排序后必须仍然相邻
        let lrc = "[00:03.00]B\n[00:01.00]A\n[00:03.00]乙\n[00:01.00]甲\n";
        let y = parse_lrc(lrc, 10_000);
        assert_eq!(y.lines.len(), 2);
        assert_eq!(y.lines[0].text, "A");
        assert_eq!(y.lines[0].translation.as_deref(), Some("甲"));
        assert_eq!(y.lines[1].text, "B");
        assert_eq!(y.lines[1].translation.as_deref(), Some("乙"));
    }
}
