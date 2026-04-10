//! Domain list management
//!
//! Supports:
//! - Static lists of domains/regex patterns
//! - Dynamic fetching of threat intelligence lists (e.g., malware lists)
//! - Per-agent allow/deny lists
//! - Regex pattern matching

use anyhow::{Context, Result};
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{info, warn};

/// A compiled domain matcher that supports exact matches and regex patterns
#[derive(Debug, Clone)]
pub struct DomainList {
    name: String,
    exact: HashSet<String>,
    patterns: Vec<Regex>,
}

impl DomainList {
    /// Create an empty domain list
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            exact: HashSet::new(),
            patterns: Vec::new(),
        }
    }

    /// Get the name of this domain list
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Add an exact domain match
    pub fn add_domain(&mut self, domain: &str) {
        self.exact.insert(domain.to_lowercase());
    }

    /// Add a regex pattern
    ///
    /// Patterns must start with `~` to indicate regex, e.g.:
    /// - `~.*\.malware\.com` — match any subdomain of malware.com
    /// - `~^tracking\.` — match domains starting with "tracking."
    pub fn add_pattern(&mut self, pattern: &str) -> Result<()> {
        let compiled =
            Regex::new(pattern).with_context(|| format!("Invalid regex pattern: {}", pattern))?;
        self.patterns.push(compiled);
        Ok(())
    }

    /// Parse a list file with domain entries
    ///
    /// Format:
    /// - Plain lines are exact domain matches
    /// - Lines starting with `~` are regex patterns
    /// - Lines starting with `#` are comments
    /// - Empty lines are ignored
    pub fn parse(&mut self, content: &str) -> Result<()> {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some(pattern) = line.strip_prefix('~') {
                self.add_pattern(pattern)?;
            } else {
                // Handle HOSTS format: "0.0.0.0 domain" or "127.0.0.1 domain"
                let domain = if let Some(d) = line.strip_prefix("0.0.0.0 ") {
                    d
                } else if let Some(d) = line.strip_prefix("127.0.0.1 ") {
                    d
                } else {
                    line
                };
                // Handle HTTP URL format: extract domain from URLs
                let domain = if domain.starts_with("http://") || domain.starts_with("https://") {
                    url::Url::parse(domain)
                        .ok()
                        .and_then(|u| u.host_str().map(|h| h.to_string()))
                        .unwrap_or_else(|| domain.to_string())
                } else {
                    domain.to_string()
                };
                self.add_domain(&domain);
            }
        }
        Ok(())
    }

    /// Check if a domain matches this list
    pub fn matches(&self, domain: &str) -> bool {
        let domain_lower = domain.to_lowercase();

        // Check exact matches (including subdomain checks)
        if self.exact.contains(&domain_lower) {
            return true;
        }

        // Check subdomain matches
        // e.g., "foo.example.com" should match "example.com"
        for part in self.exact.iter() {
            if domain_lower.ends_with(&format!(".{}", part)) {
                return true;
            }
        }

        // Check regex patterns
        self.patterns
            .iter()
            .any(|p: &regex::Regex| p.is_match(&domain_lower))
    }

    /// Number of entries
    pub fn len(&self) -> usize {
        self.exact.len() + self.patterns.len()
    }

    /// Whether the list is empty
    pub fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.patterns.is_empty()
    }
}

/// Manages dynamic domain lists with caching and periodic refresh
pub struct DomainListManager {
    lists: RwLock<Vec<DynamicList>>,
}

struct DynamicList {
    name: String,
    url: String,
    refresh_interval: Duration,
    last_fetched: Option<Instant>,
    list: DomainList,
}

impl Default for DomainListManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DomainListManager {
    pub fn new() -> Self {
        Self {
            lists: RwLock::new(Vec::new()),
        }
    }

    /// Add a dynamic list that fetches from a URL
    ///
    /// Supports:
    /// - Plain text lists (one domain per line)
    /// - HOSTS format (e.g., `0.0.0.0 malware.com`)
    /// - Adblock syntax for blocklists (simplified)
    ///
    /// Common sources:
    /// - `https://urlhaus.abuse.ch/downloads/text/` — malware URLs
    /// - `https://raw.githubusercontent.com/StevenBlack/hosts/master/hosts` — adware/malware
    /// - `https://www.malwaredomainlist.com/hostslist/hosts.txt` — malware domains
    /// - `https://pgl.yoyo.org/adservers/serverlist.php?hostformat=hosts&showintro=0` — ad servers
    pub async fn add_source(&self, name: &str, url: &str, refresh: Duration) -> Result<()> {
        let mut lists = self.lists.write().await;
        lists.push(DynamicList {
            name: name.to_string(),
            url: url.to_string(),
            refresh_interval: refresh,
            last_fetched: None,
            list: DomainList::new(name),
        });
        Ok(())
    }

