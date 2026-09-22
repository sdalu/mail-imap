//! Minimal MIME message parser, and the one writer this tree has.
//!
//! The parser is what is needed to enumerate the parts of a message and
//! extract one part's bytes: header parsing (content-type,
//! content-disposition, content-transfer-encoding), multipart/* boundary
//! splitting, and CTE decoding (base64, quoted-printable, binary).
//!
//! The writer is [`strip_parts`]: it rebuilds a message with named parts
//! replaced by stubs, for `part strip` (TODO.md section 6). It does not
//! add parts, promote a single-part message to multipart, or talk to a
//! server -- bytes in, bytes out.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, FixedOffset};
use sha2::{Digest, Sha256};

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

/// How deep a multipart tree may nest before the parser gives up.
///
/// `parse_message` recurses once per level, so an unbounded tree is a
/// stack overflow rather than an error: a ~600 KB crafted message was
/// enough to take the process down with SIGSEGV, and anyone who can
/// send the user mail can supply one. Real messages nest two or three
/// deep; 64 is far past anything a mail client produces and far short
/// of the stack.
const MAX_DEPTH: usize = 64;

/// Parse a full MIME message (headers + body).
pub fn parse_message(bytes: &[u8]) -> Result<Part> {
    parse_at_depth(bytes, 0)
}

fn parse_at_depth(bytes: &[u8], depth: usize) -> Result<Part> {
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
        if depth >= MAX_DEPTH {
            bail!(
                "multipart nesting deeper than {} levels: refusing to descend further \
                 (a message this deep is crafted, not written)",
                MAX_DEPTH
            );
        }
        let boundary = param_value(&params, "boundary")
            .context("multipart message without a boundary parameter")?;
        let segments = split_boundary(body, boundary)
            .with_context(|| format!("boundary '{}' not found in message body", boundary))?;
        part.children = segments
            .into_iter()
            .map(|seg| parse_at_depth(&seg, depth + 1))
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

/// What a delimiter line is, when a line is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delim {
    /// `--boundary`: the part after it belongs to this multipart.
    Part,
    /// `--boundary--`: the multipart ends here and an epilogue may follow.
    Close,
}

/// Is `line` a delimiter for `delim` (already `--boundary`), and which?
///
/// RFC 2046 §5.1.1 matches a whole delimiter LINE: exactly `--boundary`,
/// optionally followed by transport-padding (whitespace), with the
/// closing form appending `--` first. Two things follow, and the old
/// prefix-match got both wrong:
///
/// * `--B1` is NOT a delimiter of boundary `B`, so a message whose
///   inner boundary extends the outer one -- `----=_Part_2` enclosing
///   `----=_Part_21`, exactly what sequentially-numbered generators
///   emit -- must not be shredded by the outer split.
/// * `--B-- ` IS the close-delimiter, padding and all, so the epilogue
///   after it must not become a phantom empty part (which, nested,
///   shifts the number of every later part and makes `part save N`
///   write the wrong bytes).
fn delimiter_kind(line: &[u8], delim: &[u8]) -> Option<Delim> {
    let rest = line.strip_prefix(delim)?;
    let (kind, rest) = match rest.strip_prefix(b"--") {
        Some(rest) => (Delim::Close, rest),
        None => (Delim::Part, rest),
    };
    rest.iter()
        .all(|b| *b == b' ' || *b == b'\t')
        .then_some(kind)
}

/// Split a multipart body into the byte slices of each sub-part.
fn split_boundary(body: &[u8], boundary: &str) -> Result<Vec<Vec<u8>>> {
    let delim = format!("--{}", boundary);
    let marks = boundary_marks(body, delim.as_bytes())?;
    let mut segments = Vec::new();
    for (i, &(_, after_line, kind)) in marks.iter().enumerate() {
        if kind == Delim::Close {
            break;
        }
        let end = marks.get(i + 1).map(|(start, _, _)| *start).unwrap_or(body.len());
        let mut seg = body[after_line..end].to_vec();
        // The line break before the next delimiter belongs to the delimiter.
        if seg.ends_with(b"\r\n") {
            seg.truncate(seg.len() - 2);
        } else if seg.ends_with(b"\n") {
            seg.truncate(seg.len() - 1);
        }
        segments.push(seg);
    }
    Ok(segments)
}

