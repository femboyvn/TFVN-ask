//! Bundle a member's export request into one Markdown attachment.

use serde_json::{json, Value};

const MAX_FILENAME_CHARS: usize = 60;
const MAX_PARTS: usize = 8;
const MAX_LANGUAGE_CHARS: usize = 20;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_REPLY_ATTACHMENTS: usize = 10;
const FILE_UNUSABLE: &str = "The export file could not be attached.";

pub fn accepts_argument(key: &str) -> bool {
    matches!(key, "filename" | "parts")
}

/// Validate an export request and bundle it into one Markdown file.
pub fn build(arguments: &Value) -> Result<Value, String> {
    let object = arguments
        .as_object()
        .ok_or("Export arguments must be an object")?;
    if object.keys().any(|key| !accepts_argument(key)) {
        return Err("Unknown export argument".into());
    }
    let filename = attachment_name(
        object
            .get("filename")
            .and_then(Value::as_str)
            .ok_or("Missing export filename")?,
    )?;
    let parts = object
        .get("parts")
        .and_then(Value::as_array)
        .ok_or("Export parts must be a list of 1 through 8 items")?;
    if parts.is_empty() || parts.len() > MAX_PARTS {
        return Err("Export parts must be a list of 1 through 8 items".into());
    }
    let parts = parts
        .iter()
        .map(parse_part)
        .collect::<Result<Vec<_>, _>>()?;
    let markdown = render(&parts)?;
    if markdown.len() > MAX_FILE_BYTES {
        return Err("That export is larger than 8 MiB.".into());
    }
    Ok(json!({
        "filename": filename,
        "parts": parts.len(),
        "file_base64": base64_encode(markdown.as_bytes()),
    }))
}

/// Remove a finished export from a tool result so the model sees only a caption flag.
pub fn detach_file(value: &mut Value) -> Result<Option<(String, Vec<u8>)>, String> {
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    let Some(encoded) = object.remove("file_base64") else {
        return Ok(None);
    };
    let encoded = encoded.as_str().ok_or(FILE_UNUSABLE)?;
    let bytes = base64_decode(encoded)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("That export is larger than 8 MiB.".into());
    }
    let filename = object
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or("export.md")
        .to_owned();
    object.insert("file_attached".into(), json!(true));
    Ok(Some((filename, bytes)))
}

/// Pick a download name that does not collide with files already attached.
pub fn unique_filename(used: &[&str], filename: &str) -> String {
    if !used.contains(&filename) {
        return filename.to_owned();
    }
    let stem = filename.strip_suffix(".md").unwrap_or(filename);
    let mut index = 2u32;
    loop {
        let candidate = format!("{stem}-{index}.md");
        if !used.contains(&candidate.as_str()) {
            return candidate;
        }
        index += 1;
    }
}

pub fn can_attach_export(images: usize, files: usize) -> bool {
    images.saturating_add(files) < MAX_REPLY_ATTACHMENTS
}

struct Part {
    kind: Kind,
    title: Option<String>,
    language: Option<String>,
    body: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Excel,
    Markdown,
    Text,
    Code,
}

impl Kind {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "excel" => Ok(Self::Excel),
            "markdown" => Ok(Self::Markdown),
            "text" => Ok(Self::Text),
            "code" => Ok(Self::Code),
            _ => Err("Export kind must be excel, markdown, text, or code".into()),
        }
    }

    fn heading(self) -> &'static str {
        match self {
            Self::Excel => "Sheet",
            Self::Text => "Note",
            Self::Code => "Code",
            Self::Markdown => "Markdown",
        }
    }
}

fn parse_part(value: &Value) -> Result<Part, String> {
    let object = value
        .as_object()
        .ok_or("Each export part must be an object")?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "kind" | "title" | "language" | "body"))
    {
        return Err("Unknown export part field".into());
    }
    let kind = Kind::parse(
        object
            .get("kind")
            .and_then(Value::as_str)
            .ok_or("Export kind must be excel, markdown, text, or code")?,
    )?;
    let title = parse_title(object.get("title"))?;
    let language = parse_language(object.get("language"), kind)?;
    let body = match object.get("body") {
        Some(Value::String(body)) => body.as_str(),
        Some(_) => return Err("Export text must be a string".into()),
        None => return Err("Missing export text".into()),
    };
    if body.trim().is_empty() {
        return Err("Export text must not be empty".into());
    }
    if has_disallowed_control(body) {
        return Err("Export text cannot include control characters".into());
    }
    Ok(Part {
        kind,
        title,
        language,
        body: body.to_owned(),
    })
}

