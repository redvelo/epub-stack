use super::{AnalysisIssue, InspectionKind, MediaContainer};
use crate::analysis::inspection::{Media, MediaDuration, MediaTrack, MediaTrackKind};
use std::io::Cursor;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioCodecId, well_known as audio_codecs};
use symphonia::core::codecs::video::{VideoCodecId, well_known as video_codecs};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::well_known::{
    FORMAT_ID_ADTS, FORMAT_ID_ISOMP4, FORMAT_ID_MKV, FORMAT_ID_MP3, FORMAT_ID_OGG,
};
use symphonia::core::formats::{FormatId, FormatOptions, Track, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Duration, TimeBase, Timestamp};

pub(super) fn inspect(
    bytes: &[u8],
    container: MediaContainer,
    complete: bool,
) -> (InspectionKind, Option<AnalysisIssue>) {
    let name = media_container_name(container);
    // A non-seekable MP3 source prevents Symphonia from estimating duration from file size.
    let source: Box<dyn MediaSource + '_> = if complete && container != MediaContainer::Mp3 {
        Box::new(Cursor::new(bytes))
    } else {
        Box::new(ReadOnlySource::new(Cursor::new(bytes)))
    };
    let stream = MediaSourceStream::new(source, Default::default());
    let mut hint = Hint::new();
    hint.with_extension(media_extension(container));
    hint.mime_type(media_mime_type(container));

    let format = symphonia::default::get_probe().probe(
        &hint,
        stream,
        FormatOptions::default().prebuild_seek_index(false),
        MetadataOptions::default(),
    );
    let format = match format {
        Ok(format) => format,
        Err(error) => {
            return (
                InspectionKind::Media(Media::new(name.to_string(), None, Vec::new())),
                Some(symphonia_issue(error)),
            );
        }
    };
    if format.format_info().format != expected_format_id(container) {
        return (
            InspectionKind::Media(Media::new(name.to_string(), None, Vec::new())),
            Some(AnalysisIssue::Malformed),
        );
    }

    let duration_is_reliable = complete && container != MediaContainer::AacAdts;
    let duration = duration_is_reliable
        .then(|| media_duration(format.media_info().time_base, format.media_info().duration))
        .flatten();
    let tracks = format
        .tracks()
        .iter()
        .map(|track| inspect_symphonia_track(track, duration_is_reliable))
        .collect();
    (
        InspectionKind::Media(Media::new(name.to_string(), duration, tracks)),
        None,
    )
}

fn inspect_symphonia_track(track: &Track, duration_is_reliable: bool) -> MediaTrack {
    let duration = duration_is_reliable
        .then(|| media_duration(track.time_base, track.duration))
        .flatten();
    let (codec, sample_rate, channels, width, height) = match track.codec_params.as_ref() {
        Some(CodecParameters::Audio(params)) => (
            audio_codec_name(params.codec).map(str::to_string),
            params.sample_rate,
            params
                .channels
                .as_ref()
                .and_then(|channels| u16::try_from(channels.count()).ok()),
            None,
            None,
        ),
        Some(CodecParameters::Video(params)) => (
            video_codec_name(params.codec).map(str::to_string),
            None,
            None,
            params.width.map(u32::from),
            params.height.map(u32::from),
        ),
        Some(CodecParameters::Subtitle(_)) | None => (None, None, None, None, None),
        _ => (None, None, None, None, None),
    };
    let kind = match track.track_type() {
        Some(TrackType::Audio) => MediaTrackKind::Audio,
        Some(TrackType::Video) => MediaTrackKind::Video,
        Some(TrackType::Subtitle) => MediaTrackKind::Subtitle,
        Some(_) | None => MediaTrackKind::Unknown,
    };
    MediaTrack::new(
        kind,
        codec,
        track.language.clone(),
        duration,
        sample_rate,
        channels,
        width,
        height,
    )
}

fn media_duration(
    time_base: Option<TimeBase>,
    duration: Option<Duration>,
) -> Option<MediaDuration> {
    let timestamp = duration?.timestamp_from(Timestamp::ZERO)?;
    let milliseconds = time_base?.calc_time(timestamp)?.as_millis();
    u64::try_from(milliseconds).ok().map(MediaDuration::new)
}

fn audio_codec_name(codec: AudioCodecId) -> Option<&'static str> {
    match codec {
        audio_codecs::CODEC_ID_MP3 => Some("mp3"),
        audio_codecs::CODEC_ID_AAC => Some("aac"),
        audio_codecs::CODEC_ID_OPUS => Some("opus"),
        audio_codecs::CODEC_ID_VORBIS => Some("vorbis"),
        _ => None,
    }
}

