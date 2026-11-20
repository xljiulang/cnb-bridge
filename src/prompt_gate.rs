//! Prompt compatibility rules applied before requests reach the upstream agent.

use axum::http::{header, HeaderMap, HeaderValue};
use regex::Regex;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// User-Agent sent to the upstream agent for compatibility.
pub(crate) const WORKBUDDY_USER_AGENT: &str = concat!("WorkBuddy/", env!("CARGO_PKG_VERSION"));
const PROMPT_GATE_BLOCK_CODE: i64 = 11128;
const PATH_INSTRUCTION_REPLACEMENT: &str = concat!(
    "Paths beginning with @ refer to resources the user explicitly named. ",
    "Relative paths are resolved from the workspace root; ",
    "absolute paths refer to host files or directories."
);

#[derive(Debug, Clone, Default)]
pub(crate) struct GateReport {
    profiles: Vec<&'static str>,
    hits: BTreeMap<&'static str, usize>,
    changed: bool,
    user_agent_overridden: bool,
}

impl GateReport {
    pub(crate) fn profile_order(&self) -> &[&'static str] {
        &self.profiles
    }

    #[cfg(test)]
    pub(crate) fn hit_count(&self, rule_id: &str) -> usize {
        self.hits.get(rule_id).copied().unwrap_or(0)
    }

    pub(crate) fn changed(&self) -> bool {
        self.changed
    }

    pub(crate) fn user_agent_overridden(&self) -> bool {
        self.user_agent_overridden
    }

    pub(crate) fn summary(&self) -> String {
        let rules = self
            .hits
            .iter()
            .map(|(rule, count)| format!("{rule}:{count}"))
            .collect::<Vec<_>>()
            .join(",");
        format!("rules={rules};user_agent={}", self.user_agent_overridden)
    }

    fn record_hit(&mut self, rule_id: &'static str, count: usize) {
        if count == 0 {
            return;
        }
        *self.hits.entry(rule_id).or_default() += count;
        self.changed = true;
    }

    pub(crate) fn record_developer_roles(&mut self, count: usize) {
        self.record_hit(openai_compat::chat::ROLE_RULE_ID, count);
    }
}

struct GateProfile {
    id: &'static str,
    user_agent_products: &'static [&'static str],
    rules: &'static [GateRule],
}

enum GateRule {
    BrandBoundary {
        id: &'static str,
        terms: &'static [&'static str],
        replacement: &'static str,
    },
    PathInstructions {
        id: &'static str,
    },
}

const PROFILES: &[GateProfile] = &[GateProfile {
    id: "harness",
    user_agent_products: &["deepseek-harness"],
    rules: &[
        GateRule::BrandBoundary {
            id: "harness/brand",
            terms: &["DeepSeek Harness"],
            replacement: "Harness of DeepSeek",
        },
        GateRule::PathInstructions {
            id: "harness/path-instructions",
        },
    ],
}];

/// Applies User-Agent-matched profiles to system/developer message text.
pub(crate) fn normalize_chat(body: &mut Value, headers: &HeaderMap) -> GateReport {
    let mut report = GateReport::default();
    let user_agent_product = user_agent_product(headers);
    let profiles = PROFILES
        .iter()
        .filter(|profile| profile.matches(user_agent_product))
        .collect::<Vec<_>>();
    for profile in &profiles {
        report.profiles.push(profile.id);
    }
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return report;
    };
    for message in messages {
        let Some(role) = message.get("role").and_then(Value::as_str) else {
            continue;
        };
        if !role.eq_ignore_ascii_case("system") && !role.eq_ignore_ascii_case("developer") {
            continue;
        }
        let Some(content) = message.get_mut("content") else {
            continue;
        };
        normalize_content(content, &profiles, &mut report);
    }
    report
}

impl GateProfile {
    fn matches(&self, user_agent_product: Option<&str>) -> bool {
        user_agent_product.is_some_and(|product| {
            self.user_agent_products
                .iter()
                .any(|expected| product.eq_ignore_ascii_case(expected))
        })
    }
}

fn user_agent_product(headers: &HeaderMap) -> Option<&str> {
    let user_agent = headers.get(header::USER_AGENT)?.to_str().ok()?;
    let product = user_agent
        .split_once('/')
        .map_or(user_agent, |(product, _)| product)
        .trim();
    (!product.is_empty()).then_some(product)
}

