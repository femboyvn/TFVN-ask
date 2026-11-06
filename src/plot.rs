//! PNG charts for the plot chat tool.
//!
//! Argument checks and expression sampling stay here. A fixed matplotlib script
//! draws the image from those points. Member text is never executed as code.

use crate::calculator;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

const MAX_SERIES: usize = 3;
const MAX_POINTS: usize = 30;
const MAX_TITLE_CHARS: usize = 60;
const MAX_AXIS_CHARS: usize = 24;
const MAX_NAME_CHARS: usize = 30;
const MAX_LABEL_CHARS: usize = 12;
const DEFAULT_SAMPLES: usize = 21;
const MAX_PNG_BYTES: usize = 1_500_000;
const DRAW_TIMEOUT: Duration = Duration::from_secs(20);
const PLOT_SCRIPT: &str = include_str!("plot.py");
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
const RENDERER_UNAVAILABLE: &str = "The chart renderer is unavailable.";
const CHART_FAILED: &str = "Could not draw the chart.";
const IMAGE_UNUSABLE: &str = "The chart image was unusable.";

static DRAW_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Line,
    Bar,
    Scatter,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Bar => "bar",
            Self::Scatter => "scatter",
        }
    }
}

struct Series {
    name: Option<String>,
    points: Vec<Point>,
}

struct Point {
    x: f64,
    y: f64,
    label: Option<String>,
}

struct RawPoint {
    x: Option<f64>,
    y: f64,
    label: Option<String>,
}