/// Every delimiter line for `delim` (already `--boundary`) in `body`:
/// where it starts, where the next line starts, and whether it closes
/// the multipart. [`split_boundary`] (the parser) uses this to find the
/// segments between delimiters; the writer (`rewrite_part`, below)
/// shares it so both walk the same lines the same way.
fn boundary_marks(body: &[u8], delim: &[u8]) -> Result<Vec<(usize, usize, Delim)>> {
    let mut marks: Vec<(usize, usize, Delim)> = Vec::new();
    let mut pos = 0usize;
    while pos < body.len() {
        let line_end = find_subsequence(&body[pos..], b"\n")
            .map(|i| pos + i + 1)
            .unwrap_or(body.len());
        let line = &body[pos..line_end];
        // Strip the line ending, LF or CRLF. (`and_then` here would
        // hand back the un-stripped line for an LF-only message, which
        // only went unnoticed while the match was a prefix test.)
        let trimmed = line
            .strip_suffix(b"\n")
            .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
            .unwrap_or(line);
        if let Some(kind) = delimiter_kind(trimmed, delim) {
            marks.push((pos, line_end, kind));
            if kind == Delim::Close {
                // Nothing after the close-delimiter is a part.
                break;
            }
        }
        pos = line_end;
    }
    if marks.is_empty() {
        bail!("no boundary line found");
    }
    Ok(marks)
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

// ---------------------------------------------------------------------
// Writer: `part strip`'s content half (TODO.md section 6).
// ---------------------------------------------------------------------

/// A record of one part [`strip_parts`] removed: its number in document
/// order (the same numbering `Part::leaves()` and `part list` use), what
/// it was, and what a copy kept elsewhere can be checked against.
///
/// The digest is of the *decoded* bytes -- what `part save` would have
/// written -- not the transfer-encoded form that sat in the message:
/// that is the form a saved file is actually in, and it is stable where
/// the encoded form is not (a gateway may re-wrap base64 at a different
/// line length without changing the attachment at all).
#[derive(Debug, Clone, serde::Serialize)]
pub struct StrippedPart {
    pub part: u32,
    pub content_type: String,
    pub filename: Option<String>,
    /// Decoded size in bytes.
    pub size: u64,
    /// Lowercase hex SHA-256 of the decoded bytes.
    pub sha256: String,
}

/// Rebuild `bytes` with the leaf parts named in `numbers` (1-based,
/// `Part::leaves()` order -- the same numbering `part list` prints)
/// replaced by a `text/plain` stub, and record what each one was.
///
/// A stripped part is *replaced*, not deleted: removing it outright
/// would renumber every part after it, and a `part list` taken before
/// the strip has to still describe the message afterwards. Every other
/// part keeps its bytes and its own headers exactly, and the message's
/// own headers are preserved in order, with one `X-Mail-Imap-Stripped`
/// header appended per part stripped (folded per RFC 5322 rather than
/// emitted as one line of unbounded length).
///
/// `stripped_at` is recorded, as an RFC 2822 date, both in the stub
/// bodies and in the `X-Mail-Imap-Stripped` headers.
///
/// Refuses, rather than guessing, on: an empty `numbers`, a part number
/// that does not name a leaf of this message, or a message whose MIME
/// cannot be parsed.
///
/// Removing a part never changes a message's top-level structure, so
/// this never has to promote an already-single-part message to
/// `multipart/mixed`: stripping the sole part of one replaces its
/// content and `Content-Type` directly, keeping every other header of
/// the message.
pub fn strip_parts(
    bytes: &[u8],
    numbers: &[u32],
    stripped_at: DateTime<FixedOffset>,
) -> Result<(Vec<u8>, Vec<StrippedPart>)> {
    if numbers.is_empty() {
        bail!("no parts given to strip");
    }
    let root = parse_message(bytes).context("parsing message to strip parts from it")?;
    let leaves = root.leaves();
    let leaf_count = leaves.len();

    let mut wanted: Vec<u32> = numbers.to_vec();
    wanted.sort_unstable();
    wanted.dedup();
    for &n in &wanted {
        if n == 0 || n as usize > leaf_count {
            bail!(
                "part {} does not exist (message has {} leaf part{})",
                n,
                leaf_count,
                if leaf_count == 1 { "" } else { "s" }
            );
        }
    }

    // One record per part to strip, computed once from the already-
    // parsed tree so the raw rewrite below never has to re-derive a
    // content type or re-decode a body: there is one place this
    // information comes from, not two that could drift apart.
    let mut records = Vec::with_capacity(wanted.len());
    for &n in &wanted {
        let p = leaves[(n - 1) as usize];
        let decoded = p
            .decoded()
            .with_context(|| format!("decoding part {} to strip it", n))?;
        records.push(StrippedPart {
            part: n,
            content_type: p.content_type.clone(),
            filename: p.filename.clone(),
            size: decoded.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&decoded)),
        });
    }

    let mut leaf = 0u32;
    let rebuilt = rewrite_part(bytes, 0, &mut leaf, &wanted, &records, stripped_at)?;
    let extra = build_stripped_headers(&records, stripped_at);
    let final_bytes = splice_message_headers(&rebuilt, &extra);
    Ok((final_bytes, records))
}

/// Depth-first rewrite of one part's raw bytes -- header block through
/// body, exactly what a nested `parse_at_depth` call would receive --
/// replacing any leaf numbered in `wanted` with its stub.
///
/// Everything not being replaced is copied byte for byte: an unchanged
/// leaf returns its own input unchanged, and a multipart container
/// passes its own header block and header/body separator through
/// untouched, rebuilding only its body from the same preamble,
/// delimiter lines and epilogue bytes it was given -- so a subtree with
/// nothing to strip in it reconstructs to the exact bytes it started
/// from.
fn rewrite_part(
    raw: &[u8],
    depth: usize,
    leaf: &mut u32,
    wanted: &[u32],
    records: &[StrippedPart],
    stripped_at: DateTime<FixedOffset>,
) -> Result<Vec<u8>> {
    let (raw_headers, body) = split_headers_and_body(raw);
    let sep_len = raw.len() - raw_headers.len() - body.len();
    let sep = &raw[raw_headers.len()..raw_headers.len() + sep_len];
    let headers = parse_headers(raw_headers);
    let content_type = header_value(&headers, "content-type")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "text/plain".to_string());
    let (main, params) = parse_content_type(&content_type);

    if main.starts_with("multipart/") {
        if depth >= MAX_DEPTH {
            bail!(
                "multipart nesting deeper than {} levels: refusing to descend further \
                 (a message this deep is crafted, not written)",
                MAX_DEPTH
            );
        }
        let boundary = param_value(&params, "boundary")
            .context("multipart message without a boundary parameter")?;
        let delim = format!("--{}", boundary);
        let marks = boundary_marks(body, delim.as_bytes())
            .with_context(|| format!("boundary '{}' not found in message body", boundary))?;

        let mut new_body = Vec::with_capacity(body.len());
        new_body.extend_from_slice(&body[..marks[0].0]); // preamble
        for (i, &(start, after_line, kind)) in marks.iter().enumerate() {
            new_body.extend_from_slice(&body[start..after_line]); // delimiter line
            if kind == Delim::Close {
                new_body.extend_from_slice(&body[after_line..]); // epilogue
                break;
            }
            let seg_end = marks.get(i + 1).map(|&(s, _, _)| s).unwrap_or(body.len());
            let seg = &body[after_line..seg_end];
            // The line break right before the next delimiter belongs to
            // the delimiter, not to this part's content (same rule
            // `split_boundary` applies) -- carry it separately so it
            // lands after the rewritten part exactly as it was.
            let trim = if seg.ends_with(b"\r\n") {
                2
            } else if seg.ends_with(b"\n") {
                1
            } else {
                0
            };
            let (trimmed, trailer) = seg.split_at(seg.len() - trim);
            let child = rewrite_part(trimmed, depth + 1, leaf, wanted, records, stripped_at)?;
            new_body.extend_from_slice(&child);
            new_body.extend_from_slice(trailer);
        }

        let mut out = Vec::with_capacity(raw_headers.len() + sep.len() + new_body.len());
        out.extend_from_slice(raw_headers);
        out.extend_from_slice(sep);
        out.extend_from_slice(&new_body);
        Ok(out)
    } else {
        *leaf += 1;
        let n = *leaf;
        if !wanted.contains(&n) {
            return Ok(raw.to_vec());
        }
        let record = records
            .iter()
            .find(|r| r.part == n)
            .expect("every wanted leaf has a record computed by strip_parts");
        Ok(build_stub_part(raw_headers, record, stripped_at))
    }
}

