use super::{AnalysisIssue, FontFormat, InspectionKind};
use crate::analysis::inspection::Font;

pub(super) fn inspect(
    bytes: &[u8],
    format: FontFormat,
    complete: bool,
) -> (InspectionKind, Option<AnalysisIssue>) {
    if matches!(format, FontFormat::Woff | FontFormat::Woff2) {
        let valid = match format {
            FontFormat::Woff => valid_woff(bytes, complete),
            FontFormat::Woff2 => valid_woff2(bytes, complete),
            _ => unreachable!(),
        };
        let collection_count = valid.then_some(1);
        return (
            InspectionKind::Font(Font::new(
                format,
                Vec::new(),
                Vec::new(),
                None,
                collection_count,
                None,
            )),
            (complete && !valid).then_some(AnalysisIssue::Malformed),
        );
    }
    let count = ttf_parser::fonts_in_collection(bytes).unwrap_or(1);
    const MAX_COLLECTION_FACES: u32 = 4_096;
    let collection = format == FontFormat::Ttc;
    let available_faces = if collection {
        bytes.len().saturating_sub(12) / 4
    } else {
        1
    };
    let structurally_truncated = collection && count as usize > available_faces;
    let limited = collection && count > MAX_COLLECTION_FACES;
    let parse_count = count
        .min(u32::try_from(available_faces).unwrap_or(u32::MAX))
        .min(MAX_COLLECTION_FACES);
    let mut families = Vec::new();
    let mut subfamilies = Vec::new();
    let mut glyph_count = None;
    let mut variable = Some(false);
    let mut parsed = 0u32;
    for index in 0..parse_count {
        let Ok(face) = ttf_parser::Face::parse(bytes, index) else {
            continue;
        };
        parsed += 1;
        glyph_count = Some(
            glyph_count
                .unwrap_or(0u32)
                .saturating_add(face.number_of_glyphs() as u32),
        );
        variable = Some(variable.unwrap_or(false) || face.is_variable());
        for name in face.names() {
            let Some(value) = name.to_string() else {
                continue;
            };
            if matches!(
                name.name_id,
                ttf_parser::name_id::FAMILY | ttf_parser::name_id::TYPOGRAPHIC_FAMILY
            ) {
                push_unique(&mut families, value);
            } else if matches!(
                name.name_id,
                ttf_parser::name_id::SUBFAMILY | ttf_parser::name_id::TYPOGRAPHIC_SUBFAMILY
            ) {
                push_unique(&mut subfamilies, value);
            }
        }
    }
    if parsed == 0 {
        variable = None;
    }
    let issue = if limited {
        Some(AnalysisIssue::Unsupported)
    } else if complete && (parsed == 0 || structurally_truncated || parsed != count) {
        Some(AnalysisIssue::Malformed)
    } else {
        None
    };
    (
        InspectionKind::Font(Font::new(
            format,
            families,
            subfamilies,
            glyph_count,
            collection.then_some(count),
            variable,
        )),
        issue,
    )
}

fn valid_woff(bytes: &[u8], complete: bool) -> bool {
    if bytes.len() < 44 || !bytes.starts_with(b"wOFF") {
        return false;
    }
    let declared_len = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let table_count = u16::from_be_bytes(bytes[12..14].try_into().unwrap()) as usize;
    let reserved = u16::from_be_bytes(bytes[14..16].try_into().unwrap());
    let Some(directory_end) = table_count
        .checked_mul(20)
        .and_then(|length| 44usize.checked_add(length))
    else {
        return false;
    };
    if table_count == 0
        || reserved != 0
        || directory_end > bytes.len()
        || (complete && declared_len != bytes.len())
        || declared_len < directory_end
    {
        return false;
    }
    for entry in bytes[44..directory_end].chunks_exact(20) {
        let offset = u32::from_be_bytes(entry[4..8].try_into().unwrap()) as usize;
        let compressed_len = u32::from_be_bytes(entry[8..12].try_into().unwrap()) as usize;
        let original_len = u32::from_be_bytes(entry[12..16].try_into().unwrap()) as usize;
        if compressed_len > original_len
            || offset
                .checked_add(compressed_len)
                .is_none_or(|end| end > declared_len || (complete && end > bytes.len()))
        {
            return false;
        }
    }
    true
}

fn valid_woff2(bytes: &[u8], complete: bool) -> bool {
    if bytes.len() < 48 || !bytes.starts_with(b"wOF2") {
        return false;
    }
    let declared_len = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let table_count = u16::from_be_bytes(bytes[12..14].try_into().unwrap());
    let reserved = u16::from_be_bytes(bytes[14..16].try_into().unwrap());
    let total_sfnt_size = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let compressed_size = u32::from_be_bytes(bytes[20..24].try_into().unwrap()) as usize;
    table_count != 0
        && reserved == 0
        && total_sfnt_size != 0
        && declared_len >= 48
        && 48usize
            .checked_add(compressed_size)
            .is_some_and(|end| end <= declared_len && (!complete || end <= bytes.len()))
        && (!complete || declared_len == bytes.len())
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_wrappers_and_unbounded_ttc_counts_are_partial() {
        let mut woff = vec![0; 44];
        woff[..4].copy_from_slice(b"wOFF");
        woff[8..12].copy_from_slice(&44u32.to_be_bytes());
        woff[12..14].copy_from_slice(&1u16.to_be_bytes());
        let (_, issue) = inspect(&woff, FontFormat::Woff, true);
        assert_eq!(issue, Some(AnalysisIssue::Malformed));

        let mut ttc = b"ttcf\0\x01\0\0".to_vec();
        ttc.extend_from_slice(&u32::MAX.to_be_bytes());
        let (_, issue) = inspect(&ttc, FontFormat::Ttc, true);
        assert_eq!(issue, Some(AnalysisIssue::Unsupported));
    }
}
