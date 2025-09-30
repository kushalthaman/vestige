use futures::stream::StreamExt;
use k8s_openapi::api::core::v1::Node;
use kube::{
    api::Api,
    runtime::{controller::Controller, watcher},
    Client,
};
use node_taint_preserver::{error_policy, reconcile, Context};
use std::sync::Arc;
use tracing::{info, warn};
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = tracing_subscriber::filter::Targets::new()
        .with_target("node_taint_preserver", tracing::Level::DEBUG);
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(filter)
        .init();

    let client = Client::try_default().await?;
    let node_api: Api<Node> = Api::all(client.clone());
    let context = Arc::new(Context::new(client.clone()));

    let configmap_namespace =
        std::env::var("CONFIGMAP_NAMESPACE").unwrap_or_else(|_| "default".to_string());
    info!(
        "Starting Node Taint Preserver controller, storing in namespace {}...",
        configmap_namespace
    );

    Controller::new(node_api, watcher::Config::default())
        .run(reconcile, error_policy, context)
        .for_each(|res| async move {
            match res {
                Ok((obj, _action)) => info!("Reconciled Node '{}'", obj.name),
                Err(e) => warn!("Reconciliation error: {:?}", e),
            }
        })
        .await;
    Ok(())
}