fn parse_title(value: Option<&Value>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let title = value.as_str().ok_or("Export title must be text")?;
    let title = title.trim();
    if title.is_empty() {
        return Err("Export title cannot be empty".into());
    }
    if title.chars().any(|ch| ch.is_ascii_control()) {
        return Err("Export title cannot include control characters".into());
    }
    Ok(Some(title.to_owned()))
}

fn parse_language(value: Option<&Value>, kind: Kind) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return if kind == Kind::Code {
            Err("Code exports need a language".into())
        } else {
            Ok(None)
        };
    };
    let Some(language) = value.as_str() else {
        return Err(
            "Code language must be 1 through 20 characters using letters, digits, + # or -".into(),
        );
    };
    let language = language.trim();
    if language.is_empty() {
        return if kind == Kind::Code {
            Err("Code exports need a language".into())
        } else {
            Ok(None)
        };
    }
    if language.chars().count() > MAX_LANGUAGE_CHARS
        || language
            .chars()
            .any(|ch| !(ch.is_ascii_alphanumeric() || matches!(ch, '+' | '#' | '-')))
    {
        return Err(
            "Code language must be 1 through 20 characters using letters, digits, + # or -".into(),
        );
    }
    if kind == Kind::Code {
        Ok(Some(language.to_owned()))
    } else {
        Ok(None)
    }
}

fn has_disallowed_control(text: &str) -> bool {
    text.chars()
        .any(|ch| ch.is_ascii_control() && !matches!(ch, '\n' | '\r' | '\t'))
}

fn attachment_name(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let length = raw.chars().count();
    if !(1..=MAX_FILENAME_CHARS).contains(&length) {
        return Err("Export filename must be 1 through 60 characters".into());
    }
    if raw.contains('/') || raw.contains('\\') || raw.contains("..") {
        return Err("Export filename must not include a path".into());
    }
    if raw
        .chars()
        .any(|ch| !(ch.is_alphanumeric() || matches!(ch, ' ' | '_' | '-' | '.')))
    {
        return Err("Export filename can use only letters, digits, spaces, and . _ -".into());
    }
    let stem = match raw.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => stem,
        Some((stem, "")) if !stem.is_empty() => stem,
        _ => raw,
    };
    if stem.is_empty() || stem.chars().all(|ch| ch == '.' || ch.is_whitespace()) {
        return Err("Export filename must not include a path".into());
    }
    Ok(format!("{stem}.md"))
}

fn render(parts: &[Part]) -> Result<String, String> {
    if let [part] = parts {
        if part.kind == Kind::Markdown {
            return Ok(part.body.clone());
        }
    }
    let mut document = String::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            document.push('\n');
        }
        let heading = part.title.as_deref().unwrap_or(part.kind.heading());
        document.push_str("## ");
        document.push_str(heading);
        document.push_str("\n\n");
        document.push_str(&part.rendered_body()?);
        if !document.ends_with('\n') {
            document.push('\n');
        }
    }
    Ok(document)
}

impl Part {
    fn rendered_body(&self) -> Result<String, String> {
        match self.kind {
            Kind::Excel => Ok(excel_section(&parse_csv(&self.body)?)),
            Kind::Code => {
                let language = self
                    .language
                    .as_deref()
                    .ok_or("Code exports need a language")?;
                Ok(fenced(language, &self.body))
            }
            Kind::Text | Kind::Markdown => Ok(self.body.clone()),
        }
    }
}

fn excel_section(rows: &[Vec<String>]) -> String {
    let mut out = markdown_table(rows);
    out.push('\n');
    out.push_str(&fenced("csv", &encode_csv(rows)));
    out
}

fn markdown_table(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    write_row(&mut out, &rows[0], false);
    out.push('|');
    for _ in &rows[0] {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in &rows[1..] {
        write_row(&mut out, row, false);
    }
    out
}

fn write_row(out: &mut String, row: &[String], quote_formulas: bool) {
    if quote_formulas {
        for (index, cell) in row.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&csv_cell(cell));
        }
        return;
    }
    out.push('|');
    for cell in row {
        out.push(' ');
        out.push_str(&cell.replace('|', "\\|"));
        out.push_str(" |");
    }
    out.push('\n');
}

fn encode_csv(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        write_row(&mut out, row, true);
    }
    out
}

fn csv_cell(value: &str) -> String {
    let mut text = value.to_owned();
    if text.starts_with(['=', '+', '-', '@']) {
        text.insert(0, '\'');
    }
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

fn fenced(language: &str, body: &str) -> String {
    let fence = fence_token(body);
    let mut out = String::new();
    out.push_str(&fence);
    out.push_str(language);
    out.push('\n');
    out.push_str(body);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out.push('\n');
    out
}

fn fence_token(body: &str) -> String {
    let mut longest = 0usize;
    let mut run = 0usize;
    for ch in body.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest.max(2) + 1)
}