fn video_codec_name(codec: VideoCodecId) -> Option<&'static str> {
    match codec {
        video_codecs::CODEC_ID_H264 => Some("h264"),
        video_codecs::CODEC_ID_AV1 => Some("av1"),
        video_codecs::CODEC_ID_VP8 => Some("vp8"),
        video_codecs::CODEC_ID_VP9 => Some("vp9"),
        _ => None,
    }
}

fn symphonia_issue(error: SymphoniaError) -> AnalysisIssue {
    match error {
        SymphoniaError::Unsupported(message) if malformed_unsupported_message(message) => {
            AnalysisIssue::Malformed
        }
        SymphoniaError::Unsupported(_) => AnalysisIssue::Unsupported,
        SymphoniaError::SeekError(_) => AnalysisIssue::RandomAccessUnavailable,
        SymphoniaError::IoError(_) | SymphoniaError::DecodeError(_) => AnalysisIssue::Malformed,
        SymphoniaError::LimitError(_) | SymphoniaError::ResetRequired => {
            AnalysisIssue::ParserFailure
        }
        _ => AnalysisIssue::ParserFailure,
    }
}

fn malformed_unsupported_message(message: &str) -> bool {
    matches!(
        message,
        "core (probe): no suitable format reader found"
            | "isomp4: missing ftyp atom"
            | "isomp4: missing moov atom"
            | "ogg: page is not marked as first"
            | "mkv: not a matroska / webm file"
            | "mkv: missing segment element"
    )
}

fn expected_format_id(container: MediaContainer) -> FormatId {
    match container {
        MediaContainer::Mp3 => FORMAT_ID_MP3,
        MediaContainer::AacAdts => FORMAT_ID_ADTS,
        MediaContainer::Ogg => FORMAT_ID_OGG,
        MediaContainer::Mp4 => FORMAT_ID_ISOMP4,
        MediaContainer::WebM => FORMAT_ID_MKV,
    }
}

fn media_container_name(container: MediaContainer) -> &'static str {
    match container {
        MediaContainer::Mp3 => "mp3",
        MediaContainer::AacAdts => "adts",
        MediaContainer::Ogg => "ogg",
        MediaContainer::Mp4 => "mp4",
        MediaContainer::WebM => "webm",
    }
}

fn media_extension(container: MediaContainer) -> &'static str {
    match container {
        MediaContainer::Mp3 => "mp3",
        MediaContainer::AacAdts => "aac",
        MediaContainer::Ogg => "ogg",
        MediaContainer::Mp4 => "mp4",
        MediaContainer::WebM => "webm",
    }
}

