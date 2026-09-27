use chrono::Local;
use reqwest::{Client, StatusCode};

pub fn get_local_date_string() -> String {
    let now = Local::now();
    now.format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn country_code_to_emoji(country_code: &str) -> String {
    let code = country_code.to_ascii_uppercase();
    let bytes = code.as_bytes();
    if bytes.len() != 2 || !bytes.iter().all(u8::is_ascii_uppercase) {
        return String::new();
    }
    let first = char::from_u32(0x1F1E6 + u32::from(bytes[0] - b'A'));
    let second = char::from_u32(0x1F1E6 + u32::from(bytes[1] - b'A'));
    match (first, second) {
        (Some(a), Some(b)) => format!("{a}{b}"),
        _ => String::new(),
    }
}

pub(crate) async fn get_text(client: &Client, url: &str) -> Option<String> {
    client.get(url).send().await.ok()?.text().await.ok()
}

pub(crate) async fn get_trace_location(client: &Client, url: &str) -> Option<String> {
    get_text(client, url)
        .await?
        .lines()
        .find_map(|line| line.strip_prefix("loc="))
        .map(str::to_owned)
}

pub(crate) fn extract_quoted_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let (_, rest) = body.split_once(&format!(r#""{key}""#))?;
    let value = rest.split_once(':')?.1.trim_start();
    let value = value.strip_prefix('"')?;

    Some(value.split_once('"')?.0)
}

pub(crate) fn classify_restricted_status(status: StatusCode) -> Option<&'static str> {
    if matches!(
        status,
        StatusCode::FORBIDDEN | StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS
    ) {
        Some("No")
    } else if !status.is_success() {
        Some("Failed")
    } else {
        None
    }
}