struct Chart {
    kind: Kind,
    title: Option<String>,
    x_label: Option<String>,
    y_label: Option<String>,
    expression: Option<String>,
    samples: Option<usize>,
    series: Vec<Series>,
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct CappedRead {
    bytes: Vec<u8>,
    overflow: bool,
}

pub fn accepts_argument(key: &str) -> bool {
    matches!(
        key,
        "kind"
            | "title"
            | "x_label"
            | "y_label"
            | "series"
            | "expression"
            | "x_min"
            | "x_max"
            | "samples"
    )
}

/// Validate a plot request and sample an expression. This does not start Python.
#[cfg(test)]
fn prepare(arguments: &Value) -> Result<Value, String> {
    Ok(summary(&prepare_chart(arguments)?))
}

pub async fn render(arguments: &Value) -> Result<Value, String> {
    let chart = prepare_chart(arguments)?;
    let png = draw_png(&chart).await?;
    let mut result = summary(&chart);
    result["image_png_base64"] = json!(base64_encode(&png));
    result["media_type"] = json!("image/png");
    result["filename"] = json!("plot.png");
    Ok(result)
}

/// Remove a successful chart image from a tool result so the model sees only a caption flag.
pub fn detach_image(value: &mut Value) -> Result<Option<Vec<u8>>, String> {
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    let Some(encoded) = object.remove("image_png_base64") else {
        return Ok(None);
    };
    let encoded = encoded.as_str().ok_or(IMAGE_UNUSABLE)?;
    let bytes = base64_decode(encoded)?;
    accept_png(&bytes)?;
    object.insert("image_attached".into(), json!(true));
    Ok(Some(bytes))
}

fn prepare_chart(arguments: &Value) -> Result<Chart, String> {
    let object = arguments
        .as_object()
        .ok_or("Plot arguments must be an object")?;
    if object.keys().any(|key| !accepts_argument(key)) {
        return Err("Unknown plot argument".into());
    }
    let kind = parse_kind(object.get("kind"))?;
    let title = optional_text(
        object,
        "title",
        MAX_TITLE_CHARS,
        "Plot title must be 1 through 60 characters",
    )?;
    let x_label = optional_text(
        object,
        "x_label",
        MAX_AXIS_CHARS,
        "Plot axis label must be 1 through 24 characters",
    )?;
    let y_label = optional_text(
        object,
        "y_label",
        MAX_AXIS_CHARS,
        "Plot axis label must be 1 through 24 characters",
    )?;
    let plotted = parse_body(object, kind)?;
    Ok(Chart {
        kind,
        title,
        x_label,
        y_label,
        expression: plotted.expression,
        samples: plotted.samples,
        series: plotted.series,
    })
}

fn parse_kind(value: Option<&Value>) -> Result<Kind, String> {
    match value {
        None => Ok(Kind::Line),
        Some(Value::String(kind)) => match kind.as_str() {
            "line" => Ok(Kind::Line),
            "bar" => Ok(Kind::Bar),
            "scatter" => Ok(Kind::Scatter),
            _ => Err("Plot kind must be line, bar, or scatter".into()),
        },
        Some(_) => Err("Plot kind must be line, bar, or scatter".into()),
    }
}

struct ParsedBody {
    series: Vec<Series>,
    expression: Option<String>,
    samples: Option<usize>,
}

fn parse_body(object: &Map<String, Value>, kind: Kind) -> Result<ParsedBody, String> {
    let has_series = object.contains_key("series");
    let has_expression = object.contains_key("expression");
    if has_series && has_expression {
        return Err("Pass series or an expression, not both".into());
    }
    if has_expression {
        return parse_expression_plot(object, kind);
    }
    if !has_series {
        return Err("Plot needs series or an expression".into());
    }
    if object.contains_key("x_min")
        || object.contains_key("x_max")
        || object.contains_key("samples")
    {
        return Err("x_min, x_max, and samples apply only to an expression".into());
    }
    Ok(ParsedBody {
        series: parse_series(object.get("series"), kind)?,
        expression: None,
        samples: None,
    })
}

fn parse_expression_plot(object: &Map<String, Value>, kind: Kind) -> Result<ParsedBody, String> {
    if kind == Kind::Bar {
        return Err("Bar charts plot series, not an expression".into());
    }
    let expression = parse_expression(
        object
            .get("expression")
            .ok_or("Plot expression must be 1 through 200 characters")?,
    )?;
    let (x_min, x_max) = parse_range(object)?;
    let samples = parse_samples(object.get("samples"))?;
    let series = sample_expression(&expression, x_min, x_max, samples)?;
    Ok(ParsedBody {
        series: vec![series],
        expression: Some(expression),
        samples: Some(samples),
    })
}

fn parse_expression(value: &Value) -> Result<String, String> {
    let Value::String(expression) = value else {
        return Err("Plot expression must be 1 through 200 characters".into());
    };
    let expression = clean_text(
        expression,
        200,
        "Plot expression must be 1 through 200 characters",
    )?;
    if expression.len() > 200 {
        return Err("Plot expression must be 1 through 200 characters".into());
    }
    Ok(expression)
}

fn parse_range(object: &Map<String, Value>) -> Result<(f64, f64), String> {
    let (Some(x_min), Some(x_max)) = (object.get("x_min"), object.get("x_max")) else {
        return Err("x_min and x_max are required to plot an expression".into());
    };
    let (Some(x_min), Some(x_max)) = (finite_number(x_min), finite_number(x_max)) else {
        return Err("x_min and x_max must be finite numbers".into());
    };
    if x_min >= x_max {
        return Err("x_min must be less than x_max".into());
    }
    Ok((x_min, x_max))
}

fn parse_samples(value: Option<&Value>) -> Result<usize, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_SAMPLES);
    };
    value
        .as_u64()
        .filter(|count| (2..=MAX_POINTS as u64).contains(count))
        .map(|count| count as usize)
        .ok_or_else(|| "Plot samples must be 2 through 30".into())
}

fn sample_expression(
    expression: &str,
    x_min: f64,
    x_max: f64,
    samples: usize,
) -> Result<Series, String> {
    let steps = (samples - 1) as f64;
    let mut points = Vec::with_capacity(samples);
    for index in 0..samples {
        let x = x_min + (x_max - x_min) * (index as f64 / steps);
        let y = calculator::evaluate_at(expression, x)?;
        points.push(Point { x, y, label: None });
    }
    Ok(Series { name: None, points })
}

fn parse_series(value: Option<&Value>, kind: Kind) -> Result<Vec<Series>, String> {
    let items = value
        .and_then(Value::as_array)
        .ok_or("Plot series must be an array")?;
    if items.is_empty() || items.len() > MAX_SERIES {
        return Err("Plot accepts 1 through 3 series".into());
    }
    if kind == Kind::Bar && items.len() != 1 {
        return Err("Bar charts take one series".into());
    }
    items.iter().map(parse_one_series).collect()
}