/// The replacement for one stripped leaf: its own headers with
/// `Content-Type`, `Content-Transfer-Encoding` and `Content-Disposition`
/// dropped and a fresh `Content-Type: text/plain` put in their place,
/// then a body naming what was removed. Every other header of the part
/// -- on an ordinary leaf there normally are none, but a single-part
/// message being stripped keeps its own `From`/`Subject`/`Date`/etc.
/// this way -- passes through unchanged.
fn build_stub_part(raw_headers: &[u8], record: &StrippedPart, stripped_at: DateTime<FixedOffset>) -> Vec<u8> {
    let mut headers = drop_headers(
        raw_headers,
        &["content-type", "content-transfer-encoding", "content-disposition"],
    );
    if !headers.is_empty() && !ends_with_lf(&headers) {
        headers.extend_from_slice(b"\r\n");
    }
    headers.extend_from_slice(b"Content-Type: text/plain; charset=utf-8\r\n");
    headers.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n");

    let mut out = headers;
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(stub_body_text(record, stripped_at).as_bytes());
    out
}

/// The stub body's text: what was there, its size, its digest, and
/// when it was removed -- so a reader who goes looking for the
/// attachment finds out what happened to it, and the digest makes the
/// removal checkable against a copy kept elsewhere.
fn stub_body_text(record: &StrippedPart, stripped_at: DateTime<FixedOffset>) -> String {
    format!(
        "This attachment was removed by `part strip` and is no longer part\r\n\
         of this message.\r\n\
         \r\n\
         Part:      {}\r\n\
         Filename:  {}\r\n\
         Type:      {}\r\n\
         Size:      {} bytes\r\n\
         SHA-256:   {}\r\n\
         Date:      {}\r\n",
        record.part,
        record.filename.as_deref().unwrap_or("(none)"),
        record.content_type,
        record.size,
        record.sha256,
        stripped_at.to_rfc2822(),
    )
}

