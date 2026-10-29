//! Bounded sidecar decoding. Declared charsets override legacy heuristics;
//! WebVTT remains UTF-8 and Scenarist SCC remains ASCII.
use anyhow::{Context, Result, bail};
use encoding_rs::{DecoderResult, Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252};

const LIMIT: usize = 8 * 1024 * 1024;
const HEADER_LIMIT: usize = 64 * 1024;

fn bounded_decode(encoding: &'static Encoding, bytes: &[u8]) -> Result<String> {
    let mut decoder = encoding.new_decoder_without_bom_handling();
    let mut output = String::new();
    let mut read = 0usize;
    let mut buffer = [0u8; 8192];
    loop {
        let (status, consumed, written) =
            decoder.decode_to_utf8_without_replacement(&bytes[read..], &mut buffer, true);
        read += consumed;
        if output
            .len()
            .checked_add(written)
            .is_none_or(|length| length > LIMIT)
        {
            bail!("decoded subtitle exceeds size limit");
        }
        output.push_str(
            std::str::from_utf8(&buffer[..written])
                .context("subtitle decoder produced invalid UTF-8")?,
        );
        match status {
            DecoderResult::InputEmpty => return Ok(output),
            DecoderResult::Malformed(_, _) => bail!("invalid subtitle character encoding"),
            DecoderResult::OutputFull if consumed != 0 || written != 0 => (),
            DecoderResult::OutputFull => bail!("subtitle decoder made no progress"),
        }
    }
}

fn attributes(tag: &str) -> Result<Vec<(String, String)>> {
    let bytes = tag.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() && !bytes[offset].is_ascii_whitespace() {
        offset += 1;
    }
    let mut result = Vec::new();
    while offset < bytes.len() {
        while offset < bytes.len()
            && (bytes[offset].is_ascii_whitespace() || matches!(bytes[offset], b'/' | b'?'))
        {
            offset += 1;
        }
        if offset == bytes.len() {
            break;
        }
        let start = offset;
        while offset < bytes.len()
            && (bytes[offset].is_ascii_alphanumeric()
                || matches!(bytes[offset], b'_' | b'-' | b':' | b'.'))
        {
            offset += 1;
        }
        if offset == start {
            bail!("invalid subtitle charset declaration");
        }
        let name = tag[start..offset].to_ascii_lowercase();
        while offset < bytes.len() && bytes[offset].is_ascii_whitespace() {
            offset += 1;
        }
        if offset == bytes.len() || bytes[offset] != b'=' {
            continue;
        }
        offset += 1;
        while offset < bytes.len() && bytes[offset].is_ascii_whitespace() {
            offset += 1;
        }
        if offset == bytes.len() {
            bail!("truncated subtitle charset declaration");
        }
        let value = if matches!(bytes[offset], b'\'' | b'"') {
            let quote = bytes[offset];
            offset += 1;
            let start = offset;
            while offset < bytes.len() && bytes[offset] != quote {
                offset += 1;
            }
            if offset == bytes.len() {
                bail!("truncated subtitle charset declaration");
            }
            let value = tag[start..offset].to_owned();
            offset += 1;
            value
        } else {
            let start = offset;
            while offset < bytes.len()
                && !bytes[offset].is_ascii_whitespace()
                && !matches!(bytes[offset], b'>' | b'?')
            {
                offset += 1;
            }
            tag[start..offset].to_owned()
        };
        if result.len() >= 64 {
            bail!("subtitle charset attribute limit exceeded");
        }
        result.push((name, value));
    }
    Ok(result)
}

fn tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut quote = None;
    for (index, &byte) in bytes.iter().enumerate().skip(start) {
        if let Some(active) = quote {
            if byte == active {
                quote = None;
            }
        } else if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else if byte == b'>' {
            return Some(index);
        }
    }
    None
}