fn parse_csv(body: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_quotes = false,
                '\n' | '\r' => {
                    return Err("An Excel cell cannot contain a line break".into());
                }
                _ => field.push(ch),
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() => in_quotes = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\n' | '\r' => {
                if ch == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(ch),
        }
    }
    if in_quotes {
        return Err("An Excel row has an unfinished quoted cell".into());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    if rows.is_empty() || rows[0].iter().all(|cell| cell.trim().is_empty()) {
        return Err("An Excel export needs a header row".into());
    }
    let width = rows[0].len();
    if rows.iter().any(|row| row.len() != width) {
        return Err("Each Excel row must have the same number of columns".into());
    }
    Ok(rows)
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let block = (u32::from(bytes[index]) << 16)
            | (u32::from(bytes[index + 1]) << 8)
            | u32::from(bytes[index + 2]);
        for shift in [18, 12, 6, 0] {
            out.push(char::from(TABLE[((block >> shift) & 0x3f) as usize]));
        }
        index += 3;
    }
    let rest = bytes.len() - index;
    if rest == 1 {
        let block = u32::from(bytes[index]) << 16;
        out.push(char::from(TABLE[((block >> 18) & 0x3f) as usize]));
        out.push(char::from(TABLE[((block >> 12) & 0x3f) as usize]));
        out.push('=');
        out.push('=');
    } else if rest == 2 {
        let block = (u32::from(bytes[index]) << 16) | (u32::from(bytes[index + 1]) << 8);
        out.push(char::from(TABLE[((block >> 18) & 0x3f) as usize]));
        out.push(char::from(TABLE[((block >> 12) & 0x3f) as usize]));
        out.push(char::from(TABLE[((block >> 6) & 0x3f) as usize]));
        out.push('=');
    }
    out
}

fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return Err(FILE_UNUSABLE.into());
    }
    let pad_at = bytes
        .iter()
        .position(|byte| *byte == b'=')
        .unwrap_or(bytes.len());
    if bytes[pad_at..].iter().any(|byte| *byte != b'=') || bytes.len() - pad_at > 2 {
        return Err(FILE_UNUSABLE.into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().filter(|byte| **byte == b'=').count();
        let mut parts = [0u8; 4];
        for (index, byte) in chunk.iter().enumerate() {
            if *byte == b'=' {
                continue;
            }
            parts[index] = base64_value(*byte).ok_or(FILE_UNUSABLE)?;
        }
        let block = (u32::from(parts[0]) << 18)
            | (u32::from(parts[1]) << 12)
            | (u32::from(parts[2]) << 6)
            | u32::from(parts[3]);
        out.push((block >> 16) as u8);
        if pad < 2 {
            out.push((block >> 8) as u8);
        }
        if pad < 1 {
            out.push(block as u8);
        }
    }
    Ok(out)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn attached(arguments: Value) -> (String, String) {
        let mut value = build(&arguments).unwrap();
        let (filename, bytes) = detach_file(&mut value).unwrap().unwrap();
        assert!(value.get("file_base64").is_none());
        assert_eq!(value["file_attached"], true);
        assert_eq!(value["filename"], filename);
        (filename, String::from_utf8(bytes).expect("export is utf-8"))
    }

    #[test]
    fn bundles_excel_text_and_code_into_one_markdown_file() {
        let (filename, markdown) = attached(json!({
            "filename": "pack.txt",
            "parts": [
                {"kind": "excel", "title": "Budget", "body": "Item,Amount\nTea,2\nCake,5"},
                {"kind": "text", "title": "Note", "body": "Bring cups"},
                {"kind": "code", "title": "Script", "language": "python", "body": "print('hi')"}
            ]
        }));
        assert_eq!(filename, "pack.md");
        assert_eq!(
            markdown,
            "## Budget\n\n| Item | Amount |\n| --- | --- |\n| Tea | 2 |\n| Cake | 5 |\n\n```csv\nItem,Amount\nTea,2\nCake,5\n```\n\n## Note\n\nBring cups\n\n## Script\n\n```python\nprint('hi')\n```\n"
        );
    }

    #[test]
    fn lone_markdown_part_is_the_file_body() {
        let body = "# Notes\n\nKeep this wording.\n";
        let (filename, markdown) = attached(json!({
            "filename": "notes.md",
            "parts": [{"kind": "markdown", "body": body}]
        }));
        assert_eq!(filename, "notes.md");
        assert_eq!(markdown, body);
    }

    #[test]
    fn keeps_markdown_longer_than_a_discord_message() {
        let body = format!("{}{}", "ả".repeat(40), "y".repeat(2_000));
        assert!(body.chars().count() > 80);
        assert!(body.chars().count() > 1_900);
        let (_, markdown) = attached(json!({
            "filename": "notes.md",
            "parts": [{"kind": "markdown", "body": body.clone()}]
        }));
        assert_eq!(markdown, body);
        assert_eq!(markdown.chars().count(), body.chars().count());
    }

    #[test]
    fn escapes_table_pipes_and_quotes_formula_cells_in_the_csv_fence() {
        let (_, markdown) = attached(json!({
            "filename": "sheet.md",
            "parts": [{"kind": "excel", "body": "Item,Note\na|b,=SUM(1)\n-3,+1"}]
        }));
        assert!(markdown.contains("| a\\|b | =SUM(1) |"));
        assert!(markdown.contains("| -3 | +1 |"));
        assert!(markdown.contains("a|b,'=SUM(1)\n'-3,'+1"));
    }

    #[test]
    fn rejects_paths_and_uses_an_md_suffix() {
        let (filename, _) = attached(json!({
            "filename": "Budget.xlsx",
            "parts": [{"kind": "text", "body": "rows"}]
        }));
        assert_eq!(filename, "Budget.md");
        let (filename, _) = attached(json!({
            "filename": "Bảng.md",
            "parts": [{"kind": "text", "body": "rows"}]
        }));
        assert_eq!(filename, "Bảng.md");
        for name in ["../notes.txt", "dir/notes.md", "..\\secret.md"] {
            assert_eq!(
                build(&json!({"filename": name, "parts": [{"kind": "text", "body": "rows"}]}))
                    .unwrap_err(),
                "Export filename must not include a path"
            );
        }
    }

    #[test]
    fn rejects_bad_exports_before_building_a_file() {
        let err = |arguments: Value| build(&arguments).unwrap_err();
        assert_eq!(err(json!([])), "Export arguments must be an object");
        assert_eq!(
            err(
                json!({"filename": "notes.md", "topic": "x", "parts": [{"kind": "text", "body": "a"}]})
            ),
            "Unknown export argument"
        );
        assert_eq!(
            err(
                json!({"filename": "notes.md", "parts": [{"kind": "text", "body": "a", "extra": 1}]})
            ),
            "Unknown export part field"
        );
        assert_eq!(
            err(json!({"filename": "notes.md", "parts": [{"kind": "code", "body": "a"}]})),
            "Code exports need a language"
        );
        assert_eq!(
            err(json!({"filename": "notes.md", "parts": [{"kind": "excel", "body": "a,b\n1"}]})),
            "Each Excel row must have the same number of columns"
        );
        assert_eq!(
            err(
                json!({"filename": "notes.md", "parts": [{"kind": "text", "body": "bad\u{0001}text"}]})
            ),
            "Export text cannot include control characters"
        );
        assert_eq!(
            err(
                json!({"filename": "notes.md", "parts": [{"kind": "markdown", "body": "x".repeat(MAX_FILE_BYTES + 1)}]})
            ),
            "That export is larger than 8 MiB."
        );
    }

    #[test]
    fn detach_file_removes_the_payload_and_marks_it_attached() {
        let mut value = build(&json!({
            "filename": "notes.md",
            "parts": [{"kind": "markdown", "body": "hello"}]
        }))
        .unwrap();
        assert!(value.get("file_base64").is_some());
        let (filename, bytes) = detach_file(&mut value).unwrap().unwrap();
        assert_eq!(filename, "notes.md");
        assert_eq!(bytes, b"hello");
        assert!(value.get("file_base64").is_none());
        assert_eq!(value["file_attached"], true);
        assert_eq!(value["parts"], 1);
        assert_eq!(
            detach_file(&mut json!({"filename": "notes.md"})).unwrap(),
            None
        );
        assert_eq!(
            detach_file(&mut json!({"file_base64": "!!!!"})).unwrap_err(),
            FILE_UNUSABLE
        );
    }

    #[test]
    fn unique_filename_numbers_a_repeated_stem() {
        assert_eq!(unique_filename(&[], "Budget.md"), "Budget.md");
        assert_eq!(unique_filename(&["Budget.md"], "Budget.md"), "Budget-2.md");
        assert_eq!(
            unique_filename(&["Budget.md", "Budget-2.md"], "Budget.md"),
            "Budget-3.md"
        );
        assert!(can_attach_export(9, 0));
        assert!(!can_attach_export(6, 4));
        assert!(!can_attach_export(10, 0));
    }
}