/// One `X-Mail-Imap-Stripped` header per record, each folded to stay
/// under a bounded line length (TODO.md section 6's example shape:
/// `part=`, `type=`, `filename=`, `size=`, `sha256=`, `date=`).
fn build_stripped_headers(records: &[StrippedPart], stripped_at: DateTime<FixedOffset>) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        let fields = vec![
            format!("part={}", record.part),
            format!("type={}", quote_value(&record.content_type)),
            format!(
                "filename={}",
                quote_value(record.filename.as_deref().unwrap_or(""))
            ),
            format!("size={}", record.size),
            format!("sha256={}", record.sha256),
            format!("date={}", quote_value(&stripped_at.to_rfc2822())),
        ];
        out.extend_from_slice(fold_header("X-Mail-Imap-Stripped", &fields).as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// RFC 5322 folds a header line rather than letting it grow without
/// bound: any line beginning with whitespace continues the header
/// before it, so folding is just choosing where to break.
fn fold_header(name: &str, fields: &[String]) -> String {
    const FOLD_WIDTH: usize = 78;
    let mut lines: Vec<String> = Vec::new();
    let mut current = format!("{}:", name);
    for (i, field) in fields.iter().enumerate() {
        let token = if i + 1 < fields.len() {
            format!("{};", field)
        } else {
            field.clone()
        };
        if current.ends_with(':') || current.len() + 1 + token.len() <= FOLD_WIDTH {
            current.push(' ');
            current.push_str(&token);
        } else {
            lines.push(current);
            current = format!("    {}", token);
        }
    }
    lines.push(current);
    lines.join("\r\n")
}

/// A MIME quoted-string: backslash and double-quote escaped, per
/// RFC 2045. Nothing here needs to escape a raw CR or LF -- the header
/// parser above already collapses one to a space while folding, so a
/// value taken from a parsed header can never carry one.
fn quote_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// `raw_headers` with every header whose name (case-insensitively)
/// matches one of `names` removed. Everything else -- order, folding,
/// exact bytes -- is left alone.
fn drop_headers(raw: &[u8], names: &[&str]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    for line in header_lines(raw) {
        let name_end = line.iter().position(|&b| b == b':').unwrap_or(line.len());
        let name = String::from_utf8_lossy(&line[..name_end]).trim().to_ascii_lowercase();
        if !names.contains(&name.as_str()) {
            out.extend_from_slice(line);
        }
    }
    out
}

/// The logical header lines of a raw header block -- a starting line
/// plus any folded continuation lines -- each returned with its
/// original bytes, line endings included, untouched.
fn header_lines(raw: &[u8]) -> Vec<&[u8]> {
    let mut out: Vec<&[u8]> = Vec::new();
    let mut pos = 0usize;
    while pos < raw.len() {
        let mut end = next_line_end(raw, pos);
        while end < raw.len() && (raw[end] == b' ' || raw[end] == b'\t') {
            end = next_line_end(raw, end);
        }
        out.push(&raw[pos..end]);
        pos = end;
    }
    out
}

fn next_line_end(raw: &[u8], pos: usize) -> usize {
    find_subsequence(&raw[pos..], b"\n")
        .map(|i| pos + i + 1)
        .unwrap_or(raw.len())
}

fn ends_with_lf(buf: &[u8]) -> bool {
    buf.last() == Some(&b'\n')
}

/// Insert `extra` (already `\r\n`-terminated header lines) into `msg`'s
/// own header block, just before the blank line that starts the body.
/// The existing headers, their order, the header/body separator style,
/// and the body all pass through untouched.
fn splice_message_headers(msg: &[u8], extra: &[u8]) -> Vec<u8> {
    let (headers, body) = split_headers_and_body(msg);
    let sep_len = msg.len() - headers.len() - body.len();
    let sep = &msg[headers.len()..headers.len() + sep_len];
    let mut out = Vec::with_capacity(msg.len() + extra.len());
    out.extend_from_slice(headers);
    if !headers.is_empty() && !ends_with_lf(headers) {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(extra);
    out.extend_from_slice(sep);
    out.extend_from_slice(body);
    out
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
        let msg = "Content-Type: multipart/mixed; boundary=BOUND\r\n\r\n\
             --BOUND\r\n\
             Content-Type: text/plain\r\n\r\nbody text\r\n\
             --BOUND\r\n\
             Content-Type: application/pdf; name=doc.pdf\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nSGVsbG8=\r\n\
             --BOUND--\r\n"
         .to_string()
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
        let msg = "Content-Type: multipart/alternative; boundary=A\r\n\r\n\
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
        .to_string()
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
    fn an_inner_boundary_may_extend_the_outer_one() {
        // RFC 2046 matches a whole delimiter line, so "--X1" is not a
        // delimiter of boundary "X". Sequentially-numbered generators
        // really do emit ----=_Part_2 enclosing ----=_Part_21.
        let msg = "Content-Type: multipart/mixed; boundary=X\r\n\r\n\
             --X\r\n\
             Content-Type: multipart/alternative; boundary=X1\r\n\r\n\
             --X1\r\n\
             Content-Type: text/plain\r\n\r\nplain\r\n\
             --X1\r\n\
             Content-Type: text/html\r\n\r\n<p>html</p>\r\n\
             --X1--\r\n\
             --X\r\n\
             Content-Type: application/pdf\r\n\
             Content-Disposition: attachment; filename=\"real.pdf\"\r\n\r\nPDFBYTES\r\n\
             --X--\r\n"
            .to_string()
            .into_bytes();
        let root = parse_message(&msg).expect("a legal message must parse");
        let leaves = root.leaves();
        assert_eq!(
            leaves.iter().map(|p| p.content_type.as_str()).collect::<Vec<_>>(),
            vec!["text/plain", "text/html", "application/pdf"]
        );
        assert_eq!(leaves[2].filename.as_deref(), Some("real.pdf"));
    }

    #[test]
    fn transport_padding_after_the_close_delimiter_invents_no_part() {
        // RFC 2046 allows whitespace after the close-delimiter. Taking
        // the padded line for a part delimiter left the epilogue as a
        // phantom empty part -- and, nested, shifted every later part
        // number so `part save N` wrote the wrong bytes.
        let padded = "Content-Type: multipart/mixed; boundary=B\r\n\r\n\
             --B\r\n\
             Content-Type: text/plain\r\n\r\nhi\r\n\
             --B-- \r\n\
             epilogue text\r\n"
            .to_string()
            .into_bytes();
        assert_eq!(parse_message(&padded).unwrap().leaves().len(), 1);
        // Numbering downstream of a padded inner container is what the
        // phantom part actually broke.
        let nested = "Content-Type: multipart/mixed; boundary=OUT\r\n\r\n\
             --OUT\r\n\
             Content-Type: multipart/alternative; boundary=IN\r\n\r\n\
             --IN\r\n\
             Content-Type: text/plain\r\n\r\ntext\r\n\
             --IN--\t\r\n\
             --OUT\r\n\
             Content-Type: application/pdf\r\n\r\nPDFBYTES\r\n\
             --OUT--\r\n"
            .to_string()
            .into_bytes();
        let leaves = parse_message(&nested).unwrap();
        let leaves = leaves.leaves();
        assert_eq!(leaves.len(), 2, "no phantom part between them");
        assert_eq!(leaves[1].content_type, "application/pdf", "still part 2");
    }

    #[test]
    fn nesting_past_the_limit_is_an_error_not_a_crash() {
        // Unbounded recursion here was a SIGSEGV on a ~600 KB message.
        let mut msg = String::new();
        for i in 0..MAX_DEPTH + 5 {
            msg.push_str(&format!(
                "Content-Type: multipart/mixed; boundary=b{:05}a\r\n\r\n--b{:05}a\r\n",
                i, i
            ));
        }
        msg.push_str("Content-Type: text/plain\r\n\r\nbottom\r\n");
        for i in (0..MAX_DEPTH + 5).rev() {
            msg.push_str(&format!("--b{:05}a--\r\n", i));
        }
        let err = parse_message(msg.as_bytes()).expect_err("must refuse, not recurse");
        assert!(format!("{:#}", err).contains("nesting"), "{:#}", err);
        // ... while a tree within the limit still parses.
        let mut ok = String::new();
        for i in 0..8 {
            ok.push_str(&format!(
                "Content-Type: multipart/mixed; boundary=c{:05}a\r\n\r\n--c{:05}a\r\n",
                i, i
            ));
        }
        ok.push_str("Content-Type: text/plain\r\n\r\nbottom\r\n");
        for i in (0..8).rev() {
            ok.push_str(&format!("--c{:05}a--\r\n", i));
        }
        assert_eq!(parse_message(ok.as_bytes()).unwrap().leaves().len(), 1);
    }

    #[test]
    fn missing_boundary_fails() {
        let msg = b"Content-Type: multipart/mixed; boundary=X\r\n\r\nno parts here";
        assert!(parse_message(msg).is_err());
    }

    // -------------------------------------------------------------
    // Writer: strip_parts
    // -------------------------------------------------------------

    fn a_date() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-09-22T18:40:11+02:00").expect("fixture")
    }

    #[test]
    fn strip_replaces_the_named_part_and_round_trips() {
        let msg = "From: a@example.com\r\nSubject: hi\r\n\
             Content-Type: multipart/mixed; boundary=BOUND\r\n\r\n\
             --BOUND\r\n\
             Content-Type: text/plain\r\n\r\nbody text\r\n\
             --BOUND\r\n\
             Content-Type: application/pdf; name=doc.pdf\r\n\
             Content-Disposition: attachment; filename=doc.pdf\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nSGVsbG8=\r\n\
             --BOUND--\r\n"
            .to_string()
            .into_bytes();

        let original = parse_message(&msg).unwrap();
        let original_leaves = original.leaves();
        let survivor_body = original_leaves[0].body.clone();

        let (rebuilt, records) = strip_parts(&msg, &[2], a_date()).expect("strip must succeed");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].part, 2);
        assert_eq!(records[0].content_type, "application/pdf");
        assert_eq!(records[0].filename.as_deref(), Some("doc.pdf"));
        assert_eq!(records[0].size, 5); // "Hello", decoded from "SGVsbG8="
                                         // Known SHA-256("Hello"), from `printf 'Hello' | sha256sum`.
        assert_eq!(
            records[0].sha256,
            "185f8db32271fe25f561a6fc938b2e264306ec304eda518007d1764826381969"
        );

        let reparsed = parse_message(&rebuilt).expect("a rebuilt message must still parse");
        let leaves = reparsed.leaves();
        assert_eq!(leaves.len(), 2, "part count -- and numbering -- is unchanged");

        // Part 1 survives byte for byte, transfer encoding and all.
        assert_eq!(leaves[0].content_type, "text/plain");
        assert_eq!(leaves[0].body, survivor_body);
        assert_eq!(leaves[0].decoded().unwrap(), b"body text");

        // Part 2 is now the stub, in the same slot.
        assert_eq!(leaves[1].content_type, "text/plain");
        let stub = String::from_utf8(leaves[1].decoded().unwrap()).unwrap();
        assert!(stub.contains("doc.pdf"), "{}", stub);
        assert!(stub.contains(&records[0].sha256), "{}", stub);
        assert!(stub.contains("5 bytes"), "{}", stub);

        // The message gained exactly one X-Mail-Imap-Stripped header,
        // and its own headers are still there.
        let text = String::from_utf8_lossy(&rebuilt);
        assert_eq!(text.matches("X-Mail-Imap-Stripped:").count(), 1);
        assert!(text.contains("From: a@example.com"));
        assert!(text.contains("Subject: hi"));
        assert!(text.contains("part=2"));
        assert!(text.contains(r#"filename="doc.pdf""#));
    }

    #[test]
    fn stripping_two_parts_keeps_every_number_stable() {
        let msg = "Content-Type: multipart/mixed; boundary=B\r\n\r\n\
             --B\r\n\
             Content-Type: text/plain\r\n\r\nkeep me\r\n\
             --B\r\n\
             Content-Type: application/pdf\r\n\
             Content-Disposition: attachment; filename=a.pdf\r\n\r\nAAAA\r\n\
             --B\r\n\
             Content-Type: text/plain\r\n\r\nkeep me too\r\n\
             --B\r\n\
             Content-Type: application/zip\r\n\
             Content-Disposition: attachment; filename=b.zip\r\n\r\nZZZZ\r\n\
             --B--\r\n"
            .to_string()
            .into_bytes();

        let (rebuilt, records) = strip_parts(&msg, &[2, 4], a_date()).unwrap();
        assert_eq!(
            records.iter().map(|r| r.part).collect::<Vec<_>>(),
            vec![2, 4],
            "records come back in document order"
        );

        let leaves = parse_message(&rebuilt).unwrap();
        let leaves = leaves.leaves();
        assert_eq!(leaves.len(), 4, "no part was removed, only replaced");
        assert_eq!(leaves[0].decoded().unwrap(), b"keep me");
        assert_eq!(leaves[1].content_type, "text/plain"); // stub, was the pdf
        assert!(String::from_utf8(leaves[1].decoded().unwrap())
            .unwrap()
            .contains("a.pdf"));
        assert_eq!(leaves[2].decoded().unwrap(), b"keep me too");
        assert_eq!(leaves[3].content_type, "text/plain"); // stub, was the zip
        assert!(String::from_utf8(leaves[3].decoded().unwrap())
            .unwrap()
            .contains("b.zip"));
    }

    #[test]
    fn nested_multipart_strip_preserves_siblings_and_structure() {
        let msg = "Content-Type: multipart/alternative; boundary=A\r\n\r\n\
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
            .to_string()
            .into_bytes();

        // Strip the deeply-nested zip, part 3.
        let (rebuilt, records) = strip_parts(&msg, &[3], a_date()).unwrap();
        assert_eq!(records[0].filename.as_deref(), Some("stuff.zip"));

        let root = parse_message(&rebuilt).expect("nested structure must still parse");
        let leaves = root.leaves();
        assert_eq!(leaves.len(), 3);
        assert_eq!(leaves[0].decoded().unwrap(), b"plain");
        assert_eq!(leaves[1].content_type, "text/html");
        assert_eq!(leaves[1].decoded().unwrap(), b"<html/>");
        assert_eq!(leaves[2].content_type, "text/plain"); // the stub
        assert!(String::from_utf8(leaves[2].decoded().unwrap())
            .unwrap()
            .contains("stuff.zip"));
    }

    #[test]
    fn a_surviving_part_may_contain_a_line_that_looks_like_a_boundary() {
        // "--NOTBOUND" is a whole delimiter LINE, but not of the active
        // boundary "BOUND" -- RFC 2046 matches the whole line, so this
        // must round-trip untouched, same as any other body content.
        let msg = "Content-Type: multipart/mixed; boundary=BOUND\r\n\r\n\
             --BOUND\r\n\
             Content-Type: text/plain\r\n\r\nline one\r\n--NOTBOUND\r\nline two\r\n\
             --BOUND\r\n\
             Content-Type: application/pdf\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nSGVsbG8=\r\n\
             --BOUND--\r\n"
            .to_string()
            .into_bytes();

        let original = parse_message(&msg).unwrap();
        let survivor_body = original.leaves()[0].body.clone();

        let (rebuilt, _records) = strip_parts(&msg, &[2], a_date()).unwrap();
        let leaves = parse_message(&rebuilt).unwrap();
        let leaves = leaves.leaves();
        assert_eq!(leaves.len(), 2);
        assert_eq!(leaves[0].body, survivor_body);
        assert_eq!(leaves[0].decoded().unwrap(), b"line one\r\n--NOTBOUND\r\nline two");
    }

    #[test]
    fn strip_refuses_an_empty_selection() {
        let msg = simple_message("text/plain", "hi");
        let err = strip_parts(&msg, &[], a_date()).unwrap_err();
        assert!(format!("{:#}", err).contains("no parts"), "{:#}", err);
    }

    #[test]
    fn strip_refuses_a_part_number_that_does_not_exist() {
        let msg = simple_message("text/plain", "hi");
        let err = strip_parts(&msg, &[2], a_date()).unwrap_err();
        assert!(format!("{:#}", err).contains("does not exist"), "{:#}", err);

        let err = strip_parts(&msg, &[0], a_date()).unwrap_err();
        assert!(format!("{:#}", err).contains("does not exist"), "{:#}", err);
    }

    #[test]
    fn strip_refuses_a_message_it_cannot_parse() {
        let msg = b"Content-Type: multipart/mixed; boundary=X\r\n\r\nno boundary line here";
        assert!(strip_parts(msg, &[1], a_date()).is_err());
    }

    #[test]
    fn strip_the_sole_part_of_a_single_part_message_needs_no_multipart() {
        // TODO.md section 6: removal never changes a message's
        // top-level structure, so this must not promote the message to
        // multipart/mixed -- it replaces the content and Content-Type
        // directly, keeping the rest of the message headers.
        let msg = "From: a@example.com\r\nSubject: report\r\n\
             Content-Type: application/pdf; name=report.pdf\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nSGVsbG8=\r\n"
            .to_string()
            .into_bytes();

        let (rebuilt, records) = strip_parts(&msg, &[1], a_date()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].filename.as_deref(), Some("report.pdf"));

        let root = parse_message(&rebuilt).expect("stub message must still parse");
        assert_eq!(root.content_type, "text/plain", "not promoted to multipart");
        let leaves = root.leaves();
        assert_eq!(leaves.len(), 1);
        assert!(String::from_utf8(leaves[0].decoded().unwrap())
            .unwrap()
            .contains("report.pdf"));

        let text = String::from_utf8_lossy(&rebuilt);
        assert!(text.contains("From: a@example.com"), "envelope header kept");
        assert!(text.contains("Subject: report"), "envelope header kept");
        assert!(
            text.contains("Content-Type: text/plain"),
            "Content-Type header itself was rewritten, not just described in the stub body: {}",
            text
        );
    }
}

