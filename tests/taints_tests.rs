#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::{Node, NodeSpec, Taint};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use kube::api::{Api, DeleteParams, PostParams};
    use kube::Client;
    use rand::{distr::Alphanumeric, rng, Rng};
    use serde_json::json;

    /// Generate a random node name
    fn random_node_name(length: usize) -> String {
        assert!(length >= 1, "node name length must be ≥ 1");
        rng()
            .sample_iter(&Alphanumeric)
            .take(length)
            .map(char::from)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    }

    /// Create a node by name
    async fn create_node(client: &Client, node_name: &str) -> Result<(), anyhow::Error> {
        let nodes: Api<Node> = Api::all(client.clone());

        let node = Node {
            metadata: ObjectMeta {
                name: Some(node_name.to_string()),
                ..Default::default()
            },
            spec: Some(NodeSpec::default()),
            ..Default::default()
        };

        // Check if it already exists
        if nodes.get(node_name).await.is_ok() {
            return Ok(());
        }

        nodes.create(&PostParams::default(), &node).await?;
        wait_for_node(client, node_name, true).await?;
        Ok(())
    }

    /// Delete a node by name
    async fn delete_node(client: &Client, node_name: &str) -> Result<(), anyhow::Error> {
        let nodes: Api<Node> = Api::all(client.clone());
        nodes.delete(node_name, &DeleteParams::default()).await?;
        wait_for_node(client, node_name, false).await?;
        Ok(())
    }

    /// Add or update taints on a node
    async fn set_node_taints(
        client: &Client,
        node_name: &str,
        taints: Vec<Taint>,
    ) -> Result<(), anyhow::Error> {
        let nodes: Api<Node> = Api::all(client.clone());

        let patch = json!({
            "spec": {
                "taints": taints
            }
        });

        nodes
            .patch(
                node_name,
                &kube::api::PatchParams::default(),
                &kube::api::Patch::Merge(patch),
            )
            .await?;
        Ok(())
    }

    /// Poll until a node does or does not exist
    async fn wait_for_node(
        client: &Client,
        node_name: &str,
        should_exist: bool,
    ) -> Result<(), anyhow::Error> {
        let nodes: Api<Node> = Api::all(client.clone());
        let interval = std::time::Duration::from_millis(200);
        let timeout = std::time::Duration::from_secs(15);
        let start = std::time::Instant::now();
        loop {
            let exists = nodes.get(node_name).await.is_ok();
            if exists == should_exist {
                return Ok(());
            }
            if start.elapsed() > timeout {
                anyhow::bail!(
                    "Timeout waiting for node {} (should_exist: {})",
                    node_name,
                    should_exist
                );
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Poll until a node has specific taints
    async fn wait_for_taint(
        client: &Client,
        node_name: &str,
        taint_key: &str,
        should_exist: bool,
    ) -> Result<(), anyhow::Error> {
        let nodes: Api<Node> = Api::all(client.clone());
        let interval = std::time::Duration::from_millis(500);
        let timeout = std::time::Duration::from_secs(15);
        let start = std::time::Instant::now();
        loop {
            if let Ok(node) = nodes.get(node_name).await {
                let has_taint = node
                    .spec
                    .as_ref()
                    .and_then(|spec| spec.taints.as_ref())
                    .map(|taints| taints.iter().any(|t| t.key == taint_key))
                    .unwrap_or(false);

                if has_taint == should_exist {
                    return Ok(());
                }
            }
            if start.elapsed() > timeout {
                let node = nodes.get(node_name).await.ok();
                anyhow::bail!(
                    "Timeout waiting for taint '{}' on node '{}' (should_exist: {}). Current taints: {:?}",
                    taint_key,
                    node_name,
                    should_exist,
                    node.and_then(|n| n.spec).and_then(|s| s.taints)
                );
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Wait for a taint with specific value
    async fn wait_for_taint_value(
        client: &Client,
        node_name: &str,
        taint_key: &str,
        expected_value: Option<&str>,
    ) -> Result<(), anyhow::Error> {
        let nodes: Api<Node> = Api::all(client.clone());
        let interval = std::time::Duration::from_millis(500);
        let timeout = std::time::Duration::from_secs(15);
        let start = std::time::Instant::now();
        loop {
            if let Ok(node) = nodes.get(node_name).await {
                if let Some(taints) = node.spec.as_ref().and_then(|spec| spec.taints.as_ref()) {
                    if let Some(taint) = taints.iter().find(|t| t.key == taint_key) {
                        if taint.value.as_deref() == expected_value {
                            return Ok(());
                        }
                    }
                }
            }
            if start.elapsed() > timeout {
                let node = nodes.get(node_name).await.ok();
                anyhow::bail!(
                    "Timeout waiting for taint '{}' with value '{:?}' on node '{}'. Current taints: {:?}",
                    taint_key,
                    expected_value,
                    node_name,
                    node.and_then(|n| n.spec).and_then(|s| s.taints)
                );
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Test 1: Restore on cycle
    #[tokio::test]
    async fn test_restore_on_cycle() {
        let client = Client::try_default().await.unwrap();
        let node_name = format!("test-restore-{}", random_node_name(10));

        // Create node
        create_node(&client, &node_name).await.unwrap();

        // Add custom taint
        let custom_taint = Taint {
            key: "custom.example.com/test".to_string(),
            value: Some("testvalue".to_string()),
            effect: "NoSchedule".to_string(),
            ..Default::default()
        };
        set_node_taints(&client, &node_name, vec![custom_taint.clone()])
            .await
            .unwrap();
        wait_for_taint(&client, &node_name, &custom_taint.key, true)
            .await
            .unwrap();

        // Delete and recreate node
        delete_node(&client, &node_name).await.unwrap();
        create_node(&client, &node_name).await.unwrap();

        // Verify taint is restored
        wait_for_taint(&client, &node_name, &custom_taint.key, true)
            .await
            .unwrap();

        // Cleanup
        delete_node(&client, &node_name).await.ok();
    }

    /// Test 2: No overwrite
    #[tokio::test]
    async fn test_no_overwrite() {
        let client = Client::try_default().await.unwrap();
        let node_name = format!("test-nooverwrite-{}", random_node_name(10));

        // Create node with taint
        create_node(&client, &node_name).await.unwrap();
        let taint_key = "custom.example.com/overwrite";
        let original_taint = Taint {
            key: taint_key.to_string(),
            value: Some("original".to_string()),
            effect: "NoSchedule".to_string(),
            ..Default::default()
        };
        set_node_taints(&client, &node_name, vec![original_taint.clone()])
            .await
            .unwrap();

        // Delete node (stores taint)
        delete_node(&client, &node_name).await.unwrap();

        // Recreate with different value for same taint key
        let nodes: Api<Node> = Api::all(client.clone());
        let new_taint = Taint {
            key: taint_key.to_string(),
            value: Some("new".to_string()),
            effect: "NoSchedule".to_string(),
            ..Default::default()
        };
        let node = Node {
            metadata: ObjectMeta {
                name: Some(node_name.clone()),
                ..Default::default()
            },
            spec: Some(NodeSpec {
                taints: Some(vec![new_taint.clone()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        nodes.create(&PostParams::default(), &node).await.unwrap();

        // Verify the new value is NOT overwritten
        wait_for_taint_value(&client, &node_name, taint_key, Some("new"))
            .await
            .unwrap();

        // Cleanup
        delete_node(&client, &node_name).await.ok();
    }

    /// Test 3: Respect deletions
    #[tokio::test]
    async fn test_respect_deletions() {
        let client = Client::try_default().await.unwrap();
        let node_name = format!("test-deletions-{}", random_node_name(10));

        // Create node with taint
        create_node(&client, &node_name).await.unwrap();
        let taint_key = "custom.example.com/delete";
        let taint = Taint {
            key: taint_key.to_string(),
            value: Some("value".to_string()),
            effect: "NoSchedule".to_string(),
            ..Default::default()
        };
        set_node_taints(&client, &node_name, vec![taint.clone()])
            .await
            .unwrap();
        wait_for_taint(&client, &node_name, taint_key, true)
            .await
            .unwrap();

        // Remove the taint
        set_node_taints(&client, &node_name, vec![]).await.unwrap();
        wait_for_taint(&client, &node_name, taint_key, false)
            .await
            .unwrap();

        // Delete and recreate node
        delete_node(&client, &node_name).await.unwrap();
        create_node(&client, &node_name).await.unwrap();

        // Give controller time to reconcile
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        // Verify taint is NOT restored
        let nodes: Api<Node> = Api::all(client.clone());
        let node = nodes.get(&node_name).await.unwrap();
        let has_taint = node
            .spec
            .as_ref()
            .and_then(|spec| spec.taints.as_ref())
            .map(|taints| taints.iter().any(|t| t.key == taint_key))
            .unwrap_or(false);
        assert!(!has_taint, "Taint should not be restored after deletion");

        // Cleanup
        delete_node(&client, &node_name).await.ok();
    }

    /// Test 4: No system taints restored
    #[tokio::test]
    async fn test_no_system_taints_restored() {
        let client = Client::try_default().await.unwrap();
        let node_name = format!("test-system-{}", random_node_name(10));

        // Create node with both system and custom taints
        create_node(&client, &node_name).await.unwrap();
        let system_taint = Taint {
            key: "node.kubernetes.io/not-ready".to_string(),
            value: None,
            effect: "NoSchedule".to_string(),
            ..Default::default()
        };
        let custom_taint = Taint {
            key: "custom.example.com/test".to_string(),
            value: Some("value".to_string()),
            effect: "NoSchedule".to_string(),
            ..Default::default()
        };
        set_node_taints(
            &client,
            &node_name,
            vec![system_taint.clone(), custom_taint.clone()],
        )
        .await
        .unwrap();

        // Delete and recreate
        delete_node(&client, &node_name).await.unwrap();
        create_node(&client, &node_name).await.unwrap();

        // Verify only custom taint is restored
        wait_for_taint(&client, &node_name, &custom_taint.key, true)
            .await
            .unwrap();

        // Verify system taint is NOT restored
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let nodes: Api<Node> = Api::all(client.clone());
        let node = nodes.get(&node_name).await.unwrap();
        let has_system_taint = node
            .spec
            .as_ref()
            .and_then(|spec| spec.taints.as_ref())
            .map(|taints| taints.iter().any(|t| t.key == system_taint.key))
            .unwrap_or(false);
        assert!(!has_system_taint, "System taint should not be restored");

        // Cleanup
        delete_node(&client, &node_name).await.ok();
    }

    /// Test 5: Empty taints roundtrip
    #[tokio::test]
    async fn test_empty_taints_roundtrip() {
        let client = Client::try_default().await.unwrap();
        let node_name = format!("test-empty-{}", random_node_name(10));

        // Create node with no taints
        create_node(&client, &node_name).await.unwrap();

        // Delete and recreate
        delete_node(&client, &node_name).await.unwrap();
        create_node(&client, &node_name).await.unwrap();

        // Give controller time to reconcile
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        // Verify still no taints
        let nodes: Api<Node> = Api::all(client.clone());
        let node = nodes.get(&node_name).await.unwrap();
        let taints = node
            .spec
            .as_ref()
            .and_then(|spec| spec.taints.as_ref())
            .cloned()
            .unwrap_or_default();
        assert!(taints.is_empty(), "Node should have no taints");

        // Cleanup
        delete_node(&client, &node_name).await.ok();
    }

    /// Test 6: Idempotence
    #[tokio::test]
    async fn test_idempotence() {
        let client = Client::try_default().await.unwrap();
        let node_name = format!("test-idempotent-{}", random_node_name(10));

        // Create node with taint
        create_node(&client, &node_name).await.unwrap();
        let taint = Taint {
            key: "custom.example.com/idempotent".to_string(),
            value: Some("value".to_string()),
            effect: "NoSchedule".to_string(),
            ..Default::default()
        };
        set_node_taints(&client, &node_name, vec![taint.clone()])
            .await
            .unwrap();

        // Delete and recreate
        delete_node(&client, &node_name).await.unwrap();
        create_node(&client, &node_name).await.unwrap();

        // Wait for taint restoration
        wait_for_taint(&client, &node_name, &taint.key, true)
            .await
            .unwrap();

        // Wait for annotation
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Check annotation is present
        let nodes: Api<Node> = Api::all(client.clone());
        let node = nodes.get(&node_name).await.unwrap();
        assert!(
            node.metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("nodetaintpreserver.example.com/taints-restored"))
                .is_some(),
            "Idempotence annotation should be present"
        );

        // Cleanup
        delete_node(&client, &node_name).await.ok();
    }
}