fn parse_one_series(value: &Value) -> Result<Series, String> {
    let object = value.as_object().ok_or("Plot series must be an object")?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "name" | "points"))
    {
        return Err("Unknown plot series field".into());
    }
    let name = optional_text(
        object,
        "name",
        MAX_NAME_CHARS,
        "Series name must be 1 through 30 characters",
    )?;
    let points = parse_points(object.get("points"))?;
    Ok(Series { name, points })
}

fn parse_points(value: Option<&Value>) -> Result<Vec<Point>, String> {
    let Some(value) = value else {
        return Err("Missing points".into());
    };
    let items = value.as_array().ok_or("Plot points must be an array")?;
    if items.is_empty() || items.len() > MAX_POINTS {
        return Err("Each series needs 1 through 30 points".into());
    }
    let raw = items
        .iter()
        .map(parse_point)
        .collect::<Result<Vec<_>, _>>()?;
    assign_x(raw)
}

fn parse_point(value: &Value) -> Result<RawPoint, String> {
    let object = value.as_object().ok_or("Plot point must be an object")?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "x" | "y" | "label"))
    {
        return Err("Unknown plot point field".into());
    }
    let y = match object.get("y") {
        Some(value) => finite_number(value).ok_or("Each point needs a finite y")?,
        None => return Err("Each point needs a finite y".into()),
    };
    let x = match object.get("x") {
        None => None,
        Some(value) => Some(finite_number(value).ok_or("Each point x must be a finite number")?),
    };
    let label = optional_text(
        object,
        "label",
        MAX_LABEL_CHARS,
        "Point label must be 1 through 12 characters",
    )?;
    Ok(RawPoint { x, y, label })
}

fn assign_x(raw: Vec<RawPoint>) -> Result<Vec<Point>, String> {
    let any_x = raw.iter().any(|point| point.x.is_some());
    let all_x = raw.iter().all(|point| point.x.is_some());
    if any_x && !all_x {
        return Err("Give every point an x, or leave x out".into());
    }
    Ok(raw
        .into_iter()
        .enumerate()
        .map(|(index, point)| Point {
            x: point.x.unwrap_or((index + 1) as f64),
            y: point.y,
            label: point.label,
        })
        .collect())
}

fn optional_text(
    object: &Map<String, Value>,
    key: &str,
    max_chars: usize,
    message: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => clean_text(text, max_chars, message).map(Some),
        Some(_) => Err(message.into()),
    }
}

fn clean_text(text: &str, max_chars: usize, message: &str) -> Result<String, String> {
    if text.chars().any(char::is_control) || text.contains('`') {
        return Err("Plot text cannot contain control characters or backticks".into());
    }
    let text = text.trim();
    if text.is_empty() || text.chars().count() > max_chars {
        return Err(message.into());
    }
    Ok(text.to_owned())
}

fn finite_number(value: &Value) -> Option<f64> {
    let number = value.as_f64()?;
    number.is_finite().then_some(number)
}

fn summary(chart: &Chart) -> Value {
    let (x_min, x_max, y_min, y_max) = bounds(&chart.series);
    let series = chart
        .series
        .iter()
        .map(|series| {
            let mut item = json!({"points": series.points.len()});
            if let Some(name) = &series.name {
                item["name"] = json!(name);
            }
            item
        })
        .collect::<Vec<_>>();
    let mut result = json!({
        "kind": chart.kind.as_str(),
        "x_min": x_min,
        "x_max": x_max,
        "y_min": y_min,
        "y_max": y_max,
        "series": series,
        "range_note": "Minimum and maximum cover the plotted points only.",
    });
    if let Some(expression) = &chart.expression {
        result["expression"] = json!(expression);
    }
    if let Some(samples) = chart.samples {
        result["samples"] = json!(samples);
    }
    result
}

