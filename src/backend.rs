use std::collections::BTreeMap;

use animus_plugin_protocol::{HealthCheckResult, HealthStatus};
use animus_subject_protocol::{
    BackendError, CustomFieldKind, CustomFieldSpec, EventStream, Subject, SubjectBackend,
    SubjectFilter, SubjectId, SubjectList, SubjectPatch, SubjectSchema, SubjectStatus,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::config::SentryConfig;

const ID_PREFIX: &str = "sentry:";
const KIND_INCIDENT: &str = "incident";

pub struct SentryBackend {
    config: SentryConfig,
    client: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeId {
    issue_id: String,
}

impl NativeId {
    fn parse(id: &SubjectId) -> Result<Self, BackendError> {
        let raw = id.as_str();
        let issue_id = raw.strip_prefix(ID_PREFIX).ok_or_else(|| {
            BackendError::InvalidRequest(format!(
                "expected Sentry subject id shaped sentry:<issue-id>, got {raw}"
            ))
        })?;
        if issue_id.is_empty() {
            return Err(BackendError::InvalidRequest(format!(
                "empty Sentry issue id in {raw}"
            )));
        }
        Ok(Self {
            issue_id: issue_id.to_string(),
        })
    }

    fn subject_id(issue_id: &str) -> SubjectId {
        SubjectId::new(format!("{ID_PREFIX}{issue_id}"))
    }
}

impl SentryBackend {
    pub fn new(config: SentryConfig) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("animus-subject-sentry/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { config, client })
    }

    fn org_slug(&self) -> Result<&str, BackendError> {
        self.config
            .org_slug
            .as_deref()
            .ok_or_else(|| BackendError::InvalidRequest("SENTRY_ORG_SLUG must be set".to_string()))
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, BackendError> {
        let token = self.config.auth_token.as_deref().ok_or_else(|| {
            BackendError::PermissionDenied("SENTRY_AUTH_TOKEN must be set".to_string())
        })?;
        let url = format!("{}{}", self.config.api_base, path);
        Ok(self
            .client
            .request(method, url)
            .header("Accept", "application/json")
            .bearer_auth(token))
    }

    async fn json_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, BackendError> {
        let mut req = self.request(method, path)?;
        if let Some(body) = body {
            req = req.json(&body);
        }
        let response = req
            .send()
            .await
            .map_err(|e| BackendError::Unavailable(e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| BackendError::Unavailable(e.to_string()))?;

        if status == StatusCode::NOT_FOUND {
            return Err(BackendError::NotFound(path.to_string()));
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(BackendError::PermissionDenied(format!(
                "Sentry API returned {status}: {text}"
            )));
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(BackendError::Unavailable(format!(
                "Sentry API rate limited request: {text}"
            )));
        }
        if !status.is_success() {
            return Err(BackendError::Unavailable(format!(
                "Sentry API returned {status}: {text}"
            )));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| BackendError::Other(e.into()))
    }

    async fn fetch_issue(&self, id: &NativeId) -> Result<Value, BackendError> {
        let org_slug = self.org_slug()?;
        self.json_request(
            reqwest::Method::GET,
            &format!(
                "/organizations/{}/issues/{}/",
                encode(org_slug),
                encode(&id.issue_id)
            ),
            None,
        )
        .await
    }

    fn issue_to_subject(&self, issue: &Value) -> Result<Subject, BackendError> {
        let issue_id = issue
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| BackendError::Other(anyhow::anyhow!("Sentry issue missing id")))?;
        let status = issue
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unresolved")
            .to_string();
        let level = issue
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_string();
        let project = issue
            .get("project")
            .and_then(|p| p.get("slug").or_else(|| p.get("name")))
            .and_then(Value::as_str)
            .map(str::to_string);
        let short_id = issue
            .get("shortId")
            .or_else(|| issue.get("short_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let issue_type = issue
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string);

        let mut labels = Vec::new();
        labels.push(format!("level:{level}"));
        if let Some(project) = &project {
            labels.push(format!("project:{project}"));
        }
        if let Some(issue_type) = &issue_type {
            labels.push(format!("type:{issue_type}"));
        }

        let mut custom = BTreeMap::new();
        custom.insert("level".to_string(), json!(level));
        if let Some(project) = &project {
            custom.insert("project".to_string(), json!(project));
        }
        if let Some(short_id) = &short_id {
            custom.insert("short_id".to_string(), json!(short_id));
        }
        if let Some(count) = issue.get("count").and_then(as_u64ish) {
            custom.insert("count".to_string(), json!(count));
        }
        if let Some(user_count) = issue.get("userCount").and_then(as_u64ish) {
            custom.insert("user_count".to_string(), json!(user_count));
        }

        Ok(Subject {
            id: NativeId::subject_id(issue_id),
            kind: KIND_INCIDENT.to_string(),
            title: issue
                .get("title")
                .or_else(|| issue.get("metadata").and_then(|m| m.get("title")))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            description: issue
                .get("culprit")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            status: map_status(&status),
            priority: priority_from_level(&level),
            assignee: assigned_to(issue.get("assignedTo")),
            labels,
            parent: None,
            children: Vec::new(),
            url: issue
                .get("permalink")
                .and_then(Value::as_str)
                .map(str::to_string),
            created_at: parse_ts(issue.get("firstSeen"))?,
            updated_at: parse_ts(issue.get("lastSeen"))?,
            custom,
            native_status: Some(status),
            status_metadata: issue.get("statusDetails").cloned().unwrap_or(Value::Null),
            attachments: Vec::new(),
        })
    }

    fn subject_matches_filter(subject: &Subject, filter: &SubjectFilter) -> bool {
        if !filter.kind.is_empty() && !filter.kind.contains(&subject.kind) {
            return false;
        }
        if !filter.status.is_empty() && !filter.status.contains(&subject.status) {
            return false;
        }
        if !filter.assignee.is_empty() {
            match &subject.assignee {
                Some(assignee) if filter.assignee.contains(assignee) => {}
                _ => return false,
            }
        }
        if !filter.labels_any.is_empty()
            && !filter
                .labels_any
                .iter()
                .any(|label| subject.labels.contains(label))
        {
            return false;
        }
        if !filter
            .labels_all
            .iter()
            .all(|label| subject.labels.contains(label))
        {
            return false;
        }
        if let Some(updated_since) = filter.updated_since {
            if subject.updated_at < updated_since {
                return false;
            }
        }
        true
    }
}

#[async_trait]
impl SubjectBackend for SentryBackend {
    async fn list(&self, filter: SubjectFilter) -> Result<SubjectList, BackendError> {
        let org_slug = self.org_slug()?;
        let limit = filter.limit.unwrap_or(50).clamp(1, 100);
        let mut params = vec![format!("limit={limit}")];
        if let Some(query) = self
            .config
            .query
            .clone()
            .or_else(|| query_for_status_filter(&filter.status))
        {
            params.push(format!("query={}", encode(&query)));
        }
        for project_id in &self.config.project_ids {
            params.push(format!("project={}", encode(project_id)));
        }
        let path = format!(
            "/organizations/{}/issues/?{}",
            encode(org_slug),
            params.join("&")
        );
        let value = self.json_request(reqwest::Method::GET, &path, None).await?;
        let issues = value.as_array().ok_or_else(|| {
            BackendError::Other(anyhow::anyhow!("Sentry issue list was not an array"))
        })?;
        let mut subjects = Vec::new();
        for issue in issues {
            let subject = self.issue_to_subject(issue)?;
            if Self::subject_matches_filter(&subject, &filter) {
                subjects.push(subject);
            }
        }

        Ok(SubjectList {
            subjects,
            next_cursor: None,
            fetched_at: Utc::now(),
        })
    }

    async fn get(&self, id: &SubjectId) -> Result<Subject, BackendError> {
        let native = NativeId::parse(id)?;
        let issue = self.fetch_issue(&native).await?;
        self.issue_to_subject(&issue)
    }

    async fn update(&self, id: &SubjectId, patch: SubjectPatch) -> Result<Subject, BackendError> {
        if !patch.labels_add.is_empty() || !patch.labels_remove.is_empty() {
            return Err(BackendError::InvalidRequest(
                "Sentry issue labels are derived from issue metadata and cannot be mutated".into(),
            ));
        }
        if patch.comment.as_ref().is_some_and(|s| !s.is_empty()) {
            return Err(BackendError::InvalidRequest(
                "Sentry issue comments are not implemented by this plugin".into(),
            ));
        }

        let native = NativeId::parse(id)?;
        let org_slug = self.org_slug()?;
        let mut body = serde_json::Map::new();
        if let Some(status) = patch.status {
            body.insert("status".to_string(), json!(status_to_native(status)));
        }
        if let Some(assignee) = patch.assignee {
            match assignee {
                Some(assigned_to) => {
                    body.insert("assignedTo".to_string(), json!(assigned_to));
                }
                None => {
                    body.insert("assignedTo".to_string(), Value::Null);
                }
            }
        }

        if !body.is_empty() {
            self.json_request(
                reqwest::Method::PUT,
                &format!(
                    "/organizations/{}/issues/{}/",
                    encode(org_slug),
                    encode(&native.issue_id)
                ),
                Some(Value::Object(body)),
            )
            .await?;
        }

        self.get(id).await
    }

    async fn watch(&self) -> Option<EventStream> {
        None
    }

    fn schema(&self) -> SubjectSchema {
        SubjectSchema {
            kinds: vec![KIND_INCIDENT.to_string()],
            status_values: vec![
                SubjectStatus::Ready,
                SubjectStatus::InProgress,
                SubjectStatus::Blocked,
                SubjectStatus::Done,
                SubjectStatus::Cancelled,
            ],
            supports_watch: false,
            supports_create: false,
            supports_delete: false,
            supports_pagination: false,
            native_status_values: vec![
                "unresolved".to_string(),
                "resolved".to_string(),
                "ignored".to_string(),
            ],
            status_dispatch_hints: Vec::new(),
            custom_fields: vec![
                CustomFieldSpec {
                    key: "level".to_string(),
                    kind: CustomFieldKind::String,
                    values: Some(vec![
                        "fatal".to_string(),
                        "error".to_string(),
                        "warning".to_string(),
                        "info".to_string(),
                        "debug".to_string(),
                    ]),
                },
                CustomFieldSpec {
                    key: "project".to_string(),
                    kind: CustomFieldKind::String,
                    values: None,
                },
                CustomFieldSpec {
                    key: "short_id".to_string(),
                    kind: CustomFieldKind::String,
                    values: None,
                },
                CustomFieldSpec {
                    key: "count".to_string(),
                    kind: CustomFieldKind::Number,
                    values: None,
                },
                CustomFieldSpec {
                    key: "user_count".to_string(),
                    kind: CustomFieldKind::Number,
                    values: None,
                },
            ],
        }
    }

    async fn health(&self) -> Result<HealthCheckResult, BackendError> {
        let missing_token = self.config.auth_token.is_none();
        let missing_org = self.config.org_slug.is_none();
        let status = if missing_token || missing_org {
            HealthStatus::Unhealthy
        } else {
            HealthStatus::Healthy
        };
        let last_error = match (missing_token, missing_org) {
            (true, true) => Some("SENTRY_AUTH_TOKEN and SENTRY_ORG_SLUG unset".to_string()),
            (true, false) => Some("SENTRY_AUTH_TOKEN unset".to_string()),
            (false, true) => Some("SENTRY_ORG_SLUG unset".to_string()),
            (false, false) => None,
        };
        Ok(HealthCheckResult {
            status,
            uptime_ms: None,
            memory_usage_bytes: None,
            last_error,
        })
    }
}

fn parse_ts(value: Option<&Value>) -> Result<DateTime<Utc>, BackendError> {
    let raw = value
        .and_then(Value::as_str)
        .ok_or_else(|| BackendError::Other(anyhow::anyhow!("Sentry issue missing timestamp")))?;
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| BackendError::Other(e.into()))
}

fn map_status(status: &str) -> SubjectStatus {
    match status {
        "resolved" => SubjectStatus::Done,
        "ignored" => SubjectStatus::Cancelled,
        _ => SubjectStatus::Ready,
    }
}

fn status_to_native(status: SubjectStatus) -> &'static str {
    match status {
        SubjectStatus::Done => "resolved",
        SubjectStatus::Cancelled => "ignored",
        SubjectStatus::Ready | SubjectStatus::InProgress | SubjectStatus::Blocked => "unresolved",
    }
}

fn priority_from_level(level: &str) -> Option<u8> {
    match level {
        "fatal" => Some(4),
        "error" => Some(3),
        "warning" => Some(2),
        "info" | "debug" => Some(1),
        _ => None,
    }
}

fn assigned_to(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(raw) = value.as_str() {
        return Some(raw.to_string());
    }
    value
        .get("id")
        .or_else(|| value.get("name"))
        .or_else(|| value.get("email"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn as_u64ish(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
}

fn query_for_status_filter(statuses: &[SubjectStatus]) -> Option<String> {
    if statuses.is_empty() {
        return None;
    }
    if statuses.iter().all(|s| matches!(s, SubjectStatus::Done)) {
        return Some("is:resolved".to_string());
    }
    if statuses
        .iter()
        .all(|s| matches!(s, SubjectStatus::Cancelled))
    {
        return Some("is:ignored".to_string());
    }
    if statuses
        .iter()
        .all(|s| !matches!(s, SubjectStatus::Done | SubjectStatus::Cancelled))
    {
        return Some("is:unresolved".to_string());
    }
    None
}

fn encode(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_issue() -> Value {
        json!({
            "id": "123",
            "shortId": "APP-1",
            "title": "TypeError: cannot read property",
            "culprit": "checkout.submit",
            "status": "unresolved",
            "level": "error",
            "permalink": "https://sentry.example/issues/123",
            "firstSeen": "2026-05-28T00:00:00Z",
            "lastSeen": "2026-05-28T01:00:00Z",
            "count": "42",
            "userCount": 9,
            "project": { "slug": "frontend" },
            "assignedTo": { "id": "u1", "name": "Sam" },
            "type": "error"
        })
    }

    #[test]
    fn native_id_parses() {
        let parsed = NativeId::parse(&SubjectId::new("sentry:123")).unwrap();
        assert_eq!(parsed.issue_id, "123");
    }

    #[test]
    fn issue_maps_to_subject() {
        let backend =
            SentryBackend::new(SentryConfig::for_testing("https://sentry.example/api/0")).unwrap();
        let subject = backend.issue_to_subject(&sample_issue()).unwrap();
        assert_eq!(subject.id.as_str(), "sentry:123");
        assert_eq!(subject.kind, KIND_INCIDENT);
        assert_eq!(subject.status, SubjectStatus::Ready);
        assert_eq!(subject.priority, Some(3));
        assert_eq!(subject.assignee.as_deref(), Some("u1"));
        assert!(subject.labels.contains(&"project:frontend".to_string()));
    }

    #[test]
    fn maps_status_filters_to_queries() {
        assert_eq!(
            query_for_status_filter(&[SubjectStatus::Done]).as_deref(),
            Some("is:resolved")
        );
        assert_eq!(
            query_for_status_filter(&[SubjectStatus::Cancelled]).as_deref(),
            Some("is:ignored")
        );
        assert_eq!(
            query_for_status_filter(&[SubjectStatus::Ready, SubjectStatus::Blocked]).as_deref(),
            Some("is:unresolved")
        );
        assert!(query_for_status_filter(&[SubjectStatus::Ready, SubjectStatus::Done]).is_none());
    }
}
