//! Shared application state, managed by Rocket and injected into routes via `&State<AppState>`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use crate::aggregation::catalog::Catalog;
use crate::config::Config;
use crate::documentdb::DocumentDb;
use crate::error::{AppError, AppResult};
use crate::foundry::FoundryManager;
use crate::ontology::binder::BindingOverrides;
use crate::ontology::binding::SchemaBinding;

/// Cheaply-cloneable handle to the per-source schema binding cache.
///
/// Passed to any code that may build or update a binding so the result is
/// immediately visible to route handlers without a server restart.
/// `Arc`-wrapped so it can be moved into background tasks that outlive
/// a request (ingest pipeline, catalog refresh).
pub type BindingCache = Arc<RwLock<HashMap<String, Arc<SchemaBinding>>>>;

/// Everything a route needs at runtime. Cheaply cloneable handles live here;
/// later workstreams add the connector registry and reranker.
pub struct AppState {
    pub config: Config,
    pub db: DocumentDb,
    /// `None` when Foundry Local failed to initialise (e.g. not installed). The
    /// server still boots and serves `/health`; Foundry routes report unavailable.
    /// `Arc`-wrapped so `foundry_handle()` can hand a background task (compaction,
    /// `memory.rs`) an owned, `'static` handle without cloning the manager itself.
    foundry: Option<Arc<FoundryManager>>,
    /// In-memory cache of persisted per-role model routing overrides (role key ->
    /// variant id), mirroring the `settings` collection. Read on every routed request
    /// via `spec_for`, so it's cached here rather than hitting DocumentDB per call;
    /// `set_router_override_cache` keeps it in sync after a write.
    router_overrides: RwLock<HashMap<String, String>>,
    /// Per-IP failed-login throttle with exponential backoff lock. Process-local;
    /// acceptable for the on-prem single-host deployment model.
    pub login_throttle: crate::auth::throttle::LoginThrottle,
    /// Live metadata catalog, rebuilt whenever ingest completes or records are deleted.
    ///
    /// The outer `Arc` makes the handle cheaply cloneable for EventStream! generators
    /// that cannot borrow `&AppState` across yields. The inner `Arc<Catalog>` lets
    /// readers clone the current snapshot under a microsecond read lock without holding
    /// the lock across any `.await`.
    catalog: Arc<RwLock<Arc<Catalog>>>,
    /// Tier-2 intent-route decision cache (bounded LRU + metrics). Cleared on every
    /// `set_catalog` — a schema change can flip a structured/semantic decision.
    /// `Arc` so detached work (the ingest pipeline, which owns cloned handles
    /// rather than `&AppState`) can invalidate routes after a schema change.
    pub router_cache: Arc<crate::router::RouterCache>,
    /// Process-local bounds for expensive inference, retrieval, and ingestion work.
    pub admission: crate::admission::AdmissionControl,
    /// Process-local notifications for live ingestion progress; snapshots remain durable in MongoDB.
    pub ingest_progress: crate::ingest::IngestProgressHub,
    /// Live pipeline-stage fan-out for `/chat` and `/agents`, keyed by the run id
    /// the client mints. Read by `GET /runs/<run_id>/progress`; see `progress.rs`
    /// for why the stages can't ride the answer stream itself.
    pub run_progress: crate::progress::RunProgressHub,
    /// 0 warming, 1 ready, 2 disabled, 3 failed.
    warmup_state: Arc<AtomicU8>,

    /// Per-source schema bindings (service-line ontology), keyed by `source_id`.
    /// Built on first `build_binding` call and updated on each catalog refresh.
    bindings: Arc<RwLock<HashMap<String, Arc<SchemaBinding>>>>,

    /// Per-source manual binding overrides, loaded from `schema_metadata_overrides`.
    binding_overrides: Arc<RwLock<HashMap<String, BindingOverrides>>>,
}

