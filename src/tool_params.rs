//! Parsing and validation of the Tavily-aligned tool arguments.
//!
//! Each parser turns raw MCP `arguments` into typed values and rejects what
//! Tavily would reject with a 400, so a bad call fails locally with a clear
//! message and costs no request. Unknown keys are ignored, as elsewhere.

use serde_json::{Map, Value};

use crate::error::{GrokSearchError, Result};
use crate::model::search::{SearchHints, TimeRange};

/// `web_search` arguments beyond the original set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchExtensions {
    pub time_range: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub hints: SearchHints,
    pub tavily: Map<String, Value>,
}

fn invalid(message: String) -> GrokSearchError {
    GrokSearchError::InvalidParams(message)
}

fn opt_bool(args: &Value, tool: &str, key: &str) -> Result<Option<bool>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(invalid(format!("{tool}.{key} must be a boolean"))),
    }
}

fn opt_str(args: &Value, tool: &str, key: &str) -> Result<Option<String>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => {
            Ok(Some(value.trim().to_string()))
        }
        Some(_) => Err(invalid(format!("{tool}.{key} must be a non-empty string"))),
    }
}

fn opt_enum(args: &Value, tool: &str, key: &str, allowed: &[&str]) -> Result<Option<String>> {
    let Some(value) = opt_str(args, tool, key)? else {
        return Ok(None);
    };
    let lower = value.to_ascii_lowercase();
    if allowed.contains(&lower.as_str()) {
        Ok(Some(lower))
    } else {
        Err(invalid(format!(
            "{tool}.{key} must be one of {}, got \"{value}\"",
            allowed.join(", ")
        )))
    }
}

pub(crate) fn opt_int(
    args: &Value,
    tool: &str,
    key: &str,
    min: u64,
    max: u64,
) -> Result<Option<u64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_u64() {
            Some(n) if (min..=max).contains(&n) => Ok(Some(n)),
            _ => Err(invalid(format!(
                "{tool}.{key} must be an integer from {min} to {max}"
            ))),
        },
    }
}

/// A `bool` or one of `strings` (Tavily's `include_answer` / `include_raw_content`).
fn opt_bool_or_enum(
    args: &Value,
    tool: &str,
    key: &str,
    strings: &[&str],
) -> Result<Option<Value>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(Value::Bool(*value))),
        Some(Value::String(text)) if strings.contains(&text.to_ascii_lowercase().as_str()) => {
            Ok(Some(Value::String(text.to_ascii_lowercase())))
        }
        Some(_) => Err(invalid(format!(
            "{tool}.{key} must be a boolean or one of {}",
            strings.join(", ")
        ))),
    }
}

fn opt_date(args: &Value, tool: &str, key: &str) -> Result<Option<String>> {
    let Some(value) = opt_str(args, tool, key)? else {
        return Ok(None);
    };
    if crate::clock::day_from_ymd(&value).is_none() {
        return Err(invalid(format!(
            "{tool}.{key} must be a calendar date YYYY-MM-DD, got \"{value}\""
        )));
    }
    Ok(Some(value))
}

const TAVILY_BOOL_KEYS: &[&str] = &[
    "include_images",
    "include_image_descriptions",
    "include_favicon",
    "include_published_date",
    "filter_by_published_date",
    "filter_by_language",
    "auto_parameters",
    "include_usage",
];

