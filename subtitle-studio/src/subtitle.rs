//! Bounded, UTF-8 subtitle parsing and rendering shared by the web worker and CLI.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_CUES: usize = 100_000;
pub const MAX_SUBTITLE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CUE_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Cue {
    // Alphabetical serialization preserves legacy sorted-key cache identities.
    pub end: f64,
    pub start: f64,
    pub text: String,
}

pub fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn validate(cues: &[Cue]) -> Result<(), String> {
    if cues.is_empty() || cues.len() > MAX_CUES {
        return Err("Subtitles must contain between 1 and 100000 cues".into());
    }
    let mut bytes = 0usize;
    for cue in cues {
        if !cue.start.is_finite()
            || !cue.end.is_finite()
            || cue.start < 0.0
            || cue.end <= cue.start
            || cue.end > 3_600_000.0
            || cue.text.trim().is_empty()
            || cue.text.len() > MAX_CUE_BYTES
        {
            return Err("Invalid subtitle timing or text".into());
        }
        bytes = bytes.saturating_add(cue.text.len());
        if bytes > MAX_SUBTITLE_BYTES {
            return Err("Subtitle text exceeds the 16 MiB limit".into());
        }
    }
    Ok(())
}

fn seconds(stamp: &str) -> Result<f64, String> {
    let mut parts = stamp.split([':', ',']);
    let values: Vec<&str> = parts.by_ref().collect();
    let [hours, minutes, secs, millis] = values.as_slice() else {
        return Err("Invalid SRT timestamp".into());
    };
    if hours.len() < 2 || minutes.len() != 2 || secs.len() != 2 || millis.len() != 3 {
        return Err("Invalid SRT timestamp".into());
    }
    let parse = |value: &str| -> Result<u64, String> {
        if !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("Invalid SRT timestamp".into());
        }
        value
            .parse::<u64>()
            .map_err(|_| "Invalid SRT timestamp".into())
    };
    let (h, m, s, ms) = (parse(hours)?, parse(minutes)?, parse(secs)?, parse(millis)?);
    if m > 59 || s > 59 || h > 1000 {
        return Err("Invalid SRT timestamp".into());
    }
    Ok((h * 3600 + m * 60 + s) as f64 + ms as f64 / 1000.0)
}

pub fn parse_srt(content: &str) -> Result<Vec<Cue>, String> {
    if content.len() > MAX_SUBTITLE_BYTES {
        return Err("Saved subtitle exceeds the 16 MiB limit".into());
    }
    let normalized = content
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut cues = Vec::new();
    let mut lines = normalized.lines().peekable();
    while lines.peek().is_some() {
        while lines.peek().is_some_and(|line| line.trim().is_empty()) {
            lines.next();
        }
        let Some(index) = lines.next() else { break };
        if index.trim().parse::<usize>().ok() != Some(cues.len() + 1) {
            return Err("Saved subtitles have invalid or unordered cue numbers".into());
        }
        let timing = lines.next().ok_or("Saved subtitles have no timestamp")?;
        let (start, end) = timing
            .trim()
            .split_once(" --> ")
            .ok_or("Saved subtitles have an invalid timestamp")?;
        let mut text_lines = Vec::new();
        while lines.peek().is_some_and(|line| !line.trim().is_empty()) {
            if let Some(line) = lines.next() {
                text_lines.push(line.trim());
            }
        }
        // The renderer breaks a single unspaced token every 42 characters.
        // Rejoining those lines must not introduce spaces into CJK text.
        let sliced_token = text_lines.len() > 1
            && text_lines
                .iter()
                .all(|line| !line.chars().any(char::is_whitespace))
            && text_lines
                .iter()
                .take(text_lines.len().saturating_sub(1))
                .all(|line| line.chars().count() == 42);
        let text = normalize_text(&text_lines.join(if sliced_token { "" } else { " " }));
        cues.push(Cue {
            start: seconds(start)?,
            end: seconds(end)?,
            text,
        });
        if cues.len() > MAX_CUES {
            return Err("Too many subtitle cues".into());
        }
    }
    validate(&cues)?;
    Ok(cues)
}

pub fn timestamp(seconds: f64) -> String {
    let value = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        value / 3_600_000,
        value / 60_000 % 60,
        value / 1000 % 60,
        value % 1000
    )
}