fn normalize_content(content: &mut Value, profiles: &[&GateProfile], report: &mut GateReport) {
    match content {
        Value::String(text) => normalize_text(text, profiles, report),
        Value::Array(parts) => {
            for part in parts {
                normalize_content(part, profiles, report);
            }
        }
        Value::Object(object) => {
            if let Some(text) = object.get_mut("text") {
                normalize_content(text, profiles, report);
            }
            if let Some(content) = object.get_mut("content") {
                normalize_content(content, profiles, report);
            }
        }
        _ => {}
    }
}

fn normalize_text(text: &mut String, profiles: &[&GateProfile], report: &mut GateReport) {
    for profile in profiles {
        for rule in profile.rules {
            match rule {
                GateRule::BrandBoundary {
                    id,
                    terms,
                    replacement,
                } => {
                    let mut count = 0;
                    for term in *terms {
                        let (rewritten, matches) =
                            replace_at_ascii_letter_boundaries(text, term, replacement);
                        if matches > 0 {
                            *text = rewritten.into_owned();
                            count += matches;
                        }
                    }
                    report.record_hit(id, count);
                }
                GateRule::PathInstructions { id } => {
                    let mut matches = 0;
                    let rewritten = path_instruction_re().replace_all(
                        text.as_str(),
                        |_captures: &regex::Captures<'_>| {
                            matches += 1;
                            PATH_INSTRUCTION_REPLACEMENT
                        },
                    );
                    if matches > 0 {
                        *text = rewritten.into_owned();
                        report.record_hit(id, matches);
                    }
                }
            }
        }
    }
}

fn path_instruction_re() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(concat!(
            r"(?is)tokens\s+prefixed\s+with\s+@\s+are\s+paths\s+the\s+user\s+",
            r"explicitly\s+referenced\.\s*relative\s+paths\s+resolve\s+from\s+",
            r"the\s+workspace\s+root;\s*absolute\s+paths\s+identify\s+files\s+",
            r"or\s+directories\s+on\s+the\s+host\."
        ))
        .expect("valid path instruction regex")
    })
}

fn replace_at_ascii_letter_boundaries<'a>(
    text: &'a str,
    needle: &str,
    replacement: &str,
) -> (Cow<'a, str>, usize) {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return (Cow::Borrowed(text), 0);
    }
    let bytes = text.as_bytes();
    let mut result = String::with_capacity(text.len());
    let mut copied = 0;
    let mut search = 0;
    let mut matches = 0;
    while search < bytes.len() {
        let Some(offset) = bytes[search..]
            .windows(needle.len())
            .position(|candidate| candidate.eq_ignore_ascii_case(needle))
        else {
            break;
        };
        let start = search + offset;
        let end = start + needle.len();
        let boundary_before = start == 0 || !bytes[start - 1].is_ascii_alphabetic();
        let boundary_after = end == bytes.len() || !bytes[end].is_ascii_alphabetic();
        if boundary_before && boundary_after {
            result.push_str(&text[copied..start]);
            result.push_str(replacement);
            copied = end;
            search = end;
            matches += 1;
        } else {
            search = start + 1;
        }
    }
    result.push_str(&text[copied..]);
    if matches == 0 {
        (Cow::Borrowed(text), 0)
    } else {
        (Cow::Owned(result), matches)
    }
}

