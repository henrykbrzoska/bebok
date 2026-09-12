//! Shared image-attachment validation (single source of truth, Pakiet C3).
//!
//! Used by `POST /session/{id}/prompt` (`bebok-server::services::turn`),
//! the `task` tool and the `fleet` tool. Limits:
//! - max 5 images per prompt (`MAX_IMAGES_PER_PROMPT`)
//! - max 5 MiB decoded per image (`MAX_IMAGE_BYTES`)
//! - MIME allow-list png/jpeg/webp/gif (`ALLOWED_IMAGE_TYPES`)
//!
//! Validation is *payload-based*, not client-trust-based: the base64 is
//! strictly decoded, the decoded byte length is checked, and the magic bytes
//! must match the declared `media_type`. A JPEG relabelled `image/png` is
//! rejected instead of being forwarded to the provider.
//!
//! For PNG images, non-critical metadata chunks (tEXt, iTXt, zTXt, gAMA,
//! etc.) are stripped before sending to the provider. Some providers (e.g.
//! Xiaomi) reject PNGs containing AIGC metadata in tEXt chunks. Only the
//! critical chunks (IHDR, PLTE, IDAT, IEND) are kept.

use serde::Deserialize;

/// Max images per prompt (client `MAX_IMAGES` mirrors this).
pub const MAX_IMAGES_PER_PROMPT: usize = 5;
/// Max decoded bytes per image (5 MiB; client `MAX_IMAGE_BYTES` mirrors this).
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Max base64 chars per image: `ceil(5 MiB / 3) * 4 = 6_990_508`.
/// Cheap pre-decode guard so an oversized payload is rejected before it is
/// materialized as `Vec<u8>` (the authoritative limit is `MAX_IMAGE_BYTES`).
pub const MAX_IMAGE_BASE64_LEN: usize = 6_990_508;
/// Allowed image MIME types (client `ACCEPTED_IMAGE_TYPES` mirrors this).
pub const ALLOWED_IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// One raw image attachment (wire shape shared by prompt/task/fleet).
#[derive(Debug, Clone, Deserialize)]
pub struct AgentImageInput {
    #[serde(default)]
    pub media_type: String,
    #[serde(default)]
    pub data: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Pre-flight capability check: does this model accept image input?
///
/// The embedded model catalog is authoritative for known models. Unknown
/// models use the catalog's permissive fallback and are never blocked.
pub fn model_supports_images(model: &str) -> bool {
    bebok_llm::ModelCatalog::global().get(model).supports_images
}

/// Strip ASCII whitespace (clients may wrap base64 across lines).
fn squeeze(input: &str) -> String {
    input.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Strict base64 decode (standard alphabet, `=` padding). Returns `None` for
/// any non-alphabet byte, bad padding, or a length that is not a multiple of 4.
/// Hand-rolled to avoid a new dependency for one call site.
fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (i, chunk) in bytes.chunks(4).enumerate() {
        let last = (i + 1) * 4 == bytes.len();
        // Padding is positional: 0/1/2 trailing `=` in the final chunk only.
        let pad = if chunk[3] == b'=' {
            if chunk[2] == b'=' { 2 } else { 1 }
        } else {
            0
        };
        if pad > 0 && !last {
            return None;
        }
        if chunk[..4 - pad].contains(&b'=') {
            return None;
        }
        let mut vals = [0u32; 4];
        for (k, &c) in chunk.iter().enumerate() {
            vals[k] = if c == b'=' { 0 } else { val(c)? };
        }
        let triple = (vals[0] << 18) | (vals[1] << 12) | (vals[2] << 6) | vals[3];
        let n = 3 - pad;
        let b = [(triple >> 16) as u8, (triple >> 8) as u8, triple as u8];
        out.extend_from_slice(&b[..n]);
    }
    Some(out)
}

/// Base64-encode a byte slice (standard alphabet with `=` padding).
/// Hand-rolled to stay dependency-free.
fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Detect the image type from magic bytes (payload-based, not client-declared).
fn detect_image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

// ── PNG metadata stripping ──────────────────────────────────────────────────

/// Critical PNG chunk types that must be preserved.
/// Per the PNG spec, chunk types whose first letter is uppercase are critical.
const CRITICAL_PNG_CHUNKS: &[&[u8; 4]] = &[b"IHDR", b"PLTE", b"IDAT", b"IEND"];

/// Strip non-critical metadata chunks from a PNG byte stream.
///
/// PNG files consist of an 8-byte signature followed by chunks.
/// Each chunk: `[4-byte length][4-byte type][data][4-byte CRC]`.
///
/// Critical chunks (IHDR, PLTE, IDAT, IEND) are always kept.
/// Ancillary chunks (tEXt, iTXt, zTXt, gAMA, cHRM, sRGB, iCCP, sBIT,
/// bKGD, hIST, pHYS, tIME, etc.) are dropped. This is safe per the PNG
/// spec and strips AIGC / EXIF / author metadata that some providers reject.
///
/// Returns the cleaned PNG bytes, or `None` if the input is malformed.
fn strip_png_metadata(png: &[u8]) -> Option<Vec<u8>> {
    // PNG signature: 8 bytes.
    if png.len() < 8 {
        return None;
    }
    if png[..8] != [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return None;
    }

    let mut out = Vec::with_capacity(png.len());
    out.extend_from_slice(&png[..8]); // signature

    let mut pos = 8;
    while pos + 8 <= png.len() {
        let len = u32::from_be_bytes([png[pos], png[pos + 1], png[pos + 2], png[pos + 3]]) as usize;
        let chunk_type = &png[pos + 4..pos + 8];

        // Bounds check: 4 (len) + 4 (type) + len (data) + 4 (crc).
        let end = pos + 12 + len;
        if end > png.len() {
            return None; // truncated chunk
        }

        let is_critical = CRITICAL_PNG_CHUNKS
            .iter()
            .any(|c| c.as_slice() == chunk_type);

        if is_critical {
            out.extend_from_slice(&png[pos..end]);
        }
        // else: skip ancillary chunk (tEXt, iTXt, zTXt, gAMA, …)

        pos = end;
    }

    Some(out)
}

/// Count the number of PNG data (IDAT) chunks in a PNG byte stream.
/// Used in tests to verify that stripping ancillary chunks does not touch IDATs.
#[cfg(test)]
fn count_idat_chunks(png: &[u8]) -> usize {
    let mut count = 0;
    let mut pos = 8; // skip signature
    while pos + 8 <= png.len() {
        let len = u32::from_be_bytes([png[pos], png[pos + 1], png[pos + 2], png[pos + 3]]) as usize;
        let chunk_type = &png[pos + 4..pos + 8];
        if chunk_type == b"IDAT" {
            count += 1;
        }
        let end = pos + 12 + len;
        if end > png.len() {
            break;
        }
        pos = end;
    }
    count
}

/// CRC-32 (ISO 3309) used by the PNG chunk checksums.
/// Polynomial 0xEDB88320 (bit-reflected), same as zlib/png.
#[cfg(test)]
fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB88320
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    data.iter().fold(0xFFFFFFFF, |crc, &b| {
        (crc >> 8) ^ table[((crc ^ b as u32) & 0xFF) as usize]
    }) ^ 0xFFFFFFFF
}

