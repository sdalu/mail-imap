//! Minimal MIME message parser.
//!
//! Only what is needed to enumerate the parts of a message and extract one
//! part's bytes: header parsing (content-type, content-disposition,
//! content-transfer-encoding), multipart/* boundary splitting, and CTE
//! decoding (base64, quoted-printable, binary).

use anyhow::{bail, Context, Result};

/// One MIME part (a leaf or a multipart container).
#[derive(Debug, Clone)]
pub struct Part {
    /// Main content type, e.g. `application/pdf` (lower-cased).
    pub content_type: String,
    /// File name from `Content-Disposition: filename` or the `name=`
    /// parameter of `Content-Type`, if present.
    pub filename: Option<String>,
    /// `Content-Transfer-Encoding`, lower-cased (defaults to `7bit`).
    pub encoding: String,
    /// The part body, still in its transfer-encoded form.
    pub body: Vec<u8>,
    /// Sub-parts, non-empty only for `multipart/*` parts.
    pub children: Vec<Part>,
}

impl Part {
    /// All leaf parts in document order.
    pub fn leaves(&self) -> Vec<&Part> {
        let mut out = Vec::new();
        Self::collect_leaves(self, &mut out);
        out
    }

    fn collect_leaves<'a>(part: &'a Part, out: &mut Vec<&'a Part>) {
        if part.children.is_empty() {
            out.push(part);
        } else {
            for child in &part.children {
                Self::collect_leaves(child, out);
            }
        }
    }

    /// The part body decoded from its transfer encoding.
    pub fn decoded(&self) -> Result<Vec<u8>> {
        decode_body(&self.encoding, &self.body)
    }
}

/// Parse a full MIME message (headers + body).
pub fn parse_message(bytes: &[u8]) -> Result<Part> {
    let (raw_headers, body) = split_headers_and_body(bytes);
    let headers = parse_headers(raw_headers);
    let content_type = header_value(&headers, "content-type")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "text/plain".to_string());
    let (main, params) = parse_content_type(&content_type);
    let encoding = header_value(&headers, "content-transfer-encoding")
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "7bit".to_string());
    let filename = disposition_filename(&headers)
        .or_else(|| param_value(&params, "name").map(str::to_string));

    let mut part = Part {
        content_type: main,
        filename,
        encoding,
        body: body.to_vec(),
        children: Vec::new(),
    };
    if part.content_type.starts_with("multipart/") {
        let boundary = param_value(&params, "boundary")
            .context("multipart message without a boundary parameter")?;
        let segments = split_boundary(body, boundary)
            .with_context(|| format!("boundary '{}' not found in message body", boundary))?;
        part.children = segments
            .into_iter()
            .map(|seg| parse_message(&seg))
            .collect::<Result<Vec<_>>>()?;
    }
    Ok(part)
}

/// Split raw message bytes into (headers, body). Tolerates CRLF or LF.
fn split_headers_and_body(bytes: &[u8]) -> (&[u8], &[u8]) {
    if let Some(pos) = find_subsequence(bytes, b"\r\n\r\n") {
        return (&bytes[..pos], &bytes[pos + 4..]);
    }
    if let Some(pos) = find_subsequence(bytes, b"\n\n") {
        return (&bytes[..pos], &bytes[pos + 2..]);
    }
    (bytes, b"")
}

/// Folded header lines unfolded into `Vec<(name, value)>` pairs
/// (name lower-cased, first occurrence wins per lookup).
fn parse_headers(raw: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(raw);
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            // Continuation of the previous header.
            if let Some((_, value)) = out.last_mut() {
                value.push(' ');
                value.push_str(line.trim_start());
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            out.push((
                name.trim().to_ascii_lowercase(),
                value.trim().to_string(),
            ));
        }
    }
    out
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

/// `application/pdf; name="x.pdf"` -> ("application/pdf", {name: "x.pdf"})
fn parse_content_type(value: &str) -> (String, Vec<(String, String)>) {
    let value = value.trim();
    let main = value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let params = if let Some((_, rest)) = value.split_once(';') {
        parse_params(rest)
    } else {
        Vec::new()
    };
    (main, params)
}