/// Parse the `web_search` extensions. `recency_set` is whether the caller
/// also passed `recency_days`; `include_domains_set` whether it passed
/// `include_domains` (Tavily's domain mode needs it).
pub fn parse_search_extensions(
    args: &Value,
    recency_set: bool,
    include_domains_set: bool,
) -> Result<SearchExtensions> {
    let tool = "web_search";
    let time_range = match opt_str(args, tool, "time_range")? {
        None => None,
        Some(value) => Some(
            TimeRange::parse(&value)
                .ok_or_else(|| {
                    invalid(format!(
                        "web_search.time_range must be day, week, month or year, got \"{value}\""
                    ))
                })?
                .as_str()
                .to_string(),
        ),
    };
    let start_date = opt_date(args, tool, "start_date")?;
    let end_date = opt_date(args, tool, "end_date")?;
    if let (Some(start), Some(end)) = (&start_date, &end_date) {
        if start > end {
            return Err(invalid(format!(
                "web_search.start_date ({start}) is after end_date ({end})"
            )));
        }
    }
    let windows = [
        recency_set,
        time_range.is_some(),
        start_date.is_some() || end_date.is_some(),
    ]
    .into_iter()
    .filter(|set| *set)
    .count();
    if windows > 1 {
        return Err(invalid(
            "web_search takes one time window: recency_days, time_range, or start_date/end_date"
                .to_string(),
        ));
    }

    let country = match opt_str(args, tool, "country")? {
        None => None,
        Some(value) if value.len() == 2 && value.chars().all(|c| c.is_ascii_alphabetic()) => {
            Some(value.to_ascii_uppercase())
        }
        Some(value) => match crate::providers::country::tavily_country(&value) {
            Some(name) => Some(name.to_string()),
            None => {
                return Err(invalid(format!(
                    "web_search.country must be an ISO 3166-1 alpha-2 code or an English country name, got \"{value}\""
                )))
            }
        },
    };
    let hints = SearchHints {
        country,
        language: opt_str(args, tool, "language")?.map(|value| value.to_ascii_lowercase()),
        safe_search: opt_bool(args, tool, "safe_search")?,
        exact_match: opt_bool(args, tool, "exact_match")?,
    };

    let mut tavily = Map::new();
    if let Some(topic) = opt_enum(args, tool, "topic", &["general", "news", "finance"])? {
        tavily.insert("topic".into(), Value::String(topic));
    }
    let depth = opt_enum(
        args,
        tool,
        "search_depth",
        &["basic", "advanced", "fast", "ultra-fast"],
    )?;
    if let Some(depth) = &depth {
        tavily.insert("search_depth".into(), Value::String(depth.clone()));
    }
    if let Some(chunks) = opt_int(args, tool, "chunks_per_source", 1, 3)? {
        tavily.insert("chunks_per_source".into(), Value::from(chunks));
    }
    if let Some(answer) = opt_bool_or_enum(args, tool, "include_answer", &["basic", "advanced"])? {
        tavily.insert("include_answer".into(), answer);
    }
    if let Some(raw) = opt_bool_or_enum(args, tool, "include_raw_content", &["markdown", "text"])? {
        tavily.insert("include_raw_content".into(), raw);
    }
    if let Some(mode) = opt_enum(args, tool, "include_domains_mode", &["restrict", "prefer"])? {
        if !include_domains_set {
            return Err(invalid(
                "web_search.include_domains_mode needs include_domains".to_string(),
            ));
        }
        tavily.insert("include_domains_mode".into(), Value::String(mode));
    }
    for key in TAVILY_BOOL_KEYS {
        if let Some(value) = opt_bool(args, tool, key)? {
            tavily.insert((*key).to_string(), Value::Bool(value));
        }
    }
    if tavily.get("filter_by_language") == Some(&Value::Bool(true)) && hints.language.is_none() {
        return Err(invalid(
            "web_search.filter_by_language needs language".to_string(),
        ));
    }
    if hints.safe_search.is_some() && matches!(depth.as_deref(), Some("fast" | "ultra-fast")) {
        return Err(invalid(
            "web_search.safe_search is not supported with search_depth fast or ultra-fast"
                .to_string(),
        ));
    }

    Ok(SearchExtensions {
        time_range,
        start_date,
        end_date,
        hints,
        tavily,
    })
}