#[cfg(test)]
fn make_chunk(chunk_type: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut chunk = Vec::new();
    chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
    chunk.extend_from_slice(chunk_type);
    chunk.extend_from_slice(data);
    let crc = crc32(&[chunk_type.as_slice(), data].concat());
    chunk.extend_from_slice(&crc.to_be_bytes());
    chunk
}

/// Minimal deflate wrapper (stored/raw block, no compression).
#[cfg(test)]
fn deflate_minimal(data: &[u8]) -> Vec<u8> {
    let len = data.len() as u16;
    let nlen = !len;
    let mut out = Vec::with_capacity(5 + data.len());
    out.push(0x08); // BFINAL=1, BTYPE=00 (stored)
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&nlen.to_le_bytes());
    out.extend_from_slice(data);
    out
}

// ── Validation entry point ──────────────────────────────────────────────────

/// Validate raw image attachments into session `Part::Image` values.
///
/// - Strips a `data:<mime>;base64,` prefix when present (adopts the MIME
///   when the field is empty).
/// - Skips entries with empty data (after strip + trim).
/// - Rejects disallowed MIME types, invalid base64, payloads whose decoded
///   size exceeds 5 MB, and payloads whose magic bytes disagree with the
///   declared MIME. More than 5 images is rejected too.
/// - For PNG images, strips non-critical metadata chunks (tEXt, gAMA, etc.)
///   before re-encoding to base64. Some providers reject PNGs containing AIGC
///   metadata.
/// - Errors are human-readable (`5 MB` / `images` appear verbatim) so the
///   HTTP layer can map them to 400 and the tools can surface them.
pub fn validate_agent_images(
    images: Vec<AgentImageInput>,
) -> Result<Vec<crate::session::Part>, String> {
    let mut out = Vec::new();
    for mut img in images {
        let mut data = img.data.trim().to_string();
        // Accept a data: URL prefix: data:<mime>;base64,<payload>.
        if let Some(rest) = data.strip_prefix("data:")
            && let Some(comma) = rest.find(',')
        {
            let (meta, payload) = rest.split_at(comma);
            let payload = payload[1..].trim().to_string();
            // Adopt the MIME from the prefix when the field is empty.
            if img.media_type.trim().is_empty() {
                img.media_type = meta.split(';').next().unwrap_or("").trim().to_string();
            }
            data = payload;
        }
        data = squeeze(&data);
        if data.is_empty() {
            continue; // skip empty data
        }
        let name = img
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "unnamed".to_string());
        if out.len() >= MAX_IMAGES_PER_PROMPT {
            return Err(format!(
                "too many images: max {MAX_IMAGES_PER_PROMPT} images per prompt (png/jpeg/webp/gif)"
            ));
        }
        let media_type = img.media_type.trim().to_lowercase();
        if !ALLOWED_IMAGE_TYPES.contains(&media_type.as_str()) {
            return Err(format!(
                "unsupported image media_type '{media_type}': expected one of image/png, image/jpeg, image/webp, image/gif"
            ));
        }
        if data.len() > MAX_IMAGE_BASE64_LEN {
            return Err(format!(
                "image '{name}' too large: {} base64 chars (max 5 MB)",
                data.len()
            ));
        }
        let decoded =
            decode_base64(&data).ok_or_else(|| format!("image '{name}' is not valid base64"))?;
        if decoded.len() > MAX_IMAGE_BYTES {
            return Err(format!(
                "image '{name}' too large: {} bytes decoded (max 5 MB)",
                decoded.len()
            ));
        }
        match detect_image_type(&decoded) {
            None => {
                return Err(format!(
                    "image '{name}' is not a valid png/jpeg/webp/gif payload"
                ));
            }
            Some(detected) if detected != media_type => {
                return Err(format!(
                    "image '{name}' declares '{media_type}' but its payload is '{detected}'"
                ));
            }
            Some(_) => {}
        }

        // Strip non-critical PNG metadata chunks (tEXt with AIGC, gAMA, etc.)
        // and re-encode to base64. This fixes rejections from providers (e.g.
        // Xiaomi) that choke on ancillary PNG metadata.
        let final_data = if media_type == "image/png" {
            if let Some(clean) = strip_png_metadata(&decoded) {
                encode_base64(&clean)
            } else {
                data // malformed PNG; keep original — validation will catch it later
            }
        } else {
            data
        };

        out.push(crate::session::Part::Image {
            media_type,
            data: final_data,
            name: img.name.filter(|n| !n.trim().is_empty()),
        });
    }
    Ok(out)
}

