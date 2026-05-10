//! Per-agent secret access policy.
//!
//! This policy gates secret-name discovery and placeholder substitution by
//! identity. It deliberately contains no secret values.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const POLICY_FILE_NAME: &str = "secret-access-policy.toml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecretAccessPolicy {
    pub rules: Vec<SecretAccessRule>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecretAccessRule {
    pub agents: Vec<String>,
    pub users: Vec<String>,
    pub channels: Vec<String>,
    pub secrets: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretAccessIdentity {
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    pub channel: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AccessPolicyError {
    #[error("failed to read secret access policy at {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to parse secret access policy at {path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
}

impl SecretAccessIdentity {
    pub fn from_env() -> Self {
        Self {
            agent_id: env_trimmed("CALCIFORGE_AGENT_ID"),
            user_id: env_trimmed("CALCIFORGE_USER_ID"),
            channel: env_trimmed("CALCIFORGE_CHANNEL_ID")
                .or_else(|| env_trimmed("CALCIFORGE_CHANNEL")),
        }
    }

    pub fn is_known(&self) -> bool {
        self.agent_id.is_some() || self.user_id.is_some() || self.channel.is_some()
    }
}

impl SecretAccessPolicy {
    pub fn allows(&self, identity: &SecretAccessIdentity, secret_name: &str) -> bool {
        if !identity.is_known() {
            return true;
        }

        self.rules
            .iter()
            .any(|rule| rule.matches_identity(identity) && rule.allows_secret(secret_name))
    }

    pub fn filter_names(&self, identity: &SecretAccessIdentity, names: Vec<String>) -> Vec<String> {
        if !identity.is_known() {
            return names;
        }
        names
            .into_iter()
            .filter(|name| self.allows(identity, name))
            .collect()
    }
}

impl SecretAccessRule {
    fn matches_identity(&self, identity: &SecretAccessIdentity) -> bool {
        selector_allows(&self.agents, identity.agent_id.as_deref())
            && selector_allows(&self.users, identity.user_id.as_deref())
            && selector_allows(&self.channels, identity.channel.as_deref())
    }

    fn allows_secret(&self, secret_name: &str) -> bool {
        !self.secrets.is_empty()
            && self
                .secrets
                .iter()
                .any(|pattern| wildcard_match(pattern, secret_name))
    }
}

pub fn default_access_policy_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CALCIFORGE_SECRET_ACCESS_POLICY_FILE").map(PathBuf::from)
    {
        return Some(path);
    }
    if let Some(path) = std::env::var_os("CALCIFORGE_CONFIG_HOME")
        .map(PathBuf::from)
        .map(|base| base.join(POLICY_FILE_NAME))
    {
        return Some(path);
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .map(|base| base.join("calciforge").join(POLICY_FILE_NAME))
    {
        return Some(path);
    }
    std::env::var_os("HOME").map(PathBuf::from).map(|home| {
        home.join(".config")
            .join("calciforge")
            .join(POLICY_FILE_NAME)
    })
}

pub fn load_default_access_policy() -> Result<SecretAccessPolicy, AccessPolicyError> {
    let Some(path) = default_access_policy_path() else {
        return Ok(SecretAccessPolicy::default());
    };
    load_access_policy(path)
}

pub fn load_access_policy(
    path: impl Into<PathBuf>,
) -> Result<SecretAccessPolicy, AccessPolicyError> {
    let path = path.into();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SecretAccessPolicy::default());
        }
        Err(source) => {
            return Err(AccessPolicyError::Read {
                path: path.display().to_string(),
                source,
            });
        }
    };
    toml::from_str(&text).map_err(|source| AccessPolicyError::Parse {
        path: path.display().to_string(),
        source,
    })
}

fn selector_allows(patterns: &[String], value: Option<&str>) -> bool {
    patterns.is_empty()
        || value.is_some_and(|value| {
            patterns
                .iter()
                .any(|pattern| wildcard_match(pattern, value))
        })
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" || pattern == value {
        return true;
    }

    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut pi, mut vi) = (0, 0);
    let (mut star, mut star_value) = (None, 0);

    while vi < value.len() {
        if pi < pattern.len() && pattern[pi] == value[vi] {
            pi += 1;
            vi += 1;
        } else if pi < pattern.len() && pattern[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            star_value = vi;
        } else if let Some(star_index) = star {
            pi = star_index + 1;
            star_value += 1;
            vi = star_value;
        } else {
            return false;
        }
    }

    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }
    pi == pattern.len()
}

fn env_trimmed(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_identity_preserves_process_scoped_access() {
        let policy = SecretAccessPolicy::default();
        assert!(policy.allows(&SecretAccessIdentity::default(), "OPENAI_API_KEY"));
    }

    #[test]
    fn known_identity_without_matching_rule_fails_closed() {
        let policy = SecretAccessPolicy::default();
        let identity = SecretAccessIdentity {
            agent_id: Some("researcher".into()),
            ..SecretAccessIdentity::default()
        };

        assert!(!policy.allows(&identity, "OPENAI_API_KEY"));
    }

    #[test]
    fn matching_agent_rule_allows_secret_pattern() {
        let policy = SecretAccessPolicy {
            rules: vec![SecretAccessRule {
                agents: vec!["research-*".into()],
                secrets: vec!["BRAVE_*".into()],
                ..SecretAccessRule::default()
            }],
        };
        let identity = SecretAccessIdentity {
            agent_id: Some("research-web".into()),
            ..SecretAccessIdentity::default()
        };

        assert!(policy.allows(&identity, "BRAVE_API_KEY"));
        assert!(!policy.allows(&identity, "OPENAI_API_KEY"));
    }

    #[test]
    fn user_and_channel_selectors_must_match_when_present() {
        let policy = SecretAccessPolicy {
            rules: vec![SecretAccessRule {
                users: vec!["+1215*".into()],
                channels: vec!["signal".into()],
                secrets: vec!["HOME_*".into()],
                ..SecretAccessRule::default()
            }],
        };

        assert!(policy.allows(
            &SecretAccessIdentity {
                user_id: Some("+12154609585".into()),
                channel: Some("signal".into()),
                ..SecretAccessIdentity::default()
            },
            "HOME_ASSISTANT_TOKEN"
        ));
        assert!(!policy.allows(
            &SecretAccessIdentity {
                user_id: Some("+12154609585".into()),
                channel: Some("sms".into()),
                ..SecretAccessIdentity::default()
            },
            "HOME_ASSISTANT_TOKEN"
        ));
    }

    #[test]
    fn loads_documented_policy_file_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret-access-policy.toml");
        std::fs::write(
            &path,
            r#"
[[rules]]
agents = ["research-*"]
users = ["brian"]
channels = ["signal"]
secrets = ["BRAVE_*", "SEARCH_*"]
"#,
        )
        .unwrap();

        let policy = load_access_policy(&path).unwrap();
        let identity = SecretAccessIdentity {
            agent_id: Some("research-web".into()),
            user_id: Some("brian".into()),
            channel: Some("signal".into()),
        };

        assert_eq!(
            policy.filter_names(
                &identity,
                vec![
                    "OPENAI_API_KEY".into(),
                    "BRAVE_API_KEY".into(),
                    "SEARCH_API_KEY".into(),
                ],
            ),
            vec!["BRAVE_API_KEY", "SEARCH_API_KEY"]
        );
    }
}
