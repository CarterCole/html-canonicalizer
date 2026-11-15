//! Web fonts, the browser half: reading stylesheets and font files through DevTools.
//! The parsing and selection live in `canon_core::fonts`.

pub use canon_core::fonts::*;

use crate::cdp::Page;
use anyhow::Result;
use base64::Engine as _;
use serde_json::{json, Value};

/// Text and base URL of every stylesheet in the page, cross-origin ones included.
pub fn stylesheets(page: &Page, doc_url: &str) -> Result<Vec<(String, String)>> {
    page.take_events("CSS.styleSheetAdded");
    page.call("DOM.enable", json!({}))?;
    page.call("CSS.enable", json!({}))?;
    let headers = page.take_events("CSS.styleSheetAdded");
    let mut out = Vec::new();
    for h in headers {
        let hd = &h["header"];
        let Some(id) = hd["styleSheetId"].as_str() else { continue };
        let base = hd["sourceURL"].as_str().filter(|s| !s.is_empty()).unwrap_or(doc_url).to_string();
        if let Ok(r) = page.call("CSS.getStyleSheetText", json!({ "styleSheetId": id })) {
            if let Some(Value::String(text)) = r.get("text").cloned() {
                if text.to_ascii_lowercase().contains("@font-face") {
                    out.push((text, base));
                }
            }
        }
    }
    let _ = page.call("CSS.disable", json!({}));
    let _ = page.call("DOM.disable", json!({}));
    Ok(out)
}

/// Font bytes as base64: from disk for `file:` URLs, through the page for everything else
/// (the page could load the font, so it may fetch it).
pub fn fetch_b64(page: &Page, url: &str) -> Option<String> {
    if let Ok(u) = url::Url::parse(url) {
        if u.scheme() == "file" {
            let bytes = std::fs::read(u.to_file_path().ok()?).ok()?;
            return Some(base64::engine::general_purpose::STANDARD.encode(bytes));
        }
    }
    let v = page.eval(&format!("__canon.fetchB64({})", Value::String(url.to_string()))).ok()?;
    v.get("data")?.as_str().map(str::to_string)
}