/// Split `; key=value` pairs, honoring double-quoted values.
fn parse_params(input: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in input.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ';' if !in_quotes => {
                push_param(&mut out, &current);
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    push_param(&mut out, &current);
    out
}

fn push_param(out: &mut Vec<(String, String)>, pair: &str) {
    let pair = pair.trim();
    if pair.is_empty() {
        return;
    }
    match pair.split_once('=') {
        Some((key, value)) => {
            out.push((
                key.trim().to_ascii_lowercase(),
                value.trim().trim_matches('"').to_string(),
            ));
        }
        None => out.push((pair.to_ascii_lowercase(), String::new())),
    }
}

fn param_value<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// File name from `Content-Disposition: attachment; filename="x"`.
fn disposition_filename(headers: &[(String, String)]) -> Option<String> {
    let value = header_value(headers, "content-disposition")?;
    let params = parse_params(value);
    param_value(&params, "filename").map(str::to_string)
}

/// Split a multipart body into the byte slices of each sub-part.
fn split_boundary(body: &[u8], boundary: &str) -> Result<Vec<Vec<u8>>> {
    let delim = format!("--{}", boundary);
    // Offsets of every line that starts with the boundary delimiter
    // (including the terminating `--boundary--` line, if present).
    let mut starts: Vec<usize> = Vec::new();
    let mut pos = 0usize;
    while pos < body.len() {
        let line_end = find_subsequence(&body[pos..], b"\n")
            .map(|i| pos + i + 1)
            .unwrap_or(body.len());
        let line = &body[pos..line_end];
        let trimmed = line
            .strip_suffix(b"\n")
            .and_then(|l| l.strip_suffix(b"\r"))
            .unwrap_or(line);
        if trimmed.starts_with(delim.as_bytes()) {
            starts.push(pos);
        }
        pos = line_end;
    }
    if starts.is_empty() {
        bail!("no boundary line found");
    }
    let mut segments = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        let line_end = find_subsequence(&body[start..], b"\n")
            .map(|o| start + o + 1)
            .unwrap_or(body.len());
        let next_start = starts.get(i + 1).copied().unwrap_or(body.len());
        let mut seg = body[line_end..next_start.min(body.len())].to_vec();
        // The line break before the next delimiter belongs to the delimiter.
        if seg.ends_with(b"\r\n") {
            seg.truncate(seg.len() - 2);
        } else if seg.ends_with(b"\n") {
            seg.truncate(seg.len() - 1);
        }
        segments.push(seg);
    }
    // After the terminating delimiter (`--boundary--`) only an epilogue
    // follows; drop that last segment. If the last delimiter was not
    // terminating, the last segment is a real part and is kept.
    let last_start = starts.last().copied().unwrap_or(0);
    let last_line_end = find_subsequence(&body[last_start..], b"\n")
        .map(|o| last_start + o)
        .unwrap_or(body.len());
    let mut last_line = body[last_start..last_line_end].to_vec();
    if last_line.last() == Some(&b'\n') {
        last_line.pop();
    }
    if last_line.last() == Some(&b'\r') {
        last_line.pop();
    }
    if last_line == format!("{}--", delim).into_bytes() {
        segments.pop();
    }
    Ok(segments)
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// Decode a part body from its content transfer encoding.
pub fn decode_body(encoding: &str, body: &[u8]) -> Result<Vec<u8>> {
    match encoding {
        "base64" => {
            let compact: String = body
                .iter()
                .filter(|b| !is_ascii_ws(b))
                .map(|b| *b as char)
                .collect();
            let pad = (4 - (compact.len() % 4)) % 4;
            base64::decode(format!("{}{}", compact, "=".repeat(pad)))
                .context("base64 part is not valid base64")
        }
        "quoted-printable" => decode_quoted_printable(body),
        _ => Ok(body.to_vec()), // 7bit, 8bit, binary, anything unknown
    }
}

fn is_ascii_ws(b: &u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple_message(content_type: &str, body: &str) -> Vec<u8> {
        format!(
            "From: a@example.com\r\nContent-Type: {}\r\n\r\n{}",
            content_type, body
        )
        .into_bytes()
    }

    #[test]
    fn plain_message_is_one_part() {
        let msg = simple_message("text/plain", "hello world");
        let root = parse_message(&msg).unwrap();
        assert_eq!(root.content_type, "text/plain");
        let leaves = root.leaves();
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].decoded().unwrap(), b"hello world");
    }

    #[test]
    fn multipart_two_parts() {
        let msg = format!(
            "Content-Type: multipart/mixed; boundary=BOUND\r\n\r\n\
             --BOUND\r\n\
             Content-Type: text/plain\r\n\r\nbody text\r\n\
             --BOUND\r\n\
             Content-Type: application/pdf; name=doc.pdf\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nSGVsbG8=\r\n\
             --BOUND--\r\n"
         )
         .into_bytes();
        let root = parse_message(&msg).unwrap();
        let leaves = root.leaves();
        assert_eq!(leaves.len(), 2);
        assert_eq!(leaves[0].content_type, "text/plain");
        assert_eq!(leaves[0].decoded().unwrap(), b"body text");
        assert_eq!(leaves[1].content_type, "application/pdf");
        assert_eq!(leaves[1].filename.as_deref(), Some("doc.pdf"));
        // "SGVsbG8=" is base64 for "Hello"
        assert_eq!(leaves[1].decoded().unwrap(), b"Hello");
    }

    #[test]
    fn nested_multipart_numbers_all_leaves_in_order() {
        let msg = format!(
            "Content-Type: multipart/alternative; boundary=A\r\n\r\n\
             --A\r\n\
             Content-Type: text/plain\r\n\r\nplain\r\n\
             --A\r\n\
             Content-Type: multipart/mixed; boundary=B\r\n\r\n\
             --B\r\n\
             Content-Type: text/html\r\n\r\n<html/>\r\n\
             --B\r\n\
             Content-Type: application/zip\r\n\
             Content-Disposition: attachment; filename=stuff.zip\r\n\r\nzipped\r\n\
             --B--\r\n\
             --A--\r\n"
        )
        .into_bytes();
        let root = parse_message(&msg).unwrap();
        let leaves = root.leaves();
        assert_eq!(
            leaves
                .iter()
                .map(|p| p.content_type.as_str())
                .collect::<Vec<_>>(),
            vec!["text/plain", "text/html", "application/zip"]
        );
        assert_eq!(leaves[2].filename.as_deref(), Some("stuff.zip"));
    }

    #[test]
    fn quoted_printable_decoding() {
        assert_eq!(
            decode_body("quoted-printable", b"caf=E9\nline2\n").unwrap(),
            b"caf\xE9\nline2\n"
        );
        // soft line break
        assert_eq!(
            decode_body("quoted-printable", b"foo=\r\nbar").unwrap(),
            b"foobar"
        );
    }

    #[test]
    fn base64_decoding_ignores_line_breaks() {
        assert_eq!(
            decode_body("base64", b"SGVsbG8=\n\r\n").unwrap(),
            b"Hello"
        );
    }

    #[test]
    fn binary_passthrough() {
        let data: Vec<u8> = vec![0, 1, 2, 255, 10];
        assert_eq!(decode_body("8bit", &data).unwrap(), data);
    }

    #[test]
    fn missing_boundary_fails() {
        let msg = b"Content-Type: multipart/mixed; boundary=X\r\n\r\nno parts here";
        assert!(parse_message(msg).is_err());
    }
}

fn decode_quoted_printable(body: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0usize;
    while i < body.len() {
        match body[i] {
            b'=' if i + 1 < body.len() && (body[i + 1] == b'\n' || body[i + 1] == b'\r') => {
                // Soft line break: drop the "=<newline>" (and a CRLF pair).
                i += if body[i + 1] == b'\r' && i + 2 < body.len() && body[i + 2] == b'\n' {
                    3
                } else {
                    2
                };
            }
            b'=' if i + 2 < body.len() => {
                let hi = (body[i + 1] as char).to_digit(16);
                let lo = (body[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(body[i]);
                i += 1;
            }
            b'=' if i + 1 == body.len() => {
                // Dangling '=' at the very end: nothing to decode.
                out.push(body[i]);
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Ok(out)
}
