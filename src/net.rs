//! Networking for widgets that need it (currently the weather widget).
//!
//! Requests run on a worker thread through [`gio::spawn_blocking`] and the
//! result is delivered back on the main context, so the UI never blocks and no
//! extra runtime is needed.
//!
//! Nothing here runs unless the user places a widget that asks for it.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use gtk::{gio, glib};
use log::{debug, warn};
use serde_json::Value;

/// How long a single request may take before it is considered failed.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Fetches and parses `url` as JSON, then calls `on_done` on the main thread.
///
/// Errors are reported as short, human readable strings: widgets show them
/// instead of pretending to have data.
pub fn fetch_json(url: String, on_done: impl FnOnce(Result<Value, String>) + 'static) {
    glib::MainContext::default().spawn_local(async move {
        // A blocking worker keeps a slow network away from the UI thread; the
        // join handle reports a panic in the worker as an error too.
        let outcome = gio::spawn_blocking(move || get_json(&url)).await;
        let result = match outcome {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => Err(format!("{e:#}")),
            Err(_) => Err("取得処理が異常終了しました".to_owned()),
        };
        on_done(result);
    });
}

fn get_json(url: &str) -> Result<Value, anyhow::Error> {
    debug!("GET {url}");
    let response = minreq::get(url)
        .with_timeout(TIMEOUT.as_secs())
        .send()
        .context("接続できませんでした")?;

    if response.status_code != 200 {
        bail!("サーバーが {} を返しました", response.status_code);
    }

    let body = response.as_str().context("応答を読めませんでした")?;
    serde_json::from_str(body).context("応答を解釈できませんでした")
}

/// Percent-encodes a query parameter value.
///
/// Only the unreserved characters survive; everything else becomes `%XX`, which
/// is what Open-Meteo (and every other HTTP API in practice) expects.
pub fn encode_query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(*byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Logs a failed request once, without spamming every refresh.
pub fn report(url: &str, error: &str) {
    warn!("{url} を取得できません: {error}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_encoding_matches_rfc3986() {
        assert_eq!(encode_query("Tokyo"), "Tokyo");
        assert_eq!(encode_query("New York"), "New%20York");
        assert_eq!(encode_query("東京"), "%E6%9D%B1%E4%BA%AC");
        assert_eq!(encode_query("a/b&c=d"), "a%2Fb%26c%3Dd");
    }
}