/// Minimal, decodable image payloads for tests (real magic bytes).
#[cfg(test)]
pub mod fixtures {
    /// 1x1 PNG (68 bytes, valid PNG signature).
    pub const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
    /// Minimal JPEG: SOI + APP0/JFIF header + EOI (22 bytes).
    pub const JPEG_MIN: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2Q==";
    /// Minimal GIF89a header + trailer (44 bytes).
    pub const GIF_MIN: &str = "R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAICRAEAOw==";
    /// Minimal RIFF/WEBP header (18 bytes).
    pub const WEBP_MIN: &str = "UklGRhoAAABXRUJQVlA4IA==";
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn img(media_type: &str, data: &str) -> AgentImageInput {
        AgentImageInput {
            media_type: media_type.to_string(),
            data: data.to_string(),
            name: None,
        }
    }

    #[test]
    fn png_jpeg_webp_gif_accepted() {
        let cases = [
            ("image/png", PNG_1X1),
            ("image/jpeg", JPEG_MIN),
            ("image/webp", WEBP_MIN),
            ("image/gif", GIF_MIN),
        ];
        for (mime, data) in cases {
            let out = validate_agent_images(vec![img(mime, data)])
                .unwrap_or_else(|e| panic!("{mime} rejected: {e}"));
            assert_eq!(out.len(), 1);
            match &out[0] {
                crate::session::Part::Image { media_type, .. } => assert_eq!(media_type, mime),
                other => panic!("expected image part, got {other:?}"),
            }
        }
    }