/// `web_search` research fields (`mode=research`), returned under Tavily
/// research names, plus the `research_id` to resume. Only read when the mode
/// is research; elsewhere these keys are ignored like any unknown key.
pub fn parse_research_params(args: &Value) -> Result<(Map<String, Value>, Option<String>)> {
    let tool = "web_search";
    let mut out = Map::new();
    if let Some(model) = opt_enum(args, tool, "research_model", &["mini", "pro", "auto"])? {
        out.insert("model".into(), Value::String(model));
    }
    match args.get("output_schema") {
        None | Some(Value::Null) => {}
        Some(Value::Object(schema)) if schema.get("properties").is_some_and(Value::is_object) => {
            out.insert("output_schema".into(), Value::Object(schema.clone()));
        }
        Some(_) => {
            return Err(invalid(
                "web_search.output_schema must be a JSON Schema object with a `properties` object"
                    .to_string(),
            ))
        }
    }
    if let Some(format) = opt_enum(
        args,
        tool,
        "citation_format",
        &["numbered", "mla", "apa", "chicago"],
    )? {
        out.insert("citation_format".into(), Value::String(format));
    }
    if let Some(length) = opt_enum(args, tool, "output_length", &["short", "standard", "long"])? {
        out.insert("output_length".into(), Value::String(length));
    }
    let research_id = opt_str(args, tool, "research_id")?;
    Ok((out, research_id))
}

/// Tavily extract fields for `web_fetch` beyond `depth` / `format`.
pub fn parse_extract_extras(args: &Value) -> Result<Map<String, Value>> {
    let tool = "web_fetch";
    let mut out = Map::new();
    let query = opt_str(args, tool, "query")?;
    if let Some(query) = &query {
        out.insert("query".into(), Value::String(query.clone()));
    }
    if let Some(chunks) = opt_int(args, tool, "chunks_per_source", 1, 5)? {
        if query.is_none() {
            return Err(invalid(
                "web_fetch.chunks_per_source needs query".to_string(),
            ));
        }
        out.insert("chunks_per_source".into(), Value::from(chunks));
    }
    for key in ["include_images", "include_favicon", "include_usage"] {
        if let Some(value) = opt_bool(args, tool, key)? {
            out.insert(key.to_string(), Value::Bool(value));
        }
    }
    if let Some(timeout) = opt_seconds(args, tool, "timeout", 1.0, 60.0)? {
        out.insert("timeout".into(), Value::from(timeout));
    }
    Ok(out)
}

fn opt_string_array(args: &Value, tool: &str, key: &str) -> Result<Option<Vec<String>>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str().map(str::trim) {
                    Some(text) if !text.is_empty() => out.push(text.to_string()),
                    _ => return Err(invalid(format!("{tool}.{key} must be an array of strings"))),
                }
            }
            Ok((!out.is_empty()).then_some(out))
        }
        Some(_) => Err(invalid(format!("{tool}.{key} must be an array of strings"))),
    }
}