fn declarations(extension: &str, text: &str) -> Result<Vec<String>> {
    if !matches!(extension, "xml" | "ttml" | "dfxp" | "smi" | "sami") {
        return Ok(Vec::new());
    }
    // Header syntax and charset labels are ASCII, even in ASCII-compatible
    // legacy encodings. Masking non-ASCII bytes avoids interpreting body text.
    let header = text
        .as_bytes()
        .iter()
        .take(HEADER_LIMIT)
        .map(|&byte| if byte.is_ascii() { byte } else { b'?' })
        .collect::<Vec<_>>();
    let text = std::str::from_utf8(&header)?;
    let lower = text.to_ascii_lowercase();
    let mut labels = Vec::new();
    let initial = lower.trim_start();
    if initial.starts_with("<?xml")
        && initial
            .as_bytes()
            .get(5)
            .is_some_and(u8::is_ascii_whitespace)
    {
        let end = initial
            .find("?>")
            .context("truncated XML encoding declaration")?;
        for (name, value) in
            attributes(&text[text.len() - initial.len() + 2..text.len() - initial.len() + end])?
        {
            if name == "encoding" {
                labels.push(value);
            }
        }
        if labels.len() > 1 {
            bail!("duplicate XML encoding declarations");
        }
    }
    if !matches!(extension, "smi" | "sami") {
        return Ok(labels);
    }
    let mut offset = 0;
    let mut tags = 0;
    while let Some(relative) = lower[offset..].find('<') {
        let begin = offset + relative;
        if lower[begin..].starts_with("<!--") {
            offset = begin
                + 4
                + lower[begin + 4..]
                    .find("-->")
                    .context("truncated SAMI header comment")?
                + 3;
            continue;
        }
        let end = tag_end(header.as_slice(), begin + 1).context("truncated SAMI header tag")?;
        let tag = &text[begin + 1..end];
        let name = tag
            .split(|character: char| character.is_ascii_whitespace() || character == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(name.as_str(), "body" | "sync") {
            break;
        }
        tags += 1;
        if tags > 1024 {
            bail!("SAMI header tag count exceeded");
        }
        if name == "meta" {
            for (key, value) in attributes(tag)? {
                if key == "charset" {
                    labels.push(value);
                } else if key == "content" {
                    for part in value.split(';') {
                        if let Some((key, value)) = part.split_once('=')
                            && key.trim().eq_ignore_ascii_case("charset")
                        {
                            labels.push(value.trim().trim_matches(['\'', '"']).to_owned());
                        }
                    }
                }
                if labels.len() > 32 {
                    bail!("subtitle charset declaration count exceeded");
                }
            }
        }
        offset = end + 1;
    }
    Ok(labels)
}

fn label_encoding(label: &str) -> Result<&'static Encoding> {
    if label.is_empty() || label.len() > 64 || !label.is_ascii() {
        bail!("invalid subtitle charset label");
    }
    let encoding = Encoding::for_label(label.trim().as_bytes())
        .context("unsupported subtitle character encoding")?;
    if encoding.name() == "replacement" {
        bail!("unsupported subtitle character encoding");
    }
    Ok(encoding)
}

fn declared(extension: &str, text: &str) -> Result<Option<&'static Encoding>> {
    let mut encoding: Option<&'static Encoding> = None;
    for label in declarations(extension, text)? {
        let next = label_encoding(&label)?;
        if encoding.is_some_and(|previous| !std::ptr::eq(previous, next)) {
            bail!("conflicting subtitle charset declarations");
        }
        encoding = Some(next);
    }
    Ok(encoding)
}