    #[test]
    fn five_images_ok_sixth_rejected() {
        let five: Vec<AgentImageInput> = (0..5).map(|_| img("image/png", PNG_1X1)).collect();
        assert_eq!(validate_agent_images(five).unwrap().len(), 5);
        let six: Vec<AgentImageInput> = (0..6).map(|_| img("image/png", PNG_1X1)).collect();
        let err = validate_agent_images(six).unwrap_err();
        assert!(err.contains("max 5 images"), "{err}");
    }

    #[test]
    fn bad_mime_rejected() {
        let err = validate_agent_images(vec![img("image/bmp", PNG_1X1)]).unwrap_err();
        assert!(err.contains("unsupported image media_type"), "{err}");
    }

    #[test]
    fn data_url_prefix_stripped() {
        let raw = AgentImageInput {
            media_type: String::new(),
            data: format!("data:image/png;base64,{PNG_1X1}"),
            name: None,
        };
        let out = validate_agent_images(vec![raw]).unwrap();
        assert_eq!(out.len(), 1);
        match &out[0] {
            crate::session::Part::Image {
                media_type, data, ..
            } => {
                assert_eq!(media_type, "image/png");
                // data may differ from original if metadata was stripped
                assert!(!data.is_empty());
            }
            other => panic!("expected image part, got {other:?}"),
        }
    }

