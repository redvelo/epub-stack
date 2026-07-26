use super::{AnalysisIssue, InspectionKind};
use crate::analysis::inspection::{Text, TextEncoding, WebVtt};

pub(super) fn inspect(bytes: &[u8], complete: bool) -> Text {
    let (encoding, has_byte_order_mark) = if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        (Some(TextEncoding::Utf8), true)
    } else if bytes.starts_with(&[0xff, 0xfe]) {
        (Some(TextEncoding::Utf16Le), true)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (Some(TextEncoding::Utf16Be), true)
    } else if bytes.starts_with(&[0x3c, 0, 0x3f, 0]) {
        (Some(TextEncoding::Utf16Le), false)
    } else if bytes.starts_with(&[0, 0x3c, 0, 0x3f]) {
        (Some(TextEncoding::Utf16Be), false)
    } else if complete && std::str::from_utf8(bytes).is_ok() {
        (Some(TextEncoding::Utf8), false)
    } else {
        (None, false)
    };
    Text::new(encoding, has_byte_order_mark)
}

pub(super) fn inspect_webvtt(
    bytes: &[u8],
    complete: bool,
) -> (InspectionKind, Option<AnalysisIssue>) {
    if !complete {
        return (InspectionKind::WebVtt(WebVtt::new(None, None)), None);
    }
    let text = match decode_text(bytes) {
        Some(text) => text,
        None => return (InspectionKind::WebVtt(WebVtt::new(Some(false), None)), None),
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let (valid, cue_count) = scan_webvtt(text);
    (
        InspectionKind::WebVtt(WebVtt::new(Some(valid), Some(cue_count))),
        None,
    )
}

fn scan_webvtt(text: &str) -> (bool, u64) {
    let lines = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect::<Vec<_>>();
    let Some(header) = lines.first() else {
        return (false, 0);
    };
    if !(header == &"WEBVTT" || header.starts_with("WEBVTT ") || header.starts_with("WEBVTT\t")) {
        return (false, 0);
    }

    let mut valid = true;
    let mut index = 1usize;
    while index < lines.len() && !lines[index].is_empty() {
        if lines[index].contains("-->") {
            valid = false;
        }
        index += 1;
    }
    if index < lines.len() {
        index += 1;
    }

    let mut cue_count = 0u64;
    let mut saw_cue = false;
    while index < lines.len() {
        while index < lines.len() && lines[index].is_empty() {
            index += 1;
        }
        if index == lines.len() {
            break;
        }
        let start = index;
        while index < lines.len() && !lines[index].is_empty() {
            index += 1;
        }
        let block = &lines[start..index];
        let first = block[0];
        if first == "NOTE" || first.starts_with("NOTE ") || first.starts_with("NOTE\t") {
            continue;
        }
        if first == "STYLE" || first == "REGION" {
            if saw_cue || block.iter().any(|line| line.contains("-->")) {
                valid = false;
            }
            continue;
        }
        let timing = if first.contains("-->") {
            first
        } else if block.get(1).is_some_and(|line| line.contains("-->")) {
            block[1]
        } else {
            valid = false;
            continue;
        };
        saw_cue = true;
        if valid_vtt_timing(timing) {
            cue_count += 1;
        } else {
            valid = false;
        }
    }
    (valid, cue_count)
}

fn valid_vtt_timing(value: &str) -> bool {
    let Some((start, rest)) = value.split_once("-->") else {
        return false;
    };
    if rest.contains("-->") {
        return false;
    }
    let mut end_and_settings = rest.split_whitespace();
    let Some(end) = end_and_settings.next() else {
        return false;
    };
    let Some(start) = parse_vtt_timestamp(start.trim()) else {
        return false;
    };
    let Some(end) = parse_vtt_timestamp(end) else {
        return false;
    };
    end > start
        && end_and_settings.all(|setting| {
            setting.split_once(':').is_some_and(|(name, value)| {
                !value.is_empty()
                    && matches!(
                        name,
                        "vertical" | "line" | "position" | "size" | "align" | "region"
                    )
            })
        })
}

fn parse_vtt_timestamp(value: &str) -> Option<u64> {
    let (whole, millis) = value.rsplit_once('.')?;
    if millis.len() != 3 || !millis.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let parts = whole.split(':').collect::<Vec<_>>();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] if minutes.len() == 2 && seconds.len() == 2 => (
            0,
            minutes.parse::<u64>().ok()?,
            seconds.parse::<u64>().ok()?,
        ),
        [hours, minutes, seconds]
            if hours.len() >= 2 && minutes.len() == 2 && seconds.len() == 2 =>
        {
            (
                hours.parse::<u64>().ok()?,
                minutes.parse::<u64>().ok()?,
                seconds.parse::<u64>().ok()?,
            )
        }
        _ => return None,
    };
    if minutes > 59 || seconds > 59 {
        return None;
    }
    hours
        .checked_mul(60)?
        .checked_add(minutes)?
        .checked_mul(60)?
        .checked_add(seconds)?
        .checked_mul(1_000)?
        .checked_add(millis.parse::<u64>().ok()?)
}

fn decode_text(bytes: &[u8]) -> Option<String> {
    if let Some(content) = bytes.strip_prefix(&[0xff, 0xfe]) {
        if content.len() % 2 != 0 {
            return None;
        }
        let units = content
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&units).ok()
    } else if let Some(content) = bytes.strip_prefix(&[0xfe, 0xff]) {
        if content.len() % 2 != 0 {
            return None;
        }
        let units = content
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&units).ok()
    } else {
        std::str::from_utf8(strip_utf8_bom(bytes))
            .ok()
            .map(str::to_owned)
    }
}

fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_webvtt_is_a_completed_semantic_result() {
        let (kind, issue) = inspect_webvtt(b"not vtt", true);
        let InspectionKind::WebVtt(vtt) = kind else {
            panic!()
        };
        assert_eq!(vtt.valid(), Some(false));
        assert_eq!(issue, None);
    }

    #[test]
    fn webvtt_validates_cue_timestamps_and_ordering() {
        let (kind, _) = inspect_webvtt(
            b"WEBVTT\n\ncue\n00:01.000 --> 00:02.000 align:start\nText\n",
            true,
        );
        let InspectionKind::WebVtt(vtt) = kind else {
            panic!()
        };
        assert_eq!((vtt.valid(), vtt.cue_count()), (Some(true), Some(1)));

        for bytes in [
            b"WEBVTT\n\n00:60.000 --> 01:00.000\nText\n".as_slice(),
            b"WEBVTT\n\n00:02.000 --> 00:01.000\nText\n".as_slice(),
            b"WEBVTT\n\ngarbage\n".as_slice(),
        ] {
            let (kind, _) = inspect_webvtt(bytes, true);
            let InspectionKind::WebVtt(vtt) = kind else {
                panic!()
            };
            assert_eq!(vtt.valid(), Some(false));
        }
    }

    #[test]
    fn truncated_text_and_webvtt_do_not_report_conclusive_whole_resource_facts() {
        let text = inspect(b"plain utf-8 prefix", false);
        assert_eq!(text.encoding(), None);
        assert!(!text.has_byte_order_mark());

        let (kind, _) = inspect_webvtt(b"WEBVTT\n\n00:00.000 --> 00:01.000\nCue\n", false);
        let InspectionKind::WebVtt(vtt) = kind else {
            panic!()
        };
        assert_eq!(vtt.valid(), None);
        assert_eq!(vtt.cue_count(), None);
    }
}