/// Randomized MIME parsing, standing in for the coverage-guided fuzzer
/// this host cannot run.
///
/// `cargo fuzz` needs `-Zsanitizer=address`, which is nightly-only, and
/// there is no nightly toolchain here (Rust comes from the FreeBSD
/// port, and there is no rustup). What a fuzzer buys on a target like
/// this one is mostly panic-hunting rather than memory safety -- the
/// parser holds no `unsafe` -- so the substitute is a seeded generator
/// of awkward and ill-formed messages, plus the invariants that must
/// hold for every one of them.
///
/// The generator is structure-aware on purpose: byte-level mutation
/// almost never produces a well-formed nested multipart, and the
/// defects worth finding here live in boundary matching and nesting.
///
/// Every case comes from a printed seed, so a failure is reproducible:
/// `MIME_FUZZ_SEED=<n> cargo test fuzz::`. `MIME_FUZZ_CASES` sets how
/// many are tried.
#[cfg(test)]
mod fuzz {
    use super::*;

    /// xorshift64*, so the harness carries no dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn chance(&mut self, one_in: usize) -> bool {
            self.below(one_in) == 0
        }

        fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
            let i = self.below(xs.len());
            &xs[i]
        }
    }

    /// Boundaries including the awkward cases a real generator emits:
    /// one that is a prefix of another, the punctuation MUAs use, and
    /// one that looks like a delimiter line itself.
    const BOUNDARIES: &[&str] = &[
        "B", "B1", "B12", "bound", "----=_Part_1_123", "----=_Part_11_456",
        "_000_abc_", "x", "a.b+c",
    ];

    const TYPES: &[&str] = &[
        "text/plain", "text/html", "application/pdf",
        "application/octet-stream", "message/rfc822", "",
    ];

    const ENCODINGS: &[&str] =
        &["7bit", "8bit", "base64", "quoted-printable", "binary", "x-weird"];

    /// File names, including the ones that must never become a
    /// destination path.
    const FILENAMES: &[&str] = &[
        "doc.pdf", "\"q3 numbers.xlsx\"", "../../../tmp/escape",
        "/etc/passwd", "..", "a;b=c", "\"unterminated",
    ];

    const BODIES: &[&str] = &["hello", "SGVsbG8=", "caf=E9", "=", "", "--B", "--B--"];

    /// One message, to the given nesting depth.
    ///
    /// The caller caps `depth` well below the level at which
    /// `parse_message` exhausts the stack: that unbounded recursion is
    /// a known defect, not something this harness should trip over on
    /// every run. Raise the cap once the parser bounds its own depth.
    fn build(rng: &mut Rng, depth: usize) -> String {
        if depth == 0 || rng.chance(3) {
            let ty = rng.pick(TYPES);
            let mut out = String::new();
            if !ty.is_empty() {
                out.push_str(&format!("Content-Type: {}", ty));
                if rng.chance(2) {
                    out.push_str(&format!("; name={}", rng.pick(FILENAMES)));
                }
                out.push_str("\r\n");
            }
            if rng.chance(2) {
                out.push_str(&format!(
                    "Content-Disposition: attachment; filename={}\r\n",
                    rng.pick(FILENAMES)
                ));
            }
            out.push_str(&format!(
                "Content-Transfer-Encoding: {}\r\n",
                rng.pick(ENCODINGS)
            ));
            out.push_str("\r\n");
            out.push_str(rng.pick(BODIES));
            out.push_str("\r\n");
            return out;
        }

        let boundary = rng.pick(BOUNDARIES);
        let kind = rng.pick(&["mixed", "alternative", "related"]);
        let mut out = format!("Content-Type: multipart/{}; ", kind);
        // A quoted boundary, a bare one, or (rarely) none at all.
        out.push_str(&match rng.below(8) {
            0 => "\r\n".to_string(),
            1..=3 => format!("boundary=\"{}\"\r\n", boundary),
            _ => format!("boundary={}\r\n", boundary),
        });
        out.push_str("\r\n");
        if rng.chance(4) {
            out.push_str("preamble text\r\n");
        }
        for _ in 0..1 + rng.below(3) {
            out.push_str(&format!("--{}\r\n", boundary));
            out.push_str(&build(rng, depth - 1));
        }
        // Closed properly, closed with RFC 2046 transport padding,
        // closed without a line break, or not closed at all.
        match rng.below(6) {
            0 => {}
            1 => out.push_str(&format!("--{}-- \r\n", boundary)),
            2 => out.push_str(&format!("--{}--", boundary)),
            _ => out.push_str(&format!("--{}--\r\n", boundary)),
        }
        if rng.chance(4) {
            out.push_str("epilogue text\r\n");
        }
        out
    }

    /// Parse, walk and decode. Any of the three may fail; none may
    /// panic, and none may take the process down.
    fn check(raw: &[u8], _seed: u64) {
        let Ok(root) = parse_message(raw) else {
            return; // Refusing malformed input is correct behaviour.
        };
        for leaf in &root.leaves() {
            // Decoding may fail; it may not panic.
            let _ = leaf.decoded();
        }
    }

    /// Every case the generator can produce, as a corpus the
    /// invariant tests share.
    fn corpus(cases: usize) -> Vec<(u64, String)> {
        let base: u64 = std::env::var("MIME_FUZZ_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0x5eed_1234_abcd_0001);
        (0..cases)
            .map(|i| {
                let seed = base
                    .wrapping_add(i as u64)
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    | 1;
                let mut rng = Rng(seed);
                let depth = 1 + rng.below(4);
                (seed, build(&mut rng, depth))
            })
            .collect()
    }

    /// A spec-legal message, paired with the leaves it is made of.
    ///
    /// This is the oracle the panic-hunting tests lack: the generator
    /// knows what the parser is supposed to come back with, so a
    /// mis-split shows up as a mismatch rather than having to crash to
    /// be noticed. Everything here is legal under RFC 2046 -- including
    /// the two legal-but-awkward shapes real MUAs emit: a boundary that
    /// is a prefix of another (`B` inside `B1`), and transport padding
    /// after the close-delimiter.
    fn build_checked(rng: &mut Rng, depth: usize, used: &mut Vec<String>) -> (String, Vec<(String, String)>) {
        build_inner(rng, depth, used, true)
    }

    fn build_inner(
        rng: &mut Rng,
        depth: usize,
        used: &mut Vec<String>,
        must_be_multipart: bool,
    ) -> (String, Vec<(String, String)>) {
        if !must_be_multipart && (depth == 0 || rng.chance(3)) {
            let ty = *rng.pick(&["text/plain", "text/html", "application/pdf"]);
            let body = *rng.pick(&["hello", "body text", "PDFBYTES", "line one"]);
            return (
                format!("Content-Type: {}\r\n\r\n{}\r\n", ty, body),
                vec![(ty.to_string(), body.to_string())],
            );
        }
        // Any boundary not already in use in this message. Boundaries
        // that are prefixes of an enclosing one are legal: RFC 2046
        // matches a whole delimiter LINE.
        let boundary = {
            let mut b;
            loop {
                b = format!("{}{}", rng.pick(&["B", "X", "bound", "----=_Part_"]), rng.below(30));
                if !used.contains(&b) {
                    break;
                }
            }
            used.push(b.clone());
            b
        };
        let mut out = format!("Content-Type: multipart/mixed; boundary={}\r\n\r\n", boundary);
        let mut leaves = Vec::new();
        for _ in 0..1 + rng.below(3) {
            let (text, mut sub) = build_inner(rng, depth.saturating_sub(1), used, false);
            out.push_str(&format!("--{}\r\n", boundary));
            out.push_str(&text);
            leaves.append(&mut sub);
        }
        // RFC 2046 allows transport-padding after the close-delimiter.
        let pad = rng.chance(3);
        out.push_str(&format!("--{}--{}\r\n", boundary, if pad { " " } else { "" }));
        (out, leaves)
    }

    /// Every leaf of a spec-legal message comes back, in order, with
    /// its own content type and body.
    ///
    /// Holds the delimiter-line fix in place. It failed before it --
    /// on a boundary that is a prefix of an enclosing one (the message
    /// was refused) and on transport padding after the close-delimiter
    /// (an empty trailing part was invented) -- and passes over 600k
    /// generated messages after it.
    #[test]
    fn a_legal_message_parses_back_to_the_leaves_it_was_built_from() {
        let base: u64 = std::env::var("MIME_FUZZ_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0x01ac_1e00_0000_0001u64);
        for i in 0..case_count() {
            let seed = base
                .wrapping_add(i as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                | 1;
            let mut rng = Rng(seed);
            let mut used = Vec::new();
            let depth = 1 + rng.below(3);
            let (msg, expected) = build_checked(&mut rng, depth, &mut used);
            let root = match parse_message(msg.as_bytes()) {
                Ok(r) => r,
                Err(e) => panic!("seed {}: legal message refused: {:#}\n{}", seed, e, msg),
            };
            let got: Vec<(String, String)> = root
                .leaves()
                .iter()
                .map(|l| {
                    (
                        l.content_type.clone(),
                        String::from_utf8_lossy(&l.decoded().unwrap_or_default()).to_string(),
                    )
                })
                .collect();
            assert_eq!(got, expected, "seed {}: leaves differ\n{}", seed, msg);
        }
    }


    fn case_count() -> usize {
        std::env::var("MIME_FUZZ_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3000)
    }

    #[test]
    fn random_messages_parse_without_panicking() {
        for (seed, msg) in corpus(case_count()) {
            check(msg.as_bytes(), seed);
        }
    }

    /// A part's file name reaches `part save` as the destination path
    /// when `-o` is absent, so it must not be able to name anywhere
    /// but the current directory.
    ///
    /// A part's file name reaches `parts_save` as the destination path
    /// when `-o` is absent. This asserts the parser still hands such
    /// names through unchanged -- `safe_part_filename` is what refuses
    /// them, and `cli::tests` covers that -- so the two halves cannot
    /// drift apart silently.
    #[test]
    fn a_part_filename_cannot_name_another_directory() {
        for (seed, msg) in corpus(case_count()) {
            let Ok(root) = parse_message(msg.as_bytes()) else {
                continue;
            };
            for leaf in &root.leaves() {
                let Some(name) = &leaf.filename else { continue };
                assert_eq!(
                    crate::cli::safe_part_filename(Some(name), 7, 2)
                        .components()
                        .count(),
                    1,
                    "seed {}: part filename {:?} escaped sanitising",
                    seed,
                    name
                );
            }
        }
    }

    #[test]
    fn random_bytes_parse_without_panicking() {
        let base: u64 = std::env::var("MIME_FUZZ_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0x0bad_f00d_0000_0001);
        for i in 0..case_count() as u64 {
            let seed = base.wrapping_add(i).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut rng = Rng(seed);
            let len = rng.below(400);
            let bytes: Vec<u8> = (0..len).map(|_| (rng.next() & 0xff) as u8).collect();
            check(&bytes, seed);
        }
    }

    /// The same treatment for the other hand-written decoder, where a
    /// real oracle exists: decoding is the inverse of encoding for
    /// every name `encode` itself produces.
    #[test]
    fn modified_utf7_round_trips_under_random_names() {
        use crate::cli::modutf7;
        let alphabet: Vec<char> =
            "abzAZ09 &-+,/\"\\\u{e9}\u{53f0}\u{5317}\u{1F600}\u{00ad}~".chars().collect();
        let mut rng = Rng(0xfeed_beef_0000_0001);
        for _ in 0..5000 {
            let len = rng.below(12);
            let name: String = (0..len).map(|_| *rng.pick(&alphabet)).collect();
            let encoded = modutf7::encode(&name);
            assert!(encoded.is_ascii(), "encode({:?}) left non-ASCII", name);
            assert_eq!(
                modutf7::decode(&encoded).ok().as_deref(),
                Some(name.as_str()),
                "round trip of {:?} through {:?}",
                name,
                encoded
            );
            assert!(
                modutf7::is_canonical(&encoded),
                "encode({:?}) = {:?} is not its own canonical form",
                name,
                encoded
            );
        }
    }
}