fn bounds(series: &[Series]) -> (f64, f64, f64, f64) {
    let mut x_min = f64::INFINITY;
    let mut x_max = f64::NEG_INFINITY;
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;
    for series in series {
        for point in &series.points {
            x_min = x_min.min(point.x);
            x_max = x_max.max(point.x);
            y_min = y_min.min(point.y);
            y_max = y_max.max(point.y);
        }
    }
    (x_min, x_max, y_min, y_max)
}

fn draw_payload(chart: &Chart) -> String {
    let series = chart
        .series
        .iter()
        .map(|series| {
            let points = series
                .points
                .iter()
                .map(|point| {
                    let mut item = json!({"x": point.x, "y": point.y});
                    if let Some(label) = &point.label {
                        item["label"] = json!(label);
                    }
                    item
                })
                .collect::<Vec<_>>();
            let mut item = json!({"points": points});
            if let Some(name) = &series.name {
                item["name"] = json!(name);
            }
            item
        })
        .collect::<Vec<_>>();
    let mut payload = json!({"kind": chart.kind.as_str(), "series": series});
    if let Some(title) = &chart.title {
        payload["title"] = json!(title);
    }
    if let Some(label) = &chart.x_label {
        payload["x_label"] = json!(label);
    }
    if let Some(label) = &chart.y_label {
        payload["y_label"] = json!(label);
    }
    if let Some(expression) = &chart.expression {
        payload["expression"] = json!(expression);
    }
    payload.to_string()
}

async fn draw_png(chart: &Chart) -> Result<Vec<u8>, String> {
    let payload = draw_payload(chart);
    let config_dir = TempDir(std::env::temp_dir().join(format!(
        "knowsphere-mpl-{}-{}",
        std::process::id(),
        DRAW_SEQ.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir_all(&config_dir.0).map_err(|_| RENDERER_UNAVAILABLE.to_owned())?;
    match tokio::time::timeout(DRAW_TIMEOUT, run_renderer(&payload, &config_dir.0)).await {
        Ok(result) => result,
        Err(_) => Err(CHART_FAILED.into()),
    }
}

async fn run_renderer(payload: &str, config_dir: &Path) -> Result<Vec<u8>, String> {
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(PLOT_SCRIPT)
        .env("MPLBACKEND", "Agg")
        .env("MPLCONFIGDIR", config_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| RENDERER_UNAVAILABLE.to_owned())?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| RENDERER_UNAVAILABLE.to_owned())?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| RENDERER_UNAVAILABLE.to_owned())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| RENDERER_UNAVAILABLE.to_owned())?;
    let payload = payload.to_owned();
    let write = async move {
        stdin.write_all(payload.as_bytes()).await?;
        stdin.shutdown().await?;
        Ok::<(), std::io::Error>(())
    };
    let read_out = read_capped(&mut stdout, MAX_PNG_BYTES);
    let read_err = async {
        let CappedRead { bytes, .. } = read_capped(&mut stderr, 8 * 1024).await;
        bytes
    };
    let (written, output, errors) = tokio::join!(write, read_out, read_err);
    let status = child.wait().await.map_err(|_| CHART_FAILED.to_owned())?;
    if written.is_err() {
        return Err(CHART_FAILED.into());
    }
    if !status.success() {
        if renderer_missing(&errors) {
            return Err(RENDERER_UNAVAILABLE.into());
        }
        let note = first_note(&errors);
        if !note.is_empty() {
            eprintln!("Chart renderer failed: {note}");
        }
        return Err(CHART_FAILED.into());
    }
    if output.overflow {
        return Err(CHART_FAILED.into());
    }
    accept_png(&output.bytes).map_err(|_| CHART_FAILED.to_owned())?;
    Ok(output.bytes)
}

async fn read_capped(reader: &mut (impl AsyncRead + Unpin), limit: usize) -> CappedRead {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut overflow = false;
    while let Ok(count) = reader.read(&mut chunk).await {
        if count == 0 {
            break;
        }
        if bytes.len() >= limit {
            overflow = true;
            continue;
        }
        let room = limit - bytes.len();
        if count > room {
            bytes.extend_from_slice(&chunk[..room]);
            overflow = true;
        } else {
            bytes.extend_from_slice(&chunk[..count]);
        }
    }
    CappedRead { bytes, overflow }
}

fn renderer_missing(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr);
    text.contains("ModuleNotFoundError") || text.contains("No module named")
}