impl AppState {
    pub fn new(
        config: Config,
        db: DocumentDb,
        foundry: Option<FoundryManager>,
        router_overrides: HashMap<String, String>,
        initial_catalog: Catalog,
    ) -> Self {
        let router_cache = Arc::new(crate::router::RouterCache::new(
            config.router.router_cache_size,
        ));
        let admission = crate::admission::AdmissionControl::new(&config);
        let warmup_state = Arc::new(AtomicU8::new(if config.warmup_enabled { 0 } else { 2 }));
        AppState {
            config,
            db,
            foundry: foundry.map(Arc::new),
            router_overrides: RwLock::new(router_overrides),
            login_throttle: crate::auth::throttle::LoginThrottle::new(),
            catalog: Arc::new(RwLock::new(Arc::new(initial_catalog))),
            router_cache,
            admission,
            ingest_progress: crate::ingest::IngestProgressHub::default(),
            run_progress: crate::progress::RunProgressHub::default(),
            warmup_state,
            bindings: Arc::new(RwLock::new(HashMap::new())),
            binding_overrides: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn foundry_available(&self) -> bool {
        self.foundry.is_some()
    }

    pub fn warmup_handle(&self) -> Arc<AtomicU8> {
        self.warmup_state.clone()
    }

    pub fn warmup_status(&self) -> &'static str {
        match self.warmup_state.load(Ordering::Relaxed) {
            0 => "warming",
            1 => "ready",
            2 => "disabled",
            _ => "failed",
        }
    }

    /// Access the Foundry manager, or a clean 503 if it is not available.
    pub fn foundry(&self) -> AppResult<&FoundryManager> {
        self.foundry.as_deref().ok_or_else(|| {
            AppError::Unavailable("Foundry Local is not available on the server".into())
        })
    }

    /// An owned, cheaply-cloneable handle to the Foundry manager, for detached
    /// background tasks (e.g. `memory::maybe_spawn_compaction`) that outlive the
    /// request and can't borrow `&AppState`. `None` when Foundry is unavailable.
    pub fn foundry_handle(&self) -> Option<Arc<FoundryManager>> {
        self.foundry.clone()
    }

    /// The persisted override variant for a role key, if one is set.
    pub fn router_override(&self, role: &str) -> Option<String> {
        self.router_overrides
            .read()
            .ok()
            .and_then(|m| m.get(role).cloned())
    }

    /// Update the in-memory override cache after a successful DB write (or clear it).
    pub fn set_router_override_cache(&self, role: &str, variant_id: Option<String>) {
        if let Ok(mut m) = self.router_overrides.write() {
            match variant_id {
                Some(v) => {
                    m.insert(role.to_string(), v);
                }
                None => {
                    m.remove(role);
                }
            }
        }
    }

    /// `ModelSpec` for a model role, with any persisted per-role override applied over
    /// the `ONPREM_MODEL_*` env default. This is what every caller should use instead of
    /// `ModelSpec::for_role` directly.
    pub fn spec_for(
        &self,
        role: crate::foundry::router::ModelRole,
    ) -> crate::foundry::router::ModelSpec {
        let mut spec = crate::foundry::router::ModelSpec::for_role(role, &self.config);
        if let Some(v) = self.router_override(crate::foundry::router::override_key(role)) {
            spec.alias = v;
        }
        spec
    }

    /// Return a snapshot of the current catalog. Clones the inner `Arc` under a
    /// read lock (microsecond duration); the lock is released before any `.await`.
    pub fn catalog(&self) -> Arc<Catalog> {
        self.catalog.read().expect("catalog lock poisoned").clone()
    }

    /// Replace the catalog with a new version (called after ingest completes or
    /// records are deleted to keep the allow-list current).
    pub fn set_catalog(&self, cat: Catalog) {
        if let Ok(mut w) = self.catalog.write() {
            *w = Arc::new(cat);
            // Invalidate cached route decisions: the new schema may make a query that
            // was semantic now answerable structurally (or vice versa).
            self.router_cache.clear();
            tracing::info!("catalog updated; router cache cleared");
        }
    }

    /// Clone the catalog handle for use inside EventStream! generators, where
    /// `&AppState` cannot be borrowed across `yield` points.
    pub fn catalog_handle(&self) -> Arc<RwLock<Arc<Catalog>>> {
        self.catalog.clone()
    }

    /// Clone the router-cache handle for the same reason as `catalog_handle`:
    /// whoever replaces the catalog must also invalidate cached route decisions.
    pub fn router_cache_handle(&self) -> Arc<crate::router::RouterCache> {
        self.router_cache.clone()
    }

    // -----------------------------------------------------------------------
    // Schema binding accessors
    // -----------------------------------------------------------------------

    /// Clone the binding-cache `Arc` for background tasks (ingest pipeline, catalog
    /// refresh hooks) that outlive the request and cannot borrow `&AppState`.
    pub fn binding_cache(&self) -> BindingCache {
        self.bindings.clone()
    }

    /// Return a snapshot of all bindings (Arc clones, no copy of data).
    pub fn bindings(&self) -> HashMap<String, Arc<SchemaBinding>> {
        self.bindings
            .read()
            .expect("bindings lock poisoned")
            .clone()
    }

    /// Return the binding for one source (Arc clone), if cached.
    pub fn binding_for(&self, source_id: &str) -> Option<Arc<SchemaBinding>> {
        self.bindings
            .read()
            .expect("bindings lock poisoned")
            .get(source_id)
            .cloned()
    }

    /// Insert or replace the cached binding for a source.
    pub fn set_binding(&self, source_id: String, binding: Arc<SchemaBinding>) {
        if let Ok(mut w) = self.bindings.write() {
            w.insert(source_id, binding);
        }
    }

    /// Bulk-load bindings from a `Vec` (called at startup after DB load).
    pub fn set_all_bindings(&self, all: Vec<SchemaBinding>) {
        if let Ok(mut w) = self.bindings.write() {
            for b in all {
                w.insert(b.source_id.clone(), Arc::new(b));
            }
        }
    }

    /// Return manual binding overrides for a source, if any.
    pub fn binding_overrides_for(&self, source_id: &str) -> Option<BindingOverrides> {
        self.binding_overrides
            .read()
            .expect("binding_overrides lock poisoned")
            .get(source_id)
            .cloned()
    }

    /// Store manual binding overrides for a source.
    pub fn set_binding_overrides(&self, source_id: String, ov: BindingOverrides) {
        if let Ok(mut w) = self.binding_overrides.write() {
            w.insert(source_id, ov);
        }
    }
}