fn wrap_text(text: &str) -> String {
    let text = normalize_text(text);
    let mut output = String::with_capacity(text.len());
    let mut width = 0usize;
    for word in text.split(' ') {
        let word_width = word.chars().count();
        if width > 0 {
            if width + 1 + word_width <= 42 {
                output.push(' ');
                width += 1;
            } else {
                output.push('\n');
                width = 0;
            }
        }
        for character in word.chars() {
            if width == 42 {
                output.push('\n');
                width = 0;
            }
            output.push(character);
            width += 1;
        }
    }
    output
}

pub fn render_srt(cues: &[Cue]) -> String {
    use std::fmt::Write;
    let mut result = String::new();
    for (index, cue) in cues.iter().enumerate() {
        if index > 0 {
            result.push('\n');
        }
        let _ = writeln!(
            result,
            "{}\n{} --> {}\n{}",
            index + 1,
            timestamp(cue.start),
            timestamp(cue.end),
            wrap_text(&cue.text)
        );
    }
    result
}

/// Structural errors affect exit status; readability flags are advisory.
pub fn inspect(cues: &[Cue], source: Option<&[Cue]>, duration: Option<f64>, max_cps: f64) -> Value {
    let mut errors = Vec::<String>::new();
    let mut notes = Vec::<String>::new();
    if let Err(error) = validate(cues) {
        errors.push(error);
    }
    for (index, cue) in cues.iter().enumerate() {
        let number = index + 1;
        if index
            .checked_sub(1)
            .and_then(|previous| cues.get(previous))
            .is_some_and(|previous| cue.start < previous.end)
        {
            errors.push(format!("Cue {number}: overlaps previous cue"));
        }
        if duration.is_some_and(|duration| cue.end > duration + 0.5) {
            errors.push(format!("Cue {number}: ends after the video"));
        }
        let length = cue.end - cue.start;
        if length < 0.8 {
            notes.push(format!("Cue {number}: shown for under 0.8 seconds"));
        }
        if length > 7.0 {
            notes.push(format!("Cue {number}: shown for over 7 seconds"));
        }
        if length > 0.0 && cue.text.chars().filter(|c| *c != '\n').count() as f64 / length > max_cps
        {
            notes.push(format!("Cue {number}: exceeds {max_cps} characters/second"));
        }
        let wrapped = wrap_text(&cue.text);
        if wrapped.lines().count() > 2 {
            notes.push(format!("Cue {number}: more than 2 text lines"));
        }
        if cue.text.trim_end().ends_with('-') {
            notes.push(format!(
                "Cue {number}: ends with a hyphen; check for a broken word"
            ));
        }
    }
    if let Some(source) = source {
        if source.len() != cues.len() {
            errors.push(format!(
                "Cue counts differ: source {}, translation {}",
                source.len(),
                cues.len()
            ));
        }
        for (index, (left, right)) in source.iter().zip(cues).enumerate() {
            if (left.start - right.start).abs() > 0.0401 || (left.end - right.end).abs() > 0.0401 {
                errors.push(format!(
                    "Cue {}: source and translation timings differ",
                    index + 1
                ));
            }
        }
    }
    json!({"cues":cues.len(), "structural_issues":errors.len(), "readability_flags":notes.len(), "errors":errors.into_iter().take(20).collect::<Vec<_>>(), "notes":notes.into_iter().take(20).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millisecond_render_round_trip_and_unspaced_unicode() {
        let cues = vec![Cue {
            start: 3599.9996,
            end: 3602.0,
            text: "語".repeat(90),
        }];
        let content = render_srt(&cues);
        assert!(content.contains("01:00:00,000"));
        let parsed = parse_srt(&content).expect("valid rendered SRT");
        assert_eq!(
            parsed.first().map(|cue| cue.text.as_str()),
            cues.first().map(|cue| cue.text.as_str())
        );
        assert_eq!(parsed.first().map(|cue| cue.start), Some(3600.0));
    }

    #[test]
    fn rejects_invalid_and_unordered_subtitles() {
        for text in [
            "2\n00:00:01,000 --> 00:00:02,000\nHello",
            "1\n00:61:00,000 --> 00:62:00,000\nHello",
            "1\n00:00:02,000 --> 00:00:01,000\nHello",
            "",
        ] {
            assert!(parse_srt(text).is_err(), "{text}");
        }
    }

    #[test]
    fn readability_does_not_change_structural_validity() {
        let cues = vec![Cue {
            start: 1.0,
            end: 1.2,
            text: "A sentence too long to read that quickly".into(),
        }];
        let report = inspect(&cues, None, None, 20.0);
        assert_eq!(report["structural_issues"], 0);
        assert_eq!(report["readability_flags"], 2);
    }
}