fn first_note(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    line.chars()
        .take(180)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn accept_png(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_PNG_BYTES || !bytes.starts_with(PNG_MAGIC) {
        return Err(IMAGE_UNUSABLE.into());
    }
    Ok(())
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
        return Err(IMAGE_UNUSABLE.into());
    }
    let pad_at = bytes
        .iter()
        .position(|byte| *byte == b'=')
        .unwrap_or(bytes.len());
    if bytes[pad_at..].iter().any(|byte| *byte != b'=') || bytes.len() - pad_at > 2 {
        return Err(IMAGE_UNUSABLE.into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().filter(|byte| **byte == b'=').count();
        let mut parts = [0u8; 4];
        for (index, byte) in chunk.iter().enumerate() {
            if *byte == b'=' {
                continue;
            }
            parts[index] = base64_value(*byte).ok_or(IMAGE_UNUSABLE)?;
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

    #[test]
    fn chart_script_uses_the_agg_backend_and_does_not_execute_member_text() {
        assert!(PLOT_SCRIPT.contains("matplotlib.use(\"Agg\")"));
        assert!(!PLOT_SCRIPT.contains("eval("));
        assert!(!PLOT_SCRIPT.contains("exec("));
    }

    #[test]
    fn accepts_only_a_png_within_the_size_cap() {
        assert!(accept_png(b"\x89PNG\r\n\x1a\nrest").is_ok());
        assert_eq!(accept_png(b"not a png").unwrap_err(), IMAGE_UNUSABLE);
        let mut oversized = b"\x89PNG\r\n\x1a\n".to_vec();
        oversized.resize(MAX_PNG_BYTES + 1, 0);
        assert_eq!(accept_png(&oversized).unwrap_err(), IMAGE_UNUSABLE);
    }

    #[test]
    fn detach_image_removes_the_png_and_marks_it_attached() {
        let png = b"\x89PNG\r\n\x1a\nbody";
        let mut value = json!({"kind":"line","image_png_base64": base64_encode(png)});
        let bytes = detach_image(&mut value).unwrap().unwrap();
        assert_eq!(bytes, png);
        assert!(value.get("image_png_base64").is_none());
        assert_eq!(value["image_attached"], true);
        assert_eq!(detach_image(&mut json!({"kind":"line"})).unwrap(), None);
        assert_eq!(
            detach_image(&mut json!({"image_png_base64":"!!!!"})).unwrap_err(),
            IMAGE_UNUSABLE
        );
    }

    #[test]
    fn omitted_x_becomes_the_point_index_and_labels_reach_the_renderer() {
        let chart = prepare_chart(&json!({
            "kind": "bar",
            "title": "Hours",
            "y_label": "count",
            "series": [{"name": "sales", "points": [
                {"label": "Mon", "y": -4},
                {"label": "Tue", "y": 8}
            ]}]
        }))
        .unwrap();
        assert_eq!(chart.series[0].points[0].x, 1.0);
        assert_eq!(chart.series[0].points[1].x, 2.0);
        let payload: Value = serde_json::from_str(&draw_payload(&chart)).unwrap();
        assert_eq!(payload["kind"], "bar");
        assert_eq!(payload["title"], "Hours");
        assert_eq!(payload["y_label"], "count");
        assert_eq!(payload["series"][0]["name"], "sales");
        assert_eq!(payload["series"][0]["points"][0]["label"], "Mon");
        assert_eq!(payload["series"][0]["points"][0]["y"], -4.0);
        assert_eq!(payload["series"][0]["points"][1]["y"], 8.0);
        let summary = prepare(&json!({
            "kind": "bar",
            "series": [{"name": "sales", "points": [{"y": 1}, {"y": 2}]}]
        }))
        .unwrap();
        assert!(summary.get("chart").is_none());
        assert!(summary.get("image_png_base64").is_none());
        assert_eq!(summary["series"][0]["points"], 2);
        assert_eq!(summary["series"][0]["name"], "sales");
    }

    #[test]
    fn expression_samples_x_squared_across_the_requested_range() {
        let arguments = json!({"expression": " x * x ", "x_min": -2, "x_max": 2, "samples": 5});
        let chart = prepare_chart(&arguments).unwrap();
        assert_eq!(chart.series[0].points.len(), 5);
        assert_eq!(chart.series[0].points[0].x, -2.0);
        assert_eq!(chart.series[0].points[2].y, 0.0);
        assert_eq!(chart.series[0].points[4].x, 2.0);
        let result = prepare(&arguments).unwrap();
        assert_eq!(result["kind"], "line");
        assert_eq!(result["expression"], "x * x");
        assert_eq!(result["samples"], 5);
        assert_eq!(result["x_min"], -2.0);
        assert_eq!(result["x_max"], 2.0);
        assert_eq!(result["y_min"], 0.0);
        assert_eq!(result["y_max"], 4.0);
        assert_eq!(result["series"][0]["points"], 5);
        let payload: Value = serde_json::from_str(&draw_payload(&chart)).unwrap();
        assert_eq!(payload["expression"], "x * x");
    }

    #[test]
    fn factorial_expression_plots_integer_and_fractional_samples() {
        let integers = prepare(&json!({
            "expression": "x!",
            "x_min": 0,
            "x_max": 5,
            "samples": 6
        }))
        .unwrap();
        assert_eq!(integers["expression"], "x!");
        assert_eq!(integers["y_min"], 1.0);
        assert_eq!(integers["y_max"], 120.0);

        let halfway = prepare(&json!({
            "expression": "x!",
            "x_min": 0,
            "x_max": 1,
            "samples": 3
        }))
        .unwrap();
        let y_min = halfway["y_min"].as_f64().unwrap();
        assert!((y_min - 0.886_226_925_45).abs() < 1e-6, "{y_min}");
        assert_eq!(halfway["y_max"], 1.0);
    }

    #[test]
    fn default_expression_sample_count_includes_both_ends() {
        let result = prepare(&json!({"expression": "x", "x_min": 0, "x_max": 20})).unwrap();
        assert_eq!(result["samples"].as_u64(), Some(DEFAULT_SAMPLES as u64));
        assert_eq!(result["y_min"], 0.0);
        assert_eq!(result["y_max"], 20.0);
    }

    #[test]
    fn largest_bar_series_prepares_without_drawing() {
        let points = (0..MAX_POINTS)
            .map(|_| json!({"label": "abcdefghijkl", "y": -987654.321}))
            .collect::<Vec<_>>();
        let result = prepare(&json!({
            "kind": "bar",
            "title": "a".repeat(MAX_TITLE_CHARS),
            "x_label": "b".repeat(MAX_AXIS_CHARS),
            "y_label": "c".repeat(MAX_AXIS_CHARS),
            "series": [{"name": "d".repeat(MAX_NAME_CHARS), "points": points}]
        }))
        .unwrap();
        assert_eq!(result["kind"], "bar");
        assert_eq!(result["series"][0]["points"], MAX_POINTS);
    }

    #[test]
    fn rejects_bad_plots_before_drawing() {
        let cases = [
            (json!({}), "Plot needs series or an expression"),
            (
                json!({"series": [], "expression": "x"}),
                "Pass series or an expression, not both",
            ),
            (
                json!({"kind": "pie", "series": [{"points": [{"y": 1}]}]}),
                "Plot kind must be line, bar, or scatter",
            ),
            (
                json!({"kind": 1, "series": [{"points": [{"y": 1}]}]}),
                "Plot kind must be line, bar, or scatter",
            ),
            (json!({"series": "no"}), "Plot series must be an array"),
            (json!({"series": []}), "Plot accepts 1 through 3 series"),
            (
                json!({"series": [{}, {}, {}, {}]}),
                "Plot accepts 1 through 3 series",
            ),
            (
                json!({"series": [{"points": []}]}),
                "Each series needs 1 through 30 points",
            ),
            (
                json!({"series": [{"points": [{"y": 1}], "extra": 1}]}),
                "Unknown plot series field",
            ),
            (json!({"series": [1]}), "Plot series must be an object"),
            (json!({"series": [{}]}), "Missing points"),
            (
                json!({"series": [{"points": {"y": 1}}]}),
                "Plot points must be an array",
            ),
            (
                json!({"series": [{"points": [1]}]}),
                "Plot point must be an object",
            ),
            (
                json!({"series": [{"points": [{"y": "1"}]}]}),
                "Each point needs a finite y",
            ),
            (
                json!({"series": [{"points": [{}]}]}),
                "Each point needs a finite y",
            ),
            (
                json!({"series": [{"points": [{"x": "1", "y": 1}]}]}),
                "Each point x must be a finite number",
            ),
            (
                json!({"series": [{"points": [{"y": 1, "z": 2}]}]}),
                "Unknown plot point field",
            ),
            (
                json!({"series": [{"points": [{"y": 1}, {"x": 1, "y": 2}]}]}),
                "Give every point an x, or leave x out",
            ),
            (
                json!({"series": [{"name": "  ", "points": [{"y": 1}]}]}),
                "Series name must be 1 through 30 characters",
            ),
            (
                json!({"series": [{"name": 1, "points": [{"y": 1}]}]}),
                "Series name must be 1 through 30 characters",
            ),
            (
                json!({"series": [{"points": [{"label": "", "y": 1}]}]}),
                "Point label must be 1 through 12 characters",
            ),
            (
                json!({"title": "", "series": [{"points": [{"y": 1}]}]}),
                "Plot title must be 1 through 60 characters",
            ),
            (
                json!({"title": "bad\n", "series": [{"points": [{"y": 1}]}]}),
                "Plot text cannot contain control characters or backticks",
            ),
            (
                json!({"y_label": "a`b", "series": [{"points": [{"y": 1}]}]}),
                "Plot text cannot contain control characters or backticks",
            ),
            (
                json!({"kind": "bar", "series": [{"points": [{"y": 1}]}, {"points": [{"y": 2}]}]}),
                "Bar charts take one series",
            ),
            (
                json!({"expression": "x", "kind": "bar", "x_min": 0, "x_max": 1}),
                "Bar charts plot series, not an expression",
            ),
            (
                json!({"expression": "x"}),
                "x_min and x_max are required to plot an expression",
            ),
            (
                json!({"expression": "x", "x_min": 1, "x_max": 1}),
                "x_min must be less than x_max",
            ),
            (
                json!({"expression": "x", "x_min": 2, "x_max": 1}),
                "x_min must be less than x_max",
            ),
            (
                json!({"expression": "x", "x_min": "0", "x_max": 1}),
                "x_min and x_max must be finite numbers",
            ),
            (
                json!({"expression": "   ", "x_min": 0, "x_max": 1}),
                "Plot expression must be 1 through 200 characters",
            ),
            (
                json!({"expression": 1, "x_min": 0, "x_max": 1}),
                "Plot expression must be 1 through 200 characters",
            ),
            (
                json!({"expression": "1 / x", "x_min": -1, "x_max": 1, "samples": 3}),
                "Cannot divide by zero",
            ),
            (
                json!({"expression": "x", "x_min": 0, "x_max": 1, "samples": 1}),
                "Plot samples must be 2 through 30",
            ),
            (
                json!({"expression": "x", "x_min": 0, "x_max": 1, "samples": 2.5}),
                "Plot samples must be 2 through 30",
            ),
            (
                json!({"series": [{"points": [{"y": 1}]}], "samples": 5}),
                "x_min, x_max, and samples apply only to an expression",
            ),
            (json!([]), "Plot arguments must be an object"),
            (
                json!({"series": [{"points": [{"y": 1}]}], "topic": "no"}),
                "Unknown plot argument",
            ),
        ];
        for (arguments, message) in cases {
            assert_eq!(prepare(&arguments).unwrap_err(), message, "{arguments}");
        }
        let too_many = (0..31).map(|index| json!({"y": index})).collect::<Vec<_>>();
        assert_eq!(
            prepare(&json!({"series": [{"points": too_many}]})).unwrap_err(),
            "Each series needs 1 through 30 points"
        );
        assert_eq!(
            prepare(&json!({
                "expression": "x * 1e308",
                "x_min": 0,
                "x_max": 10,
                "samples": 2
            }))
            .unwrap_err(),
            "Calculation result is too large"
        );
    }
}
