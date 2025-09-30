use k8s_openapi::{
    api::core::v1::{ConfigMap, Event, Node, ObjectReference, Taint},
    apimachinery::pkg::apis::meta::v1::{ObjectMeta, Time},
};
use kube::{
    api::{Api, Patch, PatchParams, PostParams, ResourceExt},
    error::ErrorResponse,
    runtime::{
        controller::Action,
        finalizer::{finalizer, Event as FinalizerEvent},
    },
    Client,
};
use lazy_static::lazy_static;
use prometheus::{IntCounterVec, Opts, Registry};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::{Duration, SystemTime},
};
use thiserror::Error;
use tracing::{debug, error, info, warn};

const FINALIZER_NAME: &str = "nodetaintpreserver.example.com/finalizer";
const SERVICE_NAME: &str = "node-taint-preserver";
const JSON_STORAGE_KEY: &str = "preserved_taints_json";
const RESTORED_ANNOTATION_KEY: &str = "nodetaintpreserver.example.com/taints-restored";
const CONFIGMAP_NODE_ANNOTATION: &str = "nodetaintpreserver.example.com/node-name";
const REQUEUE_TIME: Duration = Duration::from_secs(2);
const MAX_RETRY_TIME: Duration = Duration::from_secs(3600);

// Protected taint prefixes that should never be stored or restored
const PROTECTED_TAINT_PREFIXES: &[&str] = &[
    "node.kubernetes.io/",
    "node.cloudprovider.kubernetes.io/",
    "node-role.kubernetes.io/",
];
const PROTECTED_TAINT_KEYS: &[&str] = &["CriticalAddonsOnly"];

lazy_static! {
    pub static ref PROMETHEUS_REGISTRY: Registry = Registry::new();
    static ref TAINTS_RESTORED_TOTAL: IntCounterVec = IntCounterVec::new(
        Opts::new("taints_restored_total", "Total number of taints restored"),
        &["node", "key"]
    )
    .unwrap();
    static ref NODES_RECONCILED_TOTAL: IntCounterVec = IntCounterVec::new(
        Opts::new("nodes_reconciled_total", "Total number of nodes reconciled"),
        &["phase"]
    )
    .unwrap();
    static ref ERRORS_TOTAL: IntCounterVec = IntCounterVec::new(
        Opts::new("errors_total", "Total number of errors"),
        &["kind", "reason"]
    )
    .unwrap();
}

