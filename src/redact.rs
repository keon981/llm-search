//! Secret redaction at the MCP boundary.
//!
//! Every tool result and error message passes through [`redact_value`] /
//! [`redact_text`] before it leaves the process, as in the DeepSeek plugin:
//! configured key values are masked verbatim, and common credential shapes
//! (`Bearer …`, `X-Subscription-Token: …`, `*_api_key=…`) are masked even when
//! the value is not one of ours — an upstream error body can echo a header.

use serde_json::Value;

const MASK: &str = "[redacted]";

/// Bearer tokens shorter than this are left alone, so prose such as "the
/// bearer of the token" is not mangled.
const MIN_BEARER_TOKEN: usize = 12;

/// Mask `secrets` (exact values, at least 8 characters) and credential-shaped
/// substrings in `text`.
pub fn redact_text(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if secret.chars().count() >= 8 && out.contains(secret.as_str()) {
            out = out.replace(secret.as_str(), MASK);
        }
    }
    out = mask_after(&out, "bearer ", false, MIN_BEARER_TOKEN);
    out = mask_after(&out, "x-subscription-token", true, 1);
    for keyword in ["api_key", "api-key", "apikey", "access_token"] {
        out = mask_after(&out, keyword, true, 1);
    }
    out
}

/// [`redact_text`] applied to every string inside a JSON value.
pub fn redact_value(value: Value, secrets: &[String]) -> Value {
    match value {
        Value::String(text) => Value::String(redact_text(&text, secrets)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| redact_value(item, secrets))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (key, redact_value(item, secrets)))
                .collect(),
        ),
        other => other,
    }
}

fn is_terminator(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b',' | b';' | b'"' | b'\'' | b'}' | b']' | b')' | b'&' | b'<' | b'>'
        )
}

/// After each case-insensitive `keyword`, replace the value token with the
/// mask. With `needs_separator`, a `:` or `=` must follow the keyword (closing
/// quotes and spaces allowed in between, as in `"tavily_api_key": "…"`).
/// Tokens shorter than `min_len` are kept.
fn mask_after(text: &str, keyword: &str, needs_separator: bool, min_len: usize) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut search_from = 0;
    while let Some(found) = lower[search_from..].find(keyword) {
        let keyword_end = search_from + found + keyword.len();
        let mut i = keyword_end;
        if needs_separator {
            while i < bytes.len() && matches!(bytes[i], b'"' | b'\'' | b' ' | b'\t') {
                i += 1;
            }
            if i >= bytes.len() || !matches!(bytes[i], b':' | b'=') {
                search_from = keyword_end;
                continue;
            }
            i += 1;
        }
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'"' | b'\'') {
            i += 1;
        }
        let token_start = i;
        while i < bytes.len() && !is_terminator(bytes[i]) {
            i += 1;
        }
        let token = &text[token_start..i];
        if token.chars().count() >= min_len && token != MASK {
            out.push_str(&text[cursor..token_start]);
            out.push_str(MASK);
            cursor = i;
        }
        search_from = i.max(keyword_end);
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn masks_configured_secret_values() {
        let secrets = vec!["tvly-secret-value-123".to_string()];
        let out = redact_text("Tavily rejected key tvly-secret-value-123 (401)", &secrets);
        assert_eq!(out, "Tavily rejected key [redacted] (401)");
    }

    #[test]
    fn masks_credential_shapes() {
        let out = redact_text(
            "Authorization: Bearer abcdefghijklmnop; X-Subscription-Token: BSA123456 TAVILY_API_KEY=tvly-x api_key=zzz&q=1",
            &[],
        );
        assert_eq!(
            out,
            "Authorization: Bearer [redacted]; X-Subscription-Token: [redacted] TAVILY_API_KEY=[redacted] api_key=[redacted]&q=1"
        );
    }

    #[test]
    fn masks_json_key_value_pairs() {
        let out = redact_text(r#"{"brave_api_key": "BSA-abc", "query": "rust"}"#, &[]);
        assert_eq!(out, r#"{"brave_api_key": "[redacted]", "query": "rust"}"#);
    }

    #[test]
    fn leaves_prose_and_short_bearer_words_alone() {
        let text = "The bearer of this token is a user. Use api keys wisely.";
        assert_eq!(redact_text(text, &[]), text);
    }

    #[test]
    fn redacts_inside_nested_json() {
        let secrets = vec!["exa-0123456789".to_string()];
        let value = json!({ "a": ["ok", "key exa-0123456789"], "b": { "c": 1 } });
        let out = redact_value(value, &secrets);
        assert_eq!(
            out,
            json!({ "a": ["ok", "key [redacted]"], "b": { "c": 1 } })
        );
    }

    #[test]
    fn handles_multibyte_text_around_tokens() {
        let out = redact_text("錯誤：api_key=秘密值123 結束", &[]);
        assert_eq!(out, "錯誤：api_key=[redacted] 結束");
    }
}