pub(crate) fn decode(extension: &str, bytes: &[u8]) -> Result<String> {
    if bytes.len() > LIMIT {
        bail!("subtitle input size limit exceeded");
    }
    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    let extension = extension.as_str();
    let (bom, content) = if let Some(content) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        (Some(UTF_8), content)
    } else if let Some(content) = bytes.strip_prefix(&[0xff, 0xfe]) {
        (Some(UTF_16LE), content)
    } else if let Some(content) = bytes.strip_prefix(&[0xfe, 0xff]) {
        (Some(UTF_16BE), content)
    } else {
        (None, bytes)
    };
    if matches!(extension, "vtt" | "webvtt" | "scc")
        && bom.is_some_and(|encoding| !std::ptr::eq(encoding, UTF_8))
    {
        bail!("subtitle format requires UTF-8 or ASCII");
    }
    let text = if let Some(encoding) = bom {
        let text = bounded_decode(encoding, content)?;
        for label in declarations(extension, &text)? {
            let explicit = label_encoding(&label)?;
            // A generic UTF-16 declaration may accompany either endian BOM.
            let generic_utf16 = label.trim().eq_ignore_ascii_case("utf-16")
                && (std::ptr::eq(encoding, UTF_16LE) || std::ptr::eq(encoding, UTF_16BE));
            if !generic_utf16 && !std::ptr::eq(explicit, encoding) {
                bail!("subtitle BOM conflicts with charset declaration");
            }
        }
        text
    } else {
        let ascii = content
            .iter()
            .take(HEADER_LIMIT)
            .map(|&byte| if byte.is_ascii() { byte } else { b'?' })
            .collect::<Vec<_>>();
        let encoding = declared(extension, std::str::from_utf8(&ascii)?)?;
        if encoding.is_some_and(|encoding| {
            std::ptr::eq(encoding, UTF_16LE) || std::ptr::eq(encoding, UTF_16BE)
        }) {
            bail!("UTF-16 subtitle requires a byte-order mark");
        }
        if let Some(encoding) = encoding {
            bounded_decode(encoding, content)?
        } else if let Ok(text) = std::str::from_utf8(content) {
            text.to_owned()
        } else if matches!(
            extension,
            "srt" | "smi" | "sami" | "ass" | "ssa" | "sub" | "txt" | "sbv"
        ) {
            bounded_decode(WINDOWS_1252, content)?
        } else {
            bail!("subtitle format requires valid UTF-8");
        }
    };
    if text.len() > LIMIT {
        bail!("decoded subtitle exceeds size limit");
    }
    if text.contains('\0') {
        bail!("subtitle contains null characters");
    }
    if extension == "scc" && !text.is_ascii() {
        bail!("SCC requires ASCII caption words");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_french_cp1252_and_valid_utf8_remain_readable() {
        assert_eq!(
            decode("srt", b"Fran\xe7ais: l\x92\xe9t\xe9 \x80").unwrap(),
            "Français: l’été €"
        );
        let unicode = "日本語 — café";
        assert_eq!(decode("srt", unicode.as_bytes()).unwrap(), unicode);
        assert_eq!(decode("srt", b"\xef\xbb\xbfBonjour").unwrap(), "Bonjour");
    }
    #[test]
    fn declared_shift_jis_and_windows_charsets_are_honored() {
        let mut xml = b"<?xml version='1.0' encoding='Shift_JIS'?><tt>".to_vec();
        xml.extend([0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea]);
        xml.extend(b"</tt>");
        assert!(decode("ttml", &xml).unwrap().contains("日本語"));
        let sami=b"<SAMI><HEAD><META HTTP-EQUIV=Content-Type CONTENT='text/html; charset=windows-1252'></HEAD><BODY>caf\xe9</BODY></SAMI>";
        assert!(decode("sami", sami).unwrap().contains("café"));
        let mut sami = b"<SAMI><HEAD><META charset=Shift_JIS></HEAD><BODY>".to_vec();
        sami.extend([0x93, 0xfa]);
        sami.extend(b"</BODY></SAMI>");
        assert!(decode("smi", &sami).unwrap().contains('日'));
    }
    #[test]
    fn unicode_boms_validate_utf16_surrogates_odd_bytes_and_declarations() {
        let source = "<?xml version='1.0' encoding='UTF-16'?><tt>日本語 😀</tt>";
        for (bom, big) in [([0xff, 0xfe], false), ([0xfe, 0xff], true)] {
            let mut encoded = bom.to_vec();
            for unit in source.encode_utf16() {
                encoded.extend(if big {
                    unit.to_be_bytes()
                } else {
                    unit.to_le_bytes()
                });
            }
            assert_eq!(decode("xml", &encoded).unwrap(), source);
        }
        assert!(decode("srt", &[0xff, 0xfe, 0]).is_err());
        assert!(decode("srt", &[0xff, 0xfe, 0, 0xd8]).is_err());
        assert!(decode("xml", b"\xef\xbb\xbf<?xml encoding='Shift_JIS'?><tt/>").is_err());
        assert!(decode("xml", b"<?xml encoding='UTF-16'?><tt/>").is_err());
    }
    #[test]
    fn unknown_conflicting_and_malformed_declarations_never_fall_back() {
        for source in [
            "<?xml encoding='unsupported-charset'?><tt/>",
            "<?xml encoding='UTF-8' encoding='windows-1252'?><tt/>",
            "<?xml encoding='UTF-8><tt/>",
        ] {
            assert!(decode("xml", source.as_bytes()).is_err());
        }
        assert!(decode("sami", b"<SAMI><META charset=unknown><BODY>ASCII</BODY>").is_err());
        assert!(
            decode(
                "sami",
                b"<SAMI><META charset=UTF-8><META charset=windows-1252><BODY>ASCII</BODY>"
            )
            .is_err()
        );
        assert!(decode("sami", b"<SAMI><META charset=Shift_JIS><BODY>\x81</BODY>").is_err());
        assert!(decode("xml", b"<tt>\xe9</tt>").is_err());
    }
    #[test]
    fn webvtt_is_utf8_only_and_scc_is_ascii() {
        assert!(decode("vtt", b"WEBVTT\n\ncaf\xe9").is_err());
        assert!(decode("webvtt", &[0xff, 0xfe, b'W', 0]).is_err());
        assert!(decode("scc", "Scenarist_SCC V1.0\né".as_bytes()).is_err());
        assert!(decode("scc", b"Scenarist_SCC V1.0\n\x80").is_err());
        assert!(decode("scc", b"\xef\xbb\xbfScenarist_SCC V1.0\n").is_ok());
    }
    #[test]
    fn comments_and_body_text_do_not_override_sami_encoding() {
        assert!(
            decode(
                "sami",
                b"<SAMI><!--<META charset=unknown>--><BODY><SYNC Start=0><P>caf\xe9</BODY>"
            )
            .unwrap()
            .contains("café")
        );
        assert!(
            decode(
                "sami",
                b"<SAMI><BODY><SYNC Start=0><P>charset=unknown</BODY>"
            )
            .is_ok()
        );
    }
    #[test]
    fn input_and_expanded_output_are_bounded_without_replacement() {
        assert!(decode("srt", &vec![b'a'; LIMIT + 1]).is_err());
        assert!(decode("srt", &vec![0x80; LIMIT / 3 + 1]).is_err());
        assert_eq!(decode("srt", &vec![b'a'; LIMIT]).unwrap().len(), LIMIT);
        assert!(decode("srt", b"binary\0data").is_err());
        assert!(decode("sami", b"<SAMI><META charset=UTF-8><BODY>\xff</BODY>").is_err());
    }
}