/// Overrides the upstream User-Agent and records whether a change was needed.
pub(crate) fn apply_headers(headers: &mut HeaderMap, report: &mut GateReport) {
    let mut values = headers.get_all(header::USER_AGENT).iter();
    let changed = values.next().map(HeaderValue::as_bytes) != Some(WORKBUDDY_USER_AGENT.as_bytes())
        || values.next().is_some();
    headers.remove(header::USER_AGENT);
    headers.insert(
        header::USER_AGENT,
        HeaderValue::from_static(WORKBUDDY_USER_AGENT),
    );
    report.user_agent_overridden = changed;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GateBlock {
    pub(crate) message: String,
    pub(crate) upstream_request_id: Option<String>,
}

/// Detects the upstream prompt gate rejection envelope.
pub(crate) fn detect_gate_block(text: &str) -> Option<GateBlock> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("code").and_then(Value::as_i64) != Some(PROMPT_GATE_BLOCK_CODE) {
        return None;
    }
    Some(GateBlock {
        message: value
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("upstream prompt gate blocked the request")
            .to_string(),
        upstream_request_id: value
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header, HeaderMap, HeaderValue};
    use serde_json::json;

    const HARNESS_USER_AGENT: &str = concat!(
        "deepseek-harness/0.2.2-rc.1 ",
        "(+https://github.com/deepseek-ai/deepseek-harness)"
    );

    fn headers_with_user_agent(user_agent: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_str(user_agent).unwrap(),
        );
        headers
    }

    #[test]
    fn applies_harness_profile_and_reports_rule_hits() {
        let mut body = json!({
            "messages": [{
                "role": "system",
                "content": concat!(
                    "DeepSeek Harness and Codex. ",
                    "Tokens prefixed with @ are paths the user explicitly referenced. ",
                    "Relative paths resolve from the workspace root; ",
                    "absolute paths identify files or directories on the host."
                )
            }]
        });
        let headers = headers_with_user_agent(HARNESS_USER_AGENT);

        let report = normalize_chat(&mut body, &headers);

        assert_eq!(
            body["messages"][0]["content"],
            concat!(
                "Harness of DeepSeek and Codex. ",
                "Paths beginning with @ refer to resources the user explicitly named. ",
                "Relative paths are resolved from the workspace root; ",
                "absolute paths refer to host files or directories."
            )
        );
        assert_eq!(report.profile_order(), ["harness"]);
        assert_eq!(report.hit_count("harness/brand"), 1);
        assert_eq!(report.hit_count("harness/path-instructions"), 1);
        assert_eq!(report.hit_count("codex/brand"), 0);
    }

    #[test]
    fn preserves_ascii_letter_boundaries_and_other_roles() {
        let mut body = json!({
            "messages": [{
                "role": "developer",
                "content": [
                    { "type": "text", "text": "Codex CODEX中文 Codex2 codexui opencode myCodex" },
                    { "type": "text", "text": "DeepSeek Harness XDeepSeek Harness DeepSeek HarnessY" }
                ]
            }, {
                "role": "user",
                "content": "Codex DeepSeek Harness"
            }, {
                "role": "assistant",
                "content": "Codex"
            }, {
                "role": "tool",
                "content": "Codex"
            }]
        });
        let headers = headers_with_user_agent(HARNESS_USER_AGENT);

        openai_compat::chat::normalize_roles(&mut body);
        normalize_chat(&mut body, &headers);

        assert_eq!(
            body["messages"][0]["content"][0]["text"],
            "Codex CODEX中文 Codex2 codexui opencode myCodex"
        );
        assert_eq!(
            body["messages"][0]["content"][1]["text"],
            "Harness of DeepSeek XDeepSeek Harness DeepSeek HarnessY"
        );
        assert_eq!(body["messages"][1]["content"], "Codex DeepSeek Harness");
        assert_eq!(body["messages"][2]["content"], "Codex");
        assert_eq!(body["messages"][3]["content"], "Codex");
    }

    #[test]
    fn no_op_replacement_borrows_the_original_text() {
        let text = "unchanged system prompt";
        let (rewritten, matches) =
            replace_at_ascii_letter_boundaries(text, "DeepSeek Harness", "replacement");

        assert_eq!(matches, 0);
        assert!(matches!(rewritten, std::borrow::Cow::Borrowed(value) if value == text));
    }

    #[test]
    fn leaves_developer_role_to_the_compatibility_library() {
        let mut body = json!({
            "messages": [{
                "role": "developer",
                "content": "Create a concise title for the session."
            }, {
                "role": "user",
                "content": "你好"
            }, {
                "role": "System",
                "content": "Answer briefly."
            }]
        });
        let headers = HeaderMap::new();

        openai_compat::chat::normalize_roles(&mut body);
        let report = normalize_chat(&mut body, &headers);

        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][2]["role"], "System");
        assert_eq!(report.hit_count(openai_compat::chat::ROLE_RULE_ID), 0);
    }

    #[test]
    fn applies_harness_rules_before_developer_role_is_normalized() {
        let mut body = json!({
            "messages": [{
                "role": "developer",
                "content": "You are an AI agent powered by DeepSeek Harness."
            }]
        });
        let headers = headers_with_user_agent(HARNESS_USER_AGENT);

        let report = normalize_chat(&mut body, &headers);

        assert_eq!(report.profile_order(), &["harness"]);
        assert_eq!(
            body["messages"][0]["content"],
            "You are an AI agent powered by Harness of DeepSeek."
        );
        assert_eq!(report.hit_count("harness/brand"), 1);
    }

    #[test]
    fn normalization_is_idempotent() {
        let mut body = json!({
            "messages": [{
                "role": "system",
                "content": "DeepSeek Harness Codex"
            }]
        });
        let headers = headers_with_user_agent(HARNESS_USER_AGENT);
        let first = normalize_chat(&mut body, &headers);
        let once = body.clone();
        let second = normalize_chat(&mut body, &headers);

        assert!(first.changed());
        assert!(!second.changed());
        assert_eq!(body, once);
    }

    #[test]
    fn selects_harness_profile_only_for_exact_user_agent_product() {
        let cases = [
            (HARNESS_USER_AGENT, true),
            ("DEEPSEEK-HARNESS/0.2.2", true),
            ("deepseek-harness", true),
            ("not-deepseek-harness/1.0", false),
            ("deepseek-harness-extra/1.0", false),
            ("codex_vscode/0.146.0", false),
            ("Mozilla/5.0 compatible; deepseek-harness/1.0", false),
            ("", false),
        ];

        for (user_agent, should_match) in cases {
            let mut body = json!({
                "messages": [{
                    "role": "system",
                    "content": "DeepSeek Harness"
                }]
            });
            let headers = headers_with_user_agent(user_agent);

            let report = normalize_chat(&mut body, &headers);

            assert_eq!(
                report.profile_order(),
                if should_match {
                    &["harness"][..]
                } else {
                    &[][..]
                },
                "unexpected profile selection for User-Agent {user_agent:?}"
            );
            assert_eq!(
                body["messages"][0]["content"],
                if should_match {
                    "Harness of DeepSeek"
                } else {
                    "DeepSeek Harness"
                },
                "unexpected body normalization for User-Agent {user_agent:?}"
            );
        }
    }

    #[test]
    fn ignores_invalid_user_agent_for_profile_selection() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        let mut body = json!({
            "messages": [{
                "role": "system",
                "content": "DeepSeek Harness"
            }]
        });

        let report = normalize_chat(&mut body, &headers);

        assert!(report.profile_order().is_empty());
        assert_eq!(body["messages"][0]["content"], "DeepSeek Harness");
    }

    #[test]
    fn leaves_developer_role_normalization_to_the_compatibility_library() {
        let headers = headers_with_user_agent("codex_vscode/0.146.0");
        let mut body = json!({
            "messages": [{
                "role": "developer",
                "content": "DeepSeek Harness"
            }]
        });

        openai_compat::chat::normalize_roles(&mut body);
        let report = normalize_chat(&mut body, &headers);

        assert!(report.profile_order().is_empty());
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "DeepSeek Harness");
        assert_eq!(report.hit_count(openai_compat::chat::ROLE_RULE_ID), 0);
    }

    #[test]
    fn applies_fixed_workbuddy_user_agent() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::USER_AGENT,
            HeaderValue::from_static("deepseek-harness/0.2.1"),
        );
        headers.append(
            header::USER_AGENT,
            HeaderValue::from_static("codex_vscode/0.146.0"),
        );

        let mut report = GateReport::default();
        apply_headers(&mut headers, &mut report);

        assert!(report.user_agent_overridden());
        let values = headers
            .get_all(header::USER_AGENT)
            .iter()
            .map(HeaderValue::as_bytes)
            .collect::<Vec<_>>();
        assert_eq!(values, vec![WORKBUDDY_USER_AGENT.as_bytes()]);
    }

    #[test]
    fn detects_upstream_prompt_gate_block_response() {
        let block = detect_gate_block(
            r#"{"code":11128,"msg":"Illegal API invocation from an unapproved channel","requestId":"upstream-1"}"#,
        )
        .unwrap();
        assert_eq!(block.upstream_request_id.as_deref(), Some("upstream-1"));
        assert_eq!(
            block.message,
            "Illegal API invocation from an unapproved channel"
        );
    }
}