/// Native Tavily `/map` fields, shared by `web_map` and `web_crawl`. For
/// `web_crawl`, `crawl` adds the extraction fields. `web_map` keeps its
/// legacy `max_results` as an alias of `limit`.
pub fn parse_site_params(args: &Value, tool: &str, crawl: bool) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    let instructions = opt_str(args, tool, "instructions")?;
    if let Some(text) = &instructions {
        out.insert("instructions".into(), Value::String(text.clone()));
    }
    if let Some(depth) = opt_int(args, tool, "max_depth", 1, 5)? {
        out.insert("max_depth".into(), Value::from(depth));
    }
    if let Some(breadth) = opt_int(args, tool, "max_breadth", 1, 500)? {
        out.insert("max_breadth".into(), Value::from(breadth));
    }
    let limit = opt_int(args, tool, "limit", 1, u64::MAX)?;
    let alias = if crawl {
        None
    } else {
        opt_int(args, tool, "max_results", 1, u64::MAX)?
    };
    match (limit, alias) {
        (Some(a), Some(b)) if a != b => {
            return Err(invalid(format!(
                "{tool}.limit and {tool}.max_results disagree; pass one"
            )))
        }
        (Some(n), _) | (None, Some(n)) => {
            out.insert("limit".into(), Value::from(n));
        }
        (None, None) => {}
    }
    for key in [
        "select_paths",
        "select_domains",
        "exclude_paths",
        "exclude_domains",
    ] {
        if let Some(items) = opt_string_array(args, tool, key)? {
            out.insert(key.to_string(), Value::from(items));
        }
    }
    for key in ["allow_external", "include_usage"] {
        if let Some(value) = opt_bool(args, tool, key)? {
            out.insert(key.to_string(), Value::Bool(value));
        }
    }
    if let Some(timeout) = opt_seconds(args, tool, "timeout", 10.0, 150.0)? {
        out.insert("timeout".into(), Value::from(timeout));
    }
    if crawl {
        if let Some(chunks) = opt_int(args, tool, "chunks_per_source", 1, 5)? {
            if instructions.is_none() {
                return Err(invalid(format!(
                    "{tool}.chunks_per_source needs instructions"
                )));
            }
            out.insert("chunks_per_source".into(), Value::from(chunks));
        }
        for key in ["include_images", "include_favicon"] {
            if let Some(value) = opt_bool(args, tool, key)? {
                out.insert(key.to_string(), Value::Bool(value));
            }
        }
        if let Some(depth) = opt_enum(args, tool, "extract_depth", &["basic", "advanced"])? {
            out.insert("extract_depth".into(), Value::String(depth));
        }
        if let Some(format) = opt_enum(args, tool, "format", &["markdown", "text"])? {
            out.insert("format".into(), Value::String(format));
        }
    }
    Ok(out)
}

