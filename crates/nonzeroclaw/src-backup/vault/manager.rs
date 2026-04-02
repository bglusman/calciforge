//! `VaultManager` — combines adapter + policy + relay into a single access point.
//!
//! The manager enforces per-secret `SecretPolicy`, tracks session-level approval
//! decisions, and delegates the actual credential fetch to the configured
//! `VaultAdapter`.

use crate::vault::{
    ApprovalDecision, ApprovalRelay, Secret, SecretPolicy, VaultAdapter, VaultConfig, VaultError,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// ── Session approval cache ────────────────────────────────────────────────────

/// A cached approval decision for `SecretPolicy::Session` or
/// `SecretPolicy::TimeBound` secrets.
struct CachedApproval {
    decision: ApprovalDecision,
    granted_at: Instant,
    /// For `TimeBound`: expiry instant.  `None` = valid for whole session.
    expires_at: Option<Instant>,
}

impl CachedApproval {
    fn is_valid(&self) -> bool {
        match self.expires_at {
            Some(exp) => Instant::now() < exp,
            None => true,
        }
    }
}

// ── VaultManager ─────────────────────────────────────────────────────────────

/// Combines a `VaultAdapter`, an `ApprovalRelay`, and per-secret `SecretPolicy`
/// into a unified secret-access point.
///
/// # Thread safety
///
/// `VaultManager` is `Send + Sync` and can be wrapped in an `Arc` and shared
/// across tasks.  The approval cache is protected by a `tokio::sync::Mutex`.
pub struct VaultManager {
    adapter: Arc<dyn VaultAdapter>,
    relay: Arc<dyn ApprovalRelay>,
    config: Arc<VaultConfig>,
    /// Per-key approval cache (for Session / TimeBound policies).
    approval_cache: Mutex<HashMap<String, CachedApproval>>,
}

impl VaultManager {
    /// Create a new `VaultManager`.
    pub fn new(
        adapter: Arc<dyn VaultAdapter>,
        relay: Arc<dyn ApprovalRelay>,
        config: Arc<VaultConfig>,
    ) -> Self {
        Self {
            adapter,
            relay,
            config,
            approval_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Build a `VaultManager` from the given config, using the appropriate
    /// adapter and a `NoopApprovalRelay`.
    ///
    /// For production use, call `new()` directly and supply the relay of your
    /// choice (e.g. `ChannelApprovalRelay` wired to Signal/Telegram).
    pub fn from_config(config: VaultConfig) -> Self {
        use crate::vault::{NoopApprovalRelay, VaultBackend};
        use crate::vault::adapter::NoopVaultAdapter;

        let relay: Arc<dyn ApprovalRelay> = Arc::new(NoopApprovalRelay);
        let config = Arc::new(config);

        let adapter: Arc<dyn VaultAdapter> = match config.backend {
            VaultBackend::None => Arc::new(NoopVaultAdapter),
            #[cfg(feature = "bitwarden-cli")]
            VaultBackend::BitwardenCli => Arc::new(
                crate::vault::BitwardenCliAdapter::from_config(&config),
            ),
            #[cfg(not(feature = "bitwarden-cli"))]
            VaultBackend::BitwardenCli => {
                warn!(
                    "vault.backend = \"bitwarden-cli\" is set but the \
                     \"bitwarden-cli\" feature is not compiled in. \
                     Falling back to noop adapter."
                );
                Arc::new(NoopVaultAdapter)
            }
        };

        Self::new(adapter, relay, config)
    }

    /// Access a secret, enforcing the configured `SecretPolicy`.
    ///
    /// 1. Resolves the secret's policy from `vault.secrets.<key>`.
    /// 2. For policies that require approval, checks the cache first.
    /// 3. If approval is needed, routes the request through the relay.
    /// 4. Fetches the secret from the adapter on approval.
    pub async fn access_secret(&self, key: &str) -> Result<Secret, VaultError> {
        let secret_cfg = self
            .config
            .secrets
            .get(key)
            .ok_or_else(|| VaultError::UnknownKey(key.to_owned()))?;

        let policy = secret_cfg.to_runtime_policy();

        debug!(key, ?policy, "vault: access_secret");

        match &policy {
            SecretPolicy::Auto => {
                // No approval needed — fetch directly.
                self.fetch(key).await
            }

            SecretPolicy::PerUse => {
                // Always request approval before fetching.
                self.require_approval(key, "").await?;
                self.fetch(key).await
            }

            SecretPolicy::Session => {
                // Approved once per session — check cache.
                if self.is_approved_cached(key).await {
                    self.fetch(key).await
                } else {
                    self.require_approval(key, "").await?;
                    self.cache_approval(key, None).await;
                    self.fetch(key).await
                }
            }

            SecretPolicy::TimeBound { ttl } => {
                // Approved once; valid until TTL expires.
                let expires_at = Instant::now() + *ttl;
                if self.is_approved_cached(key).await {
                    self.fetch(key).await
                } else {
                    self.require_approval(key, "").await?;
                    self.cache_approval(key, Some(expires_at)).await;
                    self.fetch(key).await
                }
            }
        }
    }

    /// Like `access_secret`, but include a rich context string that is forwarded
    /// to the approval relay message.
    pub async fn access_secret_with_context(
        &self,
        key: &str,
        context: &str,
    ) -> Result<Secret, VaultError> {
        let secret_cfg = self
            .config
            .secrets
            .get(key)
            .ok_or_else(|| VaultError::UnknownKey(key.to_owned()))?;

        let policy = secret_cfg.to_runtime_policy();

        match &policy {
            SecretPolicy::Auto => self.fetch(key).await,

            SecretPolicy::PerUse => {
                self.require_approval(key, context).await?;
                self.fetch(key).await
            }

            SecretPolicy::Session => {
                if self.is_approved_cached(key).await {
                    self.fetch(key).await
                } else {
                    self.require_approval(key, context).await?;
                    self.cache_approval(key, None).await;
                    self.fetch(key).await
                }
            }

            SecretPolicy::TimeBound { ttl } => {
                let expires_at = Instant::now() + *ttl;
                if self.is_approved_cached(key).await {
                    self.fetch(key).await
                } else {
                    self.require_approval(key, context).await?;
                    self.cache_approval(key, Some(expires_at)).await;
                    self.fetch(key).await
                }
            }
        }
    }

    /// Expose the underlying adapter for direct access (e.g. `store_secret`).
    pub fn adapter(&self) -> &Arc<dyn VaultAdapter> {
        &self.adapter
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Fetch the secret from the adapter (no policy check).
    async fn fetch(&self, key: &str) -> Result<Secret, VaultError> {
        self.adapter.get_secret(key).await
    }

    /// Route an approval request through the relay; map decisions to errors.
    async fn require_approval(&self, key: &str, context: &str) -> Result<(), VaultError> {
        let decision = self.relay.request_approval(key, context).await?;
        match decision {
            ApprovalDecision::Approved => {
                info!(key, "vault: approval granted");
                Ok(())
            }
            ApprovalDecision::Denied => {
                warn!(key, "vault: approval denied");
                Err(VaultError::Denied(key.to_owned()))
            }
            ApprovalDecision::TimedOut => {
                warn!(key, "vault: approval timed out");
                Err(VaultError::TimedOut(key.to_owned()))
            }
        }
    }

    /// Check whether a cached approval is still valid.
    ///
    /// Returns `true` if an `Approved` decision exists in the cache and has
    /// not yet expired (per the `expires_at` set at cache time).
    async fn is_approved_cached(&self, key: &str) -> bool {
        let cache = self.approval_cache.lock().await;
        if let Some(entry) = cache.get(key) {
            if entry.decision == ApprovalDecision::Approved && entry.is_valid() {
                debug!(key, "vault: using cached approval");
                return true;
            }
        }
        false
    }

    /// Cache an `Approved` decision for `key`.
    async fn cache_approval(&self, key: &str, expires_at: Option<Instant>) {
        let mut cache = self.approval_cache.lock().await;
        cache.insert(
            key.to_owned(),
            CachedApproval {
                decision: ApprovalDecision::Approved,
                granted_at: Instant::now(),
                expires_at,
            },
        );
        debug!(key, "vault: approval decision cached");
    }

    /// Invalidate a cached approval (e.g. on session end or explicit revocation).
    pub async fn invalidate_approval(&self, key: &str) {
        let mut cache = self.approval_cache.lock().await;
        cache.remove(key);
        debug!(key, "vault: approval cache invalidated");
    }

    /// Clear all cached approvals (e.g. on session end).
    pub async fn clear_approval_cache(&self) {
        let mut cache = self.approval_cache.lock().await;
        cache.clear();
        debug!("vault: all approval caches cleared");
    }
}

impl std::fmt::Debug for VaultManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultManager")
            .field("adapter", &self.adapter.name())
            .field("backend", &self.config.backend)
            .finish_non_exhaustive()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{
        adapter::{NoopVaultAdapter, VaultResult},
        approval::{ApprovalDecision, NoopApprovalRelay},
        config::{SecretPolicyConfig, VaultBackend, VaultConfig, VaultSecretConfig},
        types::{Secret, SecretValue, SessionToken},
        VaultAdapter, VaultError,
    };
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    #[allow(unused_imports)]
    use tokio::sync::Mutex as TokioMutex;

    // ── Mock adapter ─────────────────────────────────────────────────────────

    struct MockAdapter {
        secrets: HashMap<String, String>,
        get_count: Arc<AtomicUsize>,
    }

    impl MockAdapter {
        fn new(secrets: HashMap<String, String>) -> Arc<Self> {
            Arc::new(Self {
                secrets,
                get_count: Arc::new(AtomicUsize::new(0)),
            })
        }
        fn get_count(self: &Arc<Self>) -> usize {
            self.get_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl VaultAdapter for MockAdapter {
        async fn get_secret(&self, key: &str) -> VaultResult<Secret> {
            self.get_count.fetch_add(1, Ordering::SeqCst);
            self.secrets
                .get(key)
                .map(|v| Secret::new(key, v.clone()))
                .ok_or_else(|| VaultError::UnknownKey(key.to_owned()))
        }

        async fn store_secret(&self, _key: &str, _value: SecretValue) -> VaultResult<()> {
            Ok(())
        }

        async fn unlock(&self) -> VaultResult<SessionToken> {
            Ok(SessionToken::new("mock-token", Duration::from_secs(3600)))
        }

        fn name(&self) -> &'static str {
            "mock"
        }
    }

    // ── Mock relay that always denies ────────────────────────────────────────

    struct DenyRelay;

    #[async_trait]
    impl ApprovalRelay for DenyRelay {
        async fn request_approval(
            &self,
            _key: &str,
            _context: &str,
        ) -> Result<ApprovalDecision, VaultError> {
            Ok(ApprovalDecision::Denied)
        }
    }

    // ── Mock relay that times out ────────────────────────────────────────────

    struct TimeoutRelay;

    #[async_trait]
    impl ApprovalRelay for TimeoutRelay {
        async fn request_approval(
            &self,
            _key: &str,
            _context: &str,
        ) -> Result<ApprovalDecision, VaultError> {
            Ok(ApprovalDecision::TimedOut)
        }
    }

    // ── Counting relay ───────────────────────────────────────────────────────

    struct CountingRelay {
        count: Arc<AtomicUsize>,
    }

    impl CountingRelay {
        fn new() -> (Arc<Self>, Arc<AtomicUsize>) {
            let count = Arc::new(AtomicUsize::new(0));
            (Arc::new(Self { count: count.clone() }), count)
        }
    }

    #[async_trait]
    impl ApprovalRelay for CountingRelay {
        async fn request_approval(
            &self,
            _key: &str,
            _context: &str,
        ) -> Result<ApprovalDecision, VaultError> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(ApprovalDecision::Approved)
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_config(
        key: &str,
        bw_item_id: &str,
        policy: SecretPolicyConfig,
        ttl_secs: Option<u64>,
    ) -> VaultConfig {
        let mut secrets = HashMap::new();
        secrets.insert(
            key.to_owned(),
            VaultSecretConfig {
                bw_item_id: bw_item_id.to_owned(),
                policy,
                ttl_secs,
            },
        );
        VaultConfig {
            backend: VaultBackend::BitwardenCli,
            bw_path: "bw".into(),
            session_ttl_secs: 3600,
            secrets,
        }
    }

    fn make_adapter() -> Arc<MockAdapter> {
        let mut m = HashMap::new();
        m.insert("anthropic_key".into(), "sk-ant-test".into());
        m.insert("stripe_key".into(), "sk-stripe-test".into());
        m.insert("deploy_key".into(), "ssh-rsa-test".into());
        MockAdapter::new(m)
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// Auto policy — no relay invocation, direct fetch.
    #[tokio::test]
    async fn auto_policy_no_approval_needed() {
        let cfg = make_config("anthropic_key", "anthropic-api-key", SecretPolicyConfig::Auto, None);
        let adapter = make_adapter();
        let (relay, relay_count) = CountingRelay::new();
        let mgr = VaultManager::new(adapter.clone(), relay, Arc::new(cfg));

        let secret = mgr.access_secret("anthropic_key").await.expect("should succeed");
        assert_eq!(secret.expose(), "sk-ant-test");
        assert_eq!(relay_count.load(Ordering::SeqCst), 0, "relay should NOT be called for auto policy");
        assert_eq!(adapter.get_count(), 1);
    }

    /// PerUse policy — relay called on every access.
    #[tokio::test]
    async fn per_use_policy_relay_called_every_time() {
        let cfg = make_config("stripe_key", "stripe-live-key", SecretPolicyConfig::PerUse, None);
        let (relay, relay_count) = CountingRelay::new();
        let adapter = make_adapter();
        let mgr = VaultManager::new(adapter.clone(), relay, Arc::new(cfg));

        for _ in 0..3 {
            mgr.access_secret("stripe_key").await.expect("should succeed");
        }
        assert_eq!(relay_count.load(Ordering::SeqCst), 3, "relay called once per access");
        assert_eq!(adapter.get_count(), 3);
    }

    /// Session policy — relay called once; subsequent accesses use cache.
    #[tokio::test]
    async fn session_policy_relay_called_once() {
        let cfg = make_config("stripe_key", "stripe-key", SecretPolicyConfig::Session, None);
        let (relay, relay_count) = CountingRelay::new();
        let adapter = make_adapter();
        let mgr = VaultManager::new(adapter.clone(), relay, Arc::new(cfg));

        for _ in 0..5 {
            mgr.access_secret("stripe_key").await.expect("should succeed");
        }
        assert_eq!(relay_count.load(Ordering::SeqCst), 1, "relay called only once for session policy");
        assert_eq!(adapter.get_count(), 5);
    }

    /// Session policy — relay invalidation forces re-approval.
    #[tokio::test]
    async fn session_policy_cache_invalidation_re_approves() {
        let cfg = make_config("stripe_key", "stripe-key", SecretPolicyConfig::Session, None);
        let (relay, relay_count) = CountingRelay::new();
        let adapter = make_adapter();
        let mgr = VaultManager::new(adapter.clone(), relay, Arc::new(cfg));

        mgr.access_secret("stripe_key").await.expect("first access");
        mgr.invalidate_approval("stripe_key").await;
        mgr.access_secret("stripe_key").await.expect("second access after invalidation");

        assert_eq!(relay_count.load(Ordering::SeqCst), 2, "relay called again after invalidation");
    }

    /// TimeBound policy — relay called once; after TTL expires, re-approves.
    #[tokio::test]
    async fn time_bound_policy_expires_and_re_approves() {
        let cfg = make_config(
            "deploy_key",
            "deploy-ssh-key",
            SecretPolicyConfig::TimeBound,
            Some(1), // 1 second TTL for testing
        );
        let (relay, relay_count) = CountingRelay::new();
        let adapter = make_adapter();
        let mgr = VaultManager::new(adapter.clone(), relay, Arc::new(cfg));

        // First access — should call relay.
        mgr.access_secret("deploy_key").await.expect("first access");
        assert_eq!(relay_count.load(Ordering::SeqCst), 1);

        // Invalidate cache manually (simulating TTL expiry without sleeping in tests).
        mgr.invalidate_approval("deploy_key").await;

        // Second access — should call relay again.
        mgr.access_secret("deploy_key").await.expect("second access after expiry");
        assert_eq!(relay_count.load(Ordering::SeqCst), 2);
    }

    /// Deny relay — returns `VaultError::Denied`.
    #[tokio::test]
    async fn deny_relay_returns_denied_error() {
        let cfg = make_config("stripe_key", "stripe-key", SecretPolicyConfig::PerUse, None);
        let relay = Arc::new(DenyRelay);
        let adapter = make_adapter();
        let mgr = VaultManager::new(adapter, relay, Arc::new(cfg));

        let err = mgr.access_secret("stripe_key").await.expect_err("should be denied");
        assert!(matches!(err, VaultError::Denied(_)));
    }

    /// Timeout relay — returns `VaultError::TimedOut`.
    #[tokio::test]
    async fn timeout_relay_returns_timed_out_error() {
        let cfg = make_config("stripe_key", "stripe-key", SecretPolicyConfig::PerUse, None);
        let relay = Arc::new(TimeoutRelay);
        let adapter = make_adapter();
        let mgr = VaultManager::new(adapter, relay, Arc::new(cfg));

        let err = mgr.access_secret("stripe_key").await.expect_err("should time out");
        assert!(matches!(err, VaultError::TimedOut(_)));
    }

    /// Unknown key — returns `VaultError::UnknownKey`.
    #[tokio::test]
    async fn unknown_key_returns_error() {
        let cfg = VaultConfig::default();
        let adapter = make_adapter();
        let relay = Arc::new(NoopApprovalRelay);
        let mgr = VaultManager::new(adapter, relay, Arc::new(cfg));

        let err = mgr.access_secret("does_not_exist").await.expect_err("should fail");
        assert!(matches!(err, VaultError::UnknownKey(_)));
    }

    /// Noop adapter returns NotConfigured.
    #[tokio::test]
    async fn noop_adapter_returns_not_configured() {
        let cfg = make_config("k", "id", SecretPolicyConfig::Auto, None);
        let adapter = Arc::new(NoopVaultAdapter);
        let relay = Arc::new(NoopApprovalRelay);
        let mgr = VaultManager::new(adapter, relay, Arc::new(cfg));

        let err = mgr.access_secret("k").await.expect_err("noop should fail");
        assert!(matches!(err, VaultError::NotConfigured));
    }

    /// clear_approval_cache clears all entries.
    #[tokio::test]
    async fn clear_approval_cache_forces_reapproval() {
        let cfg = make_config("stripe_key", "stripe-key", SecretPolicyConfig::Session, None);
        let (relay, relay_count) = CountingRelay::new();
        let adapter = make_adapter();
        let mgr = VaultManager::new(adapter.clone(), relay, Arc::new(cfg));

        mgr.access_secret("stripe_key").await.expect("first");
        mgr.clear_approval_cache().await;
        mgr.access_secret("stripe_key").await.expect("second after clear");

        assert_eq!(relay_count.load(Ordering::SeqCst), 2);
    }

    // ── Property tests (hegel) ────────────────────────────────────────────────

    /// Property: for any N accesses with `Auto` policy, relay is called 0 times.
    ///
    /// From opus-review-2.md §8: `Auto` policy → relay is NEVER invoked.
    /// This holds for arbitrary N ≥ 1, not just N=1 (the unit test checks N=1).
    /// An off-by-one bug in the policy dispatch could cause relay calls for
    /// large N or after cache expiry.
    #[hegel::test]
    fn prop_auto_policy_relay_never_called(tc: hegel::TestCase) {
        use hegel::generators as gs;
        use hegel::Generator;

        // Generate N accesses: 1 to 20.
        let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(20));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let cfg = make_config(
                "anthropic_key",
                "anthropic-api-key",
                SecretPolicyConfig::Auto,
                None,
            );
            let adapter = make_adapter();
            let (relay, relay_count) = CountingRelay::new();
            let mgr = VaultManager::new(adapter.clone(), relay, Arc::new(cfg));

            for _ in 0..n {
                mgr.access_secret("anthropic_key").await.expect("auto policy must succeed");
            }

            let calls = relay_count.load(Ordering::SeqCst);
            assert_eq!(
                calls, 0,
                "Auto policy: relay called {calls} times after {n} accesses (must be 0)"
            );
            // Adapter should be called exactly N times (no caching of fetches).
            assert_eq!(adapter.get_count(), n, "Auto policy: adapter should be called {n} times");
        });
    }

    /// Property: for any N accesses with `PerUse` policy, relay is called exactly N times.
    ///
    /// From opus-review-2.md §8: `PerUse` → relay called on every access.
    /// An off-by-one or caching bug could make relay_count < N.
    #[hegel::test]
    fn prop_per_use_policy_relay_called_n_times(tc: hegel::TestCase) {
        use hegel::generators as gs;
        use hegel::Generator;

        let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(20));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let cfg = make_config(
                "stripe_key",
                "stripe-live-key",
                SecretPolicyConfig::PerUse,
                None,
            );
            let adapter = make_adapter();
            let (relay, relay_count) = CountingRelay::new();
            let mgr = VaultManager::new(adapter.clone(), relay, Arc::new(cfg));

            for _ in 0..n {
                mgr.access_secret("stripe_key").await.expect("per-use must succeed");
            }

            let calls = relay_count.load(Ordering::SeqCst);
            assert_eq!(
                calls, n,
                "PerUse policy: relay called {calls} times after {n} accesses (must be {n})"
            );
        });
    }

    /// Property: for any N accesses with `Session` policy, relay is called exactly 1 time.
    ///
    /// From opus-review-2.md §8: `Session` → relay called once; subsequent accesses
    /// use the cache.  This must hold for N=1 AND N=100 — the cache must not expire
    /// prematurely.
    #[hegel::test]
    fn prop_session_policy_relay_called_once(tc: hegel::TestCase) {
        use hegel::generators as gs;
        use hegel::Generator;

        let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(20));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let cfg = make_config(
                "stripe_key",
                "stripe-key",
                SecretPolicyConfig::Session,
                None,
            );
            let adapter = make_adapter();
            let (relay, relay_count) = CountingRelay::new();
            let mgr = VaultManager::new(adapter.clone(), relay, Arc::new(cfg));

            for _ in 0..n {
                mgr.access_secret("stripe_key").await.expect("session policy must succeed");
            }

            let calls = relay_count.load(Ordering::SeqCst);
            assert_eq!(
                calls, 1,
                "Session policy: relay called {calls} times after {n} accesses (must be exactly 1)"
            );
            // All N fetches must succeed.
            assert_eq!(
                adapter.get_count(), n,
                "Session policy: adapter must be called {n} times"
            );
        });
    }

    /// Property: `Session` policy — after cache invalidation, relay is called again.
    ///
    /// Tests that M invalidations cause M+1 relay invocations for N accesses
    /// split across M+1 sessions.
    ///
    /// This is strictly stronger than the unit test which only does 1 invalidation:
    /// it tests arbitrary M invalidations and verifies the relay count equals M+1.
    #[hegel::test]
    fn prop_session_policy_invalidation_causes_reapproval(tc: hegel::TestCase) {
        use hegel::generators as gs;
        use hegel::Generator;

        // Number of invalidations (each resets the session cache).
        let invalidations = tc.draw(gs::integers::<usize>().min_value(1).max_value(5));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let cfg = make_config(
                "stripe_key",
                "stripe-key",
                SecretPolicyConfig::Session,
                None,
            );
            let adapter = make_adapter();
            let (relay, relay_count) = CountingRelay::new();
            let mgr = VaultManager::new(adapter.clone(), relay, Arc::new(cfg));

            // First access in first "session".
            mgr.access_secret("stripe_key").await.expect("first access");

            for _ in 0..invalidations {
                mgr.invalidate_approval("stripe_key").await;
                mgr.access_secret("stripe_key").await.expect("access after invalidation");
            }

            // Relay must have been called once for the initial access +
            // once per invalidation (total: invalidations + 1).
            let expected = invalidations + 1;
            let calls = relay_count.load(Ordering::SeqCst);
            assert_eq!(
                calls, expected,
                "Session policy with {invalidations} invalidations: \
                 relay called {calls} times (expected {expected})"
            );
        });
    }
}
