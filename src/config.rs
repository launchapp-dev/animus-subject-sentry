use anyhow::Result;

pub const DEFAULT_SENTRY_API_BASE: &str = "https://sentry.io/api/0";
pub const ENV_AUTH_TOKEN: &str = "SENTRY_AUTH_TOKEN";
pub const ENV_ORG_SLUG: &str = "SENTRY_ORG_SLUG";
pub const ENV_PROJECT_IDS: &str = "SENTRY_PROJECT_IDS";
pub const ENV_QUERY: &str = "SENTRY_QUERY";
pub const ENV_API_BASE: &str = "SENTRY_API_BASE";

#[derive(Debug, Clone)]
pub struct SentryConfig {
    pub auth_token: Option<String>,
    pub org_slug: Option<String>,
    pub project_ids: Vec<String>,
    pub query: Option<String>,
    pub api_base: String,
}

impl SentryConfig {
    pub fn from_env() -> Result<Self> {
        let auth_token = std::env::var(ENV_AUTH_TOKEN).ok().filter(|s| !s.is_empty());
        let org_slug = std::env::var(ENV_ORG_SLUG).ok().filter(|s| !s.is_empty());
        let project_ids = std::env::var(ENV_PROJECT_IDS)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let query = std::env::var(ENV_QUERY)
            .ok()
            .filter(|s| !s.trim().is_empty());
        let api_base = std::env::var(ENV_API_BASE)
            .unwrap_or_else(|_| DEFAULT_SENTRY_API_BASE.to_string())
            .trim_end_matches('/')
            .to_string();

        Ok(Self {
            auth_token,
            org_slug,
            project_ids,
            query,
            api_base,
        })
    }

    pub fn for_testing(api_base: impl Into<String>) -> Self {
        Self {
            auth_token: Some("test-token".into()),
            org_slug: Some("example".into()),
            project_ids: Vec::new(),
            query: None,
            api_base: api_base.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn testing_config_uses_api_base() {
        let config = SentryConfig::for_testing("https://sentry.example/api/0");
        assert_eq!(config.api_base, "https://sentry.example/api/0");
        assert_eq!(config.org_slug.as_deref(), Some("example"));
    }
}