/// A number of seconds within `min..=max`.
pub(crate) fn opt_seconds(
    args: &Value,
    tool: &str,
    key: &str,
    min: f64,
    max: f64,
) -> Result<Option<f64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_f64() {
            Some(n) if (min..=max).contains(&n) => Ok(Some(n)),
            _ => Err(invalid(format!(
                "{tool}.{key} must be a number of seconds from {min} to {max}"
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn research_params_validate() {
        let (params, id) = parse_research_params(&json!({
            "research_model": "Pro", "citation_format": "apa", "output_length": "short",
            "output_schema": { "properties": { "company": { "type": "string" } } },
            "research_id": "abc"
        }))
        .unwrap();
        assert_eq!(params["model"], "pro");
        assert_eq!(params["citation_format"], "apa");
        assert!(params.contains_key("output_schema"));
        assert_eq!(id.as_deref(), Some("abc"));
        assert!(parse_research_params(&json!({ "output_schema": { "type": "object" } })).is_err());
        assert!(parse_research_params(&json!({ "research_model": "max" })).is_err());
    }

    #[test]
    fn extract_extras_validate() {
        assert!(parse_extract_extras(&json!({})).unwrap().is_empty());
        let extras = parse_extract_extras(&json!({
            "query": "pricing", "chunks_per_source": 2, "include_images": true, "timeout": 20
        }))
        .unwrap();
        assert_eq!(
            Value::Object(extras),
            json!({ "query": "pricing", "chunks_per_source": 2, "include_images": true, "timeout": 20.0 })
        );
        assert!(parse_extract_extras(&json!({ "chunks_per_source": 2 })).is_err());
        assert!(parse_extract_extras(&json!({ "timeout": 61 })).is_err());
    }

    #[test]
    fn site_params_validate_and_alias_max_results() {
        let map = parse_site_params(
            &json!({
                "instructions": "API reference", "max_depth": 2, "max_results": 30,
                "select_paths": ["/docs/.*"], "allow_external": false, "timeout": 60
            }),
            "web_map",
            false,
        )
        .unwrap();
        assert_eq!(
            Value::Object(map),
            json!({
                "instructions": "API reference", "max_depth": 2, "limit": 30,
                "select_paths": ["/docs/.*"], "allow_external": false, "timeout": 60.0
            })
        );
        assert!(
            parse_site_params(&json!({ "limit": 5, "max_results": 6 }), "web_map", false).is_err()
        );
        assert!(parse_site_params(&json!({ "max_depth": 6 }), "web_map", false).is_err());
        assert!(parse_site_params(&json!({ "timeout": 5 }), "web_map", false).is_err());
        assert!(parse_site_params(&json!({ "select_paths": [1] }), "web_map", false).is_err());
        // Extraction fields only exist on crawl.
        let crawl = parse_site_params(
            &json!({ "extract_depth": "advanced", "format": "text", "include_images": true }),
            "web_crawl",
            true,
        )
        .unwrap();
        assert_eq!(crawl["extract_depth"], "advanced");
        assert!(parse_site_params(&json!({ "chunks_per_source": 2 }), "web_crawl", true).is_err());
        assert!(
            parse_site_params(&json!({ "extract_depth": "advanced" }), "web_map", false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn empty_args_parse_to_defaults() {
        assert_eq!(
            parse_search_extensions(&json!({}), false, false).unwrap(),
            SearchExtensions::default()
        );
    }

    #[test]
    fn unified_and_tavily_fields_are_typed() {
        let ext = parse_search_extensions(
            &json!({
                "time_range": "W", "country": "tw", "language": "ZH-TW",
                "safe_search": true, "exact_match": true,
                "topic": "News", "search_depth": "fast", "include_answer": "advanced",
                "include_raw_content": true, "include_usage": true, "chunks_per_source": 2
            }),
            false,
            false,
        );
        // safe_search + fast depth is rejected; drop safe_search and retry.
        assert!(ext.is_err());
        let ext = parse_search_extensions(
            &json!({
                "time_range": "W", "country": "tw", "language": "ZH-TW", "exact_match": true,
                "topic": "News", "search_depth": "fast", "include_answer": "advanced",
                "include_raw_content": true, "include_usage": true, "chunks_per_source": 2
            }),
            false,
            false,
        )
        .unwrap();
        assert_eq!(ext.time_range.as_deref(), Some("week"));
        assert_eq!(ext.hints.country.as_deref(), Some("TW"));
        assert_eq!(ext.hints.language.as_deref(), Some("zh-tw"));
        assert_eq!(ext.hints.exact_match, Some(true));
        assert_eq!(
            Value::Object(ext.tavily),
            json!({
                "topic": "news", "search_depth": "fast", "include_answer": "advanced",
                "include_raw_content": true, "include_usage": true, "chunks_per_source": 2
            })
        );
    }

    #[test]
    fn rejects_conflicting_windows_and_bad_values() {
        let cases = [
            (json!({ "time_range": "week" }), true, false),
            (
                json!({ "time_range": "week", "start_date": "2026-01-01" }),
                false,
                false,
            ),
            (json!({ "start_date": "2026-02-30" }), false, false),
            (
                json!({ "start_date": "2026-03-01", "end_date": "2026-02-01" }),
                false,
                false,
            ),
            (json!({ "time_range": "fortnight" }), false, false),
            (json!({ "country": "atlantis" }), false, false),
            (json!({ "topic": "sports" }), false, false),
            (json!({ "chunks_per_source": 4 }), false, false),
            (json!({ "include_domains_mode": "prefer" }), false, false),
            (json!({ "filter_by_language": true }), false, false),
            (json!({ "include_answer": "long" }), false, false),
        ];
        for (args, recency, domains) in cases {
            assert!(
                parse_search_extensions(&args, recency, domains).is_err(),
                "expected error for {args}"
            );
        }
        assert!(
            parse_search_extensions(&json!({ "include_domains_mode": "prefer" }), false, true)
                .is_ok()
        );
        assert!(
            parse_search_extensions(&json!({ "country": "Taiwan" }), false, false)
                .unwrap()
                .hints
                .country
                .as_deref()
                == Some("taiwan")
        );
    }
}