    /// Refresh all lists that are due for refresh
    pub async fn refresh_all(&self, client: &reqwest::Client) -> Result<()> {
        let mut lists = self.lists.write().await;
        let now = Instant::now();

        for dl in lists.iter_mut() {
            let needs_refresh = dl
                .last_fetched
                .map(|f| now.duration_since(f) > dl.refresh_interval)
                .unwrap_or(true);

            if needs_refresh {
                if let Err(e) = dl.fetch(client).await {
                    warn!(name = %dl.name, error = %e, "Failed to refresh domain list");
                }
            }
        }

        Ok(())
    }

    /// Check if a domain is in any managed list
    pub async fn matches(&self, domain: &str) -> Vec<String> {
        let lists = self.lists.read().await;
        lists
            .iter()
            .filter(|dl| dl.list.matches(domain))
            .map(|dl| dl.name.clone())
            .collect()
    }

    /// Get summary of all loaded lists
    pub async fn summary(&self) -> Vec<(String, usize)> {
        let lists = self.lists.read().await;
        lists
            .iter()
            .map(|dl| (dl.name.clone(), dl.list.len()))
            .collect()
    }
}

impl DynamicList {
    /// Fetch and parse the list from its URL
    async fn fetch(&mut self, client: &reqwest::Client) -> Result<()> {
        info!(name = %self.name, url = %self.url, "Fetching domain list");

        let text = client
            .get(&self.url)
            .send()
            .await
            .with_context(|| format!("Failed to fetch {}", self.name))?
            .text()
            .await
            .with_context(|| format!("Failed to read {} response body", self.name))?;

        // Parse the content, handling HOSTS format and plain lists
        let mut list = DomainList::new(&self.name);
        list.parse(&self.normalize_hosts_format(&text))?;

        let count = list.len();
        self.list = list;
        self.last_fetched = Some(Instant::now());

        info!(name = %self.name, count, "Domain list updated");
        Ok(())
    }

    /// Normalize HOSTS format to plain domain lists
    /// Converts `0.0.0.0 domain.com` → `domain.com`
    fn normalize_hosts_format(&self, text: &str) -> String {
        text.lines()
            .map(|line| {
                let line = line.trim();
                // HOSTS format: "0.0.0.0 domain" or "127.0.0.1 domain"
                if line.starts_with("0.0.0.0 ") {
                    line.trim_start_matches("0.0.0.0 ").to_string()
                } else if line.starts_with("127.0.0.1 ") {
                    line.trim_start_matches("127.0.0.1 ").to_string()
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Load a static domain list from a file path
pub async fn load_static_list(path: &Path, name: &str) -> Result<DomainList> {
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read domain list: {}", path.display()))?;

    let mut list = DomainList::new(name);
    list.parse(&content)?;

    info!(name, count = list.len(), path = %path.display(), "Loaded static domain list");
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let mut list = DomainList::new("test");
        list.add_domain("malware.com");
        list.add_domain("evil.org");

        assert!(list.matches("malware.com"));
        assert!(list.matches("MALWARE.COM")); // case insensitive
        assert!(list.matches("evil.org"));
        assert!(!list.matches("safe.com"));
    }

    #[test]
    fn test_subdomain_match() {
        let mut list = DomainList::new("test");
        list.add_domain("example.com");

        assert!(list.matches("example.com"));
        assert!(list.matches("sub.example.com"));
        assert!(list.matches("deep.sub.example.com"));
        assert!(!list.matches("example.net"));
    }

    #[test]
    fn test_regex_pattern() {
        let mut list = DomainList::new("test");
        list.add_pattern(r".*\.malware\.com").unwrap();
        list.add_pattern(r"^tracking\..*").unwrap();

        assert!(list.matches("evil.malware.com"));
        assert!(list.matches("tracking.example.com"));
        assert!(!list.matches("safe.com"));
    }

    #[test]
    fn test_parse_hosts_format() {
        let content = r#"
# Comment line
0.0.0.0 malware.com
127.0.0.1 adware.org
~.*\.tracker\.net
plain-domain.net
"#;
        let mut list = DomainList::new("test");
        list.parse(content).unwrap();

        assert!(list.matches("malware.com"));
        assert!(list.matches("adware.org"));
        assert!(list.matches("evil.tracker.net"));
        assert!(list.matches("plain-domain.net"));
        assert!(!list.matches("safe.com"));
    }

    #[test]
    fn test_malware_urlhaus_format() {
        let content = r#"
http://1.2.3.4/path/malware.exe
http://malware.com/c2
~.*\.suspicious\.tk
"#;
        let mut list = DomainList::new("urlhaus");
        list.parse(content).unwrap();

        // Exact matches
        assert!(list.matches("1.2.3.4"));
        assert!(list.matches("malware.com"));
        // Regex
        assert!(list.matches("evil.suspicious.tk"));
    }
}