    #[test]
    fn empty_data_skipped() {
        let out = validate_agent_images(vec![img("image/png", "   ")]).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn oversized_rejected_with_5mb_message() {
        let big = "A".repeat(MAX_IMAGE_BASE64_LEN + 1);
        let err = validate_agent_images(vec![img("image/png", &big)]).unwrap_err();
        assert!(err.contains("max 5 MB"), "{err}");
    }

    #[test]
    fn decoded_size_limit_is_authoritative() {
        // Valid base64 whose decoded size exceeds 5 MiB.
        let big = "A".repeat((MAX_IMAGE_BYTES / 3 + 2) * 4);
        let err = validate_agent_images(vec![img("image/png", &big)]).unwrap_err();
        assert!(err.contains("max 5 MB"), "{err}");
    }

    #[test]
    fn invalid_base64_rejected() {
        let err = validate_agent_images(vec![img("image/png", "!!!!")]).unwrap_err();
        assert!(err.contains("not valid base64"), "{err}");
    }

    #[test]
    fn mime_spoof_rejected_by_magic_bytes() {
        // A JPEG payload declared as PNG must not reach the provider.
        let err = validate_agent_images(vec![img("image/png", JPEG_MIN)]).unwrap_err();
        assert!(err.contains("declares 'image/png'"), "{err}");
        assert!(err.contains("'image/jpeg'"), "{err}");
    }

    #[test]
    fn non_image_payload_rejected() {
        // "eA==" decodes to the single byte 'x' - valid base64, not an image.
        let err = validate_agent_images(vec![img("image/png", "eA==")]).unwrap_err();
        assert!(
            err.contains("not a valid png/jpeg/webp/gif payload"),
            "{err}"
        );
    }

    #[test]
    fn whitespace_in_base64_tolerated() {
        let wrapped = format!("{}\n{}\n", &PNG_1X1[..40], &PNG_1X1[40..]);
        let out = validate_agent_images(vec![img("image/png", &wrapped)]).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn base64_roundtrip_matches_expected_bytes() {
        // Guards the hand-rolled decoder against regressions.
        assert_eq!(decode_base64("eA==").unwrap(), vec![b'x']);
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello".to_vec());
        assert!(decode_base64("eA").is_none());
        assert!(decode_base64("eA=A").is_none());
        assert!(decode_base64("a===").is_none());
    }

    #[test]
    fn magic_bytes_recognized() {
        assert_eq!(
            detect_image_type(&decode_base64(PNG_1X1).unwrap()),
            Some("image/png")
        );
        assert_eq!(
            detect_image_type(&decode_base64(JPEG_MIN).unwrap()),
            Some("image/jpeg")
        );
        assert_eq!(
            detect_image_type(&decode_base64(GIF_MIN).unwrap()),
            Some("image/gif")
        );
        assert_eq!(
            detect_image_type(&decode_base64(WEBP_MIN).unwrap()),
            Some("image/webp")
        );
        assert_eq!(detect_image_type(b"plain text"), None);
    }

    #[test]
    fn vision_preflight_uses_model_catalog() {
        assert!(!model_supports_images("deepseek/deepseek-chat"));
        assert!(!model_supports_images("deepseek-reasoner"));
        assert!(!model_supports_images("groq/llama-3.3-70b-versatile"));
        assert!(model_supports_images("zai/glm-4.6"));
        assert!(model_supports_images("openai/gpt-4o"));
        assert!(model_supports_images("anthropic/claude-sonnet-4-5"));
        // This nested OpenRouter id is catalog data and does not rely on a
        // recognizable model-name fragment.
        assert!(model_supports_images("openrouter/acme/custom-vision"));
        // Unknown/custom models retain the permissive fallback.
        assert!(model_supports_images("my-custom-local-model"));
    }

    // ── PNG metadata stripping tests ────────────────────────────────────

    /// Build a PNG with an ancillary tEXt chunk containing AIGC metadata,
    /// then verify it is stripped while the image remains valid.
    fn make_png_with_text_chunk(text: &[u8]) -> Vec<u8> {
        // Minimal valid IHDR (1x1, 8-bit RGB).
        let ihdr_data: [u8; 13] = [
            0, 0, 0, 1, // width = 1
            0, 0, 0, 1, // height = 1
            8, // bit depth
            2, // color type = RGB
            0, // compression = deflate
            0, // filter = adaptive
            0, // interlace = none
        ];
        let ihdr = make_chunk(b"IHDR", &ihdr_data);

        // tEXt chunk: keyword\0text
        let mut text_data = b"Description".to_vec();
        text_data.push(0);
        text_data.extend_from_slice(text);
        let text_chunk = make_chunk(b"tEXt", &text_data);

        // Minimal IDAT: deflate-compressed 1x1 RGB (all zero).
        // Raw: filter byte 0 + 3 bytes RGB = [0, 0, 0, 0]
        let raw = [0, 0, 0, 0];
        let compressed = deflate_minimal(&raw);
        let idat = make_chunk(b"IDAT", &compressed);

        let iend = make_chunk(b"IEND", &[]);

        let mut png = Vec::new();
        png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        png.extend_from_slice(&ihdr);
        png.extend_from_slice(&text_chunk);
        png.extend_from_slice(&idat);
        png.extend_from_slice(&iend);
        png
    }

    #[test]
    fn strip_png_metadata_removes_text_chunk() {
        let png = make_png_with_text_chunk(b"Software\x00TestSuite");
        let original_len = png.len();
        let cleaned = strip_png_metadata(&png).expect("strip should succeed");

        // The cleaned PNG should be smaller (tEXt chunk removed).
        assert!(cleaned.len() < original_len, "cleaned should be shorter");

        // IHDR, IDAT, IEND should still be present.
        assert!(
            cleaned.windows(4).any(|w| w == b"IHDR"),
            "IHDR must be present"
        );
        assert!(
            cleaned.windows(4).any(|w| w == b"IDAT"),
            "IDAT must be present"
        );
        assert!(
            cleaned.windows(4).any(|w| w == b"IEND"),
            "IEND must be present"
        );

        // tEXt should be gone.
        assert!(
            !cleaned.windows(4).any(|w| w == b"tEXt"),
            "tEXt must be stripped"
        );
    }

    #[test]
    fn strip_png_metadata_preserves_idat_count() {
        let png = make_png_with_text_chunk(b"AIGC");
        let original_idats = count_idat_chunks(&png);
        let cleaned = strip_png_metadata(&png).unwrap();
        let cleaned_idats = count_idat_chunks(&cleaned);
        assert_eq!(original_idats, cleaned_idats, "IDAT count must not change");
    }

    #[test]
    fn strip_png_metadata_keeps_image_valid() {
        let png = make_png_with_text_chunk(b"AIGC\x00metadata");
        let cleaned = strip_png_metadata(&png).unwrap();
        assert_eq!(detect_image_type(&cleaned), Some("image/png"));
        // The cleaned PNG should still be decodable (valid signature + chunks).
        let encoded = encode_base64(&cleaned);
        let out = validate_agent_images(vec![img("image/png", &encoded)])
            .expect("cleaned PNG should pass validation");
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn strip_png_metadata_passes_through_clean_png() {
        // A PNG with no ancillary chunks should pass through unchanged.
        let png_no_text = {
            let mut p = Vec::new();
            p.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
            // IHDR + IDAT + IEND (no ancillary chunks)
            let ihdr_data: [u8; 13] = [0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0];
            p.extend_from_slice(&make_chunk(b"IHDR", &ihdr_data));
            let raw = [0, 0, 0, 0];
            p.extend_from_slice(&make_chunk(b"IDAT", &deflate_minimal(&raw)));
            p.extend_from_slice(&make_chunk(b"IEND", &[]));
            p
        };
        let cleaned = strip_png_metadata(&png_no_text).unwrap();
        assert_eq!(cleaned, png_no_text, "clean PNG should be unchanged");
    }

    #[test]
    fn strip_png_metadata_returns_none_for_invalid() {
        assert!(strip_png_metadata(&[0; 5]).is_none());
        assert!(strip_png_metadata(b"NOTAPNG!!").is_none());
    }

    #[test]
    fn validate_strips_aigc_metadata_before_provider() {
        // Build a PNG with "AIGC" tEXt metadata and verify the output
        // base64 differs from input (metadata was stripped).
        let aigc_png = make_png_with_text_chunk(b"AIGC\x00generated");
        let original_b64 = encode_base64(&aigc_png);
        let out = validate_agent_images(vec![img("image/png", &original_b64)])
            .expect("should pass validation");
        assert_eq!(out.len(), 1);
        match &out[0] {
            crate::session::Part::Image { data, .. } => {
                assert_ne!(data, &original_b64, "base64 should differ after stripping");
                // The output should decode to a valid PNG without tEXt.
                let decoded = decode_base64(data).unwrap();
                assert!(
                    !decoded.windows(4).any(|w| w == b"tEXt"),
                    "output PNG must not contain tEXt chunks"
                );
            }
            other => panic!("expected image part, got {other:?}"),
        }
    }

    #[test]
    fn encode_base64_roundtrip() {
        let input = b"Hello, PNG metadata stripping!";
        let encoded = encode_base64(input);
        let decoded = decode_base64(&encoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn crc32_known_value() {
        // CRC-32 of empty input is 0x00000000.
        assert_eq!(crc32(b""), 0x00000000);
        // CRC-32 of "123456789" (standard test vector) = 0xCBF43926.
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }
}