/// Initialize Prometheus metrics
pub fn init_metrics() {
    PROMETHEUS_REGISTRY
        .register(Box::new(TAINTS_RESTORED_TOTAL.clone()))
        .ok();
    PROMETHEUS_REGISTRY
        .register(Box::new(NODES_RECONCILED_TOTAL.clone()))
        .ok();
    PROMETHEUS_REGISTRY
        .register(Box::new(ERRORS_TOTAL.clone()))
        .ok();
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to get node name: {0:?}")]
    MissingNodeName(Box<Node>),
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Finalizer error: {0}")]
    Finalizer(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Passed to the reconciler
pub struct Context {
    client: Client,
    configmap_namespace: String,
    extra_protected_prefixes: Vec<String>,
    attempt: AtomicU32,
}

impl Context {
    /// Create a new Context
    pub fn new(client: Client) -> Self {
        let configmap_namespace =
            std::env::var("CONFIGMAP_NAMESPACE").unwrap_or_else(|_| "default".to_string());
        let extra_protected_prefixes = std::env::var("EXTRA_PROTECTED_TAINT_PREFIXES")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().to_string())
            .collect();

        init_metrics();

        Self {
            client,
            configmap_namespace,
            extra_protected_prefixes,
            attempt: AtomicU32::new(0),
        }
    }

    fn cm_api(&self) -> Api<ConfigMap> {
        Api::<ConfigMap>::namespaced(self.client.clone(), &self.configmap_namespace)
    }
}

/// Generates the expected ConfigMap name for a given node name.
/// We hash the node name to a fixed length to ensure our ConfigMap
/// name is not longer than Kubernetes' key character limit.
fn configmap_name(node_name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(node_name.as_bytes());
    let full_hash = hasher.finalize();
    let hex_encoded_hash = hex::encode(full_hash);
    format!("node-taints-{}", hex_encoded_hash)
}

/// Check if a taint is protected and should not be stored/restored
fn is_taint_protected(taint: &Taint, extra_prefixes: &[String]) -> bool {
    let key = &taint.key;

    // Check against protected keys
    if PROTECTED_TAINT_KEYS.contains(&key.as_str()) {
        return true;
    }

    // Check against protected prefixes
    for prefix in PROTECTED_TAINT_PREFIXES {
        if key.starts_with(prefix) {
            return true;
        }
    }

    // Check against extra protected prefixes
    for prefix in extra_prefixes {
        if key.starts_with(prefix) {
            return true;
        }
    }

    false
}

/// Filter out protected taints from a list
fn filter_protected_taints(taints: Vec<Taint>, extra_prefixes: &[String]) -> Vec<Taint> {
    taints
        .into_iter()
        .filter(|t| !is_taint_protected(t, extra_prefixes))
        .collect()
}

/// Action to take on Node events
pub async fn reconcile(node: Arc<Node>, ctx: Arc<Context>) -> Result<Action> {
    let node_name = node
        .metadata
        .name
        .as_deref()
        .ok_or_else(|| Error::MissingNodeName(Box::new(node.as_ref().clone())))?
        .to_string();
    let node_api: Api<Node> = Api::all(ctx.client.clone());

    finalizer(&node_api, FINALIZER_NAME, node, |event| async {
        match event {
            FinalizerEvent::Apply(node) => apply_node(node, ctx.clone()).await,
            FinalizerEvent::Cleanup(node) => cleanup_node(node, ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| {
        warn!("Finalizer error for node {}: {:?}", node_name, e);
        ERRORS_TOTAL
            .with_label_values(&["finalizer", "finalizer_error"])
            .inc();
        Error::Finalizer(e.to_string())
    })
}

/// Handle Node Creation/Update
async fn apply_node(node: Arc<Node>, ctx: Arc<Context>) -> Result<Action> {
    let node_name = node.name_any();

    // Check if already processed (idempotence)
    if node.annotations().contains_key(RESTORED_ANNOTATION_KEY) {
        return Ok(Action::await_change());
    }

    info!("Reconciling node '{}' (Apply)", node_name);
    NODES_RECONCILED_TOTAL.with_label_values(&["apply"]).inc();

    let node_api: Api<Node> = Api::all(ctx.client.clone());
    let current_taints = node
        .spec
        .as_ref()
        .and_then(|spec| spec.taints.clone())
        .unwrap_or_default();

    let mut taints_to_restore: Vec<Taint> = Vec::new();

    // Check ConfigMap for preserved taints
    let cm_name = configmap_name(&node_name);
    match ctx.cm_api().get(&cm_name).await {
        Ok(cm) => {
            if let Some(data) = &cm.data {
                if let Some(taints_json_str) = data.get(JSON_STORAGE_KEY) {
                    taints_to_restore =
                        serde_json::from_str(taints_json_str).map_err(Error::Serialization)?;
                }
            }
        }
        Err(kube::Error::Api(ErrorResponse { code: 404, .. })) => {
            debug!("No ConfigMap found for node '{}'", node_name);
        }
        Err(e) => {
            ERRORS_TOTAL
                .with_label_values(&["configmap", "get_error"])
                .inc();
            return Err(Error::Kube(e));
        }
    }

    // Merge taints: only add if key doesn't exist
    let mut merged_taints = current_taints.clone();
    let mut restored_keys: Vec<String> = Vec::new();

    for taint in taints_to_restore {
        let exists = merged_taints.iter().any(|t| t.key == taint.key);
        if !exists {
            restored_keys.push(taint.key.clone());
            merged_taints.push(taint.clone());
            TAINTS_RESTORED_TOTAL
                .with_label_values(&[&node_name, &taint.key])
                .inc();
        }
    }

    // Only patch if we actually restored taints or need to add annotation
    if !restored_keys.is_empty() || !node.annotations().contains_key(RESTORED_ANNOTATION_KEY) {
        let mut node_spec = node.spec.clone().unwrap_or_default();
        node_spec.taints = if merged_taints.is_empty() {
            None
        } else {
            Some(merged_taints)
        };

        let mut annotations = node.annotations().clone();
        annotations.insert(RESTORED_ANNOTATION_KEY.to_string(), "1".to_string());

        let patch_payload = serde_json::json!({
            "metadata": {
                "annotations": annotations
            },
            "spec": {
                "taints": node_spec.taints
            }
        });

        let patch_params = PatchParams::apply(SERVICE_NAME).force();
        node_api
            .patch(&node_name, &patch_params, &Patch::Apply(&patch_payload))
            .await
            .map_err(Error::Kube)?;

        // Emit Kubernetes Event
        if !restored_keys.is_empty() {
            let message = if restored_keys.len() <= 5 {
                format!("Restored taints: {}", restored_keys.join(", "))
            } else {
                format!(
                    "Restored {} taints: {} ... (truncated)",
                    restored_keys.len(),
                    restored_keys[..5].join(", ")
                )
            };
            emit_event(&ctx, &node_name, "TaintsRestored", &message, "Normal").await;
            info!("Node '{}': {}", node_name, message);
        } else {
            emit_event(
                &ctx,
                &node_name,
                "NoTaintsToRestore",
                "No taints needed to be restored",
                "Normal",
            )
            .await;
        }
    }

    Ok(Action::await_change())
}

/// Handle Node Deletion
async fn cleanup_node(node: Arc<Node>, ctx: Arc<Context>) -> Result<Action> {
    let node_name = node.name_any();
    info!("Cleaning up node '{}' (Cleanup)", node_name);
    NODES_RECONCILED_TOTAL.with_label_values(&["cleanup"]).inc();

    // Check if deletion has been pending for too long
    if let Some(Time(deletion_time)) = node.metadata.deletion_timestamp {
        let current_time = SystemTime::now();
        let deletion_system_time: SystemTime = deletion_time.into();
        if current_time
            .duration_since(deletion_system_time)
            .unwrap_or_default()
            > MAX_RETRY_TIME
        {
            warn!(
                "Node '{}' termination cleanup failed for over {}s. Forcing finalizer removal.",
                node_name,
                MAX_RETRY_TIME.as_secs()
            );
            ERRORS_TOTAL
                .with_label_values(&["cleanup", "timeout"])
                .inc();
            return Ok(Action::await_change());
        }
    }

    // Get current taints
    let all_taints = node
        .spec
        .as_ref()
        .and_then(|spec| spec.taints.clone())
        .unwrap_or_default();

    // Filter out protected taints
    let taints_to_preserve = filter_protected_taints(all_taints, &ctx.extra_protected_prefixes);

    debug!(
        "Taints to preserve for node '{}': {:?}",
        node_name, taints_to_preserve
    );

    let cm_name = configmap_name(&node_name);
    let mut cm_data = BTreeMap::new();

    // Always write ConfigMap, even if empty, to avoid restoring stale taints
    if !taints_to_preserve.is_empty() {
        let taints_json =
            serde_json::to_string(&taints_to_preserve).map_err(Error::Serialization)?;
        cm_data.insert(JSON_STORAGE_KEY.to_string(), taints_json);
    }

    let mut cm_annotations = BTreeMap::new();
    cm_annotations.insert(CONFIGMAP_NODE_ANNOTATION.to_string(), node_name.clone());

    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name.clone()),
            namespace: Some(ctx.configmap_namespace.clone()),
            annotations: Some(cm_annotations),
            ..Default::default()
        },
        data: Some(cm_data),
        binary_data: None,
        immutable: None,
    };

    let patch_params = PatchParams::apply(SERVICE_NAME).force();
    ctx.cm_api()
        .patch(&cm_name, &patch_params, &Patch::Apply(&cm))
        .await
        .map_err(|e| {
            ERRORS_TOTAL
                .with_label_values(&["configmap", "patch_error"])
                .inc();
            Error::Kube(e)
        })?;

    info!(
        "Stored {} custom taints for node '{}'",
        taints_to_preserve.len(),
        node_name
    );

    Ok(Action::await_change())
}

/// Emit a Kubernetes Event
async fn emit_event(ctx: &Context, node_name: &str, reason: &str, message: &str, event_type: &str) {
    let events_api: Api<Event> = Api::namespaced(ctx.client.clone(), &ctx.configmap_namespace);
    let now = SystemTime::now();
    let timestamp = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let event_name = format!("{}.{}", node_name, timestamp);

    let event = Event {
        metadata: ObjectMeta {
            name: Some(event_name),
            namespace: Some(ctx.configmap_namespace.clone()),
            ..Default::default()
        },
        involved_object: ObjectReference {
            api_version: Some("v1".to_string()),
            kind: Some("Node".to_string()),
            name: Some(node_name.to_string()),
            ..Default::default()
        },
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
        type_: Some(event_type.to_string()),
        action: Some("Reconcile".to_string()),
        reporting_component: Some(SERVICE_NAME.to_string()),
        reporting_instance: Some(
            std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string()),
        ),
        ..Default::default()
    };

    if let Err(e) = events_api.create(&PostParams::default(), &event).await {
        warn!("Failed to create event: {:?}", e);
    }
}

/// Exponential backoff on error
pub fn error_policy(_node: Arc<Node>, error: &Error, ctx: Arc<Context>) -> Action {
    error!("Reconciliation failed: {:?}", error);
    let attempt = ctx.attempt.fetch_add(1, Ordering::SeqCst) + 1;
    let base_secs = REQUEUE_TIME.as_secs();
    let max_secs = MAX_RETRY_TIME.as_secs();
    let factor = 2u64.checked_pow(attempt).unwrap_or(u64::MAX);
    let delay_s = base_secs.saturating_mul(factor).min(max_secs);
    Action::requeue(Duration::from_secs(delay_s))
}