fn media_mime_type(container: MediaContainer) -> &'static str {
    match container {
        MediaContainer::Mp3 => "audio/mpeg",
        MediaContainer::AacAdts => "audio/aac",
        MediaContainer::Ogg => "application/ogg",
        MediaContainer::Mp4 => "application/mp4",
        MediaContainer::WebM => "video/webm",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symphonia_inspects_audio_fixtures_without_decoding() {
        #[derive(Clone, Copy)]
        enum DurationExpectation {
            Exact(u64),
            Present,
            Absent,
        }
        type AudioCase = (
            &'static [u8],
            MediaContainer,
            &'static str,
            u32,
            Option<u16>,
            DurationExpectation,
        );
        let cases: &[AudioCase] = &[
            (
                include_bytes!("../../../tests/fixtures/media-cbr.mp3"),
                MediaContainer::Mp3,
                "mp3",
                44_100,
                Some(1),
                DurationExpectation::Exact(250),
            ),
            (
                include_bytes!("../../../tests/fixtures/media-vbr.mp3"),
                MediaContainer::Mp3,
                "mp3",
                44_100,
                Some(1),
                DurationExpectation::Exact(400),
            ),
            (
                include_bytes!("../../../tests/fixtures/media.aac"),
                MediaContainer::AacAdts,
                "aac",
                48_000,
                Some(1),
                DurationExpectation::Absent,
            ),
            (
                include_bytes!("../../../tests/fixtures/media-opus.ogg"),
                MediaContainer::Ogg,
                "opus",
                48_000,
                Some(1),
                DurationExpectation::Present,
            ),
            (
                include_bytes!("../../../tests/fixtures/media-vorbis.ogg"),
                MediaContainer::Ogg,
                "vorbis",
                44_100,
                Some(1),
                DurationExpectation::Present,
            ),
            (
                include_bytes!("../../../tests/fixtures/media-opus.mp4"),
                MediaContainer::Mp4,
                "opus",
                48_000,
                None,
                DurationExpectation::Exact(300),
            ),
        ];

        for (bytes, container, codec, sample_rate, channels, duration) in cases {
            let (kind, issue) = inspect(bytes, *container, true);
            assert_eq!(issue, None, "{container:?}");
            let InspectionKind::Media(media) = kind else {
                panic!("expected media inspection")
            };
            assert_eq!(media.tracks().len(), 1, "{container:?}");
            let track = &media.tracks()[0];
            assert_eq!(track.kind(), MediaTrackKind::Audio);
            assert_eq!(track.codec(), Some(*codec));
            assert_eq!(track.sample_rate(), Some(*sample_rate));
            assert_eq!(track.channels(), *channels);
            match duration {
                DurationExpectation::Exact(milliseconds) => assert_eq!(
                    media.duration().map(MediaDuration::milliseconds),
                    Some(*milliseconds)
                ),
                DurationExpectation::Present => assert!(media.duration().is_some()),
                DurationExpectation::Absent => assert_eq!(media.duration(), None),
            }
        }
    }

    #[test]
    fn symphonia_inspects_multitrack_video_fixtures() {
        let cases: &[(&[u8], MediaContainer, &str, &str)] = &[
            (
                include_bytes!("../../../tests/fixtures/media-h264-aac.mp4"),
                MediaContainer::Mp4,
                "h264",
                "aac",
            ),
            (
                include_bytes!("../../../tests/fixtures/media-h264-aac-tail.mp4"),
                MediaContainer::Mp4,
                "h264",
                "aac",
            ),
            (
                include_bytes!("../../../tests/fixtures/media-vp9-opus.webm"),
                MediaContainer::WebM,
                "vp9",
                "opus",
            ),
        ];

        for (bytes, container, video_codec, audio_codec) in cases {
            let (kind, issue) = inspect(bytes, *container, true);
            assert_eq!(issue, None, "{container:?}");
            let InspectionKind::Media(media) = kind else {
                panic!("expected media inspection")
            };
            assert_eq!(media.tracks().len(), 2, "{container:?}");
            let video = media
                .tracks()
                .iter()
                .find(|track| track.kind() == MediaTrackKind::Video)
                .expect("video track");
            let audio = media
                .tracks()
                .iter()
                .find(|track| track.kind() == MediaTrackKind::Audio)
                .expect("audio track");
            assert_eq!(video.codec(), Some(*video_codec));
            assert_eq!((video.width(), video.height()), (Some(32), Some(24)));
            assert_eq!(audio.codec(), Some(*audio_codec));
            assert_eq!(audio.sample_rate(), Some(48_000));
            assert!(media.duration().is_some());
        }
    }

    #[test]
    fn symphonia_identifies_webm_video_codecs() {
        let cases: &[(&[u8], &str)] = &[
            (
                include_bytes!("../../../tests/fixtures/media-vp8.webm"),
                "vp8",
            ),
            (
                include_bytes!("../../../tests/fixtures/media-av1.webm"),
                "av1",
            ),
        ];
        for (bytes, codec) in cases {
            let (kind, issue) = inspect(bytes, MediaContainer::WebM, true);
            assert_eq!(issue, None, "{codec}");
            let InspectionKind::Media(media) = kind else {
                panic!("expected media inspection")
            };
            assert_eq!(media.tracks().len(), 1);
            let track = &media.tracks()[0];
            assert_eq!(track.codec(), Some(*codec));
            assert_eq!((track.width(), track.height()), (Some(32), Some(24)));
        }
    }

    #[test]
    fn symphonia_rejects_a_different_probed_container() {
        let mut bytes = vec![b'x'; 32];
        bytes.extend_from_slice(include_bytes!("../../../tests/fixtures/media-opus.ogg"));
        let (kind, issue) = inspect(&bytes, MediaContainer::Mp4, true);
        assert_eq!(issue, Some(AnalysisIssue::Malformed));
        let InspectionKind::Media(media) = kind else {
            panic!("expected media inspection")
        };
        assert_eq!(media.container(), "mp4");
        assert!(media.tracks().is_empty());
    }

    #[test]
    fn missing_required_mp4_structure_is_malformed_not_unsupported() {
        let bytes = b"\0\0\0\x18ftypisom\0\0\0\0isomiso2";
        let (_, issue) = inspect(bytes, MediaContainer::Mp4, true);
        assert_eq!(issue, Some(AnalysisIssue::Malformed));
    }
}
