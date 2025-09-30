Within a Kubernetes cluster, nodes are often added/deleted as they undergo maintenance with cloud providers. When this happens, metadata stored in the Kubernetes Node object is lost. This is particularly problematic for custom node taints that administrators use to control workload placement on specific nodes.

This service preserves custom Node taints when nodes are deleted from the cluster and re-applies them when nodes return to the cluster. The controller is stateless but uses Kubernetes ConfigMaps for state storage.

## Features
- **Automatic Taint Preservation**: Captures custom taints before node deletion
- **Merge-Only Restoration**: Restores taints without overwriting existing ones
- **Protected Taint Filtering**: Never touches system taints (e.g., `node.kubernetes.io/*`)
- **Idempotent Operations**: Uses annotations to avoid redundant reconciliation
- **Observability**: Structured logging, Prometheus metrics, and Kubernetes Events
- **Production-Ready**: Exponential backoff, finalizer timeout protection, non-root container

## Architecture
The controller uses a finalizer-based approach:
1. **On Node Creation/Update (Apply)**: 
   - Check for stored taints in ConfigMap
   - Merge missing taints onto the node (no overwrites)
   - Mark node with restoration annotation for idempotence
   - Emit Kubernetes Event with restored taint keys

2. **On Node Deletion (Cleanup)**:
   - Extract custom taints (filter out protected ones)
   - Serialize to JSON and store in ConfigMap
   - Create empty ConfigMap if no custom taints (prevents stale restoration)

## Assumptions
- **Merge Strategy**: If a node is recreated with specific taints already set, we assume those are the latest and do not overwrite them. Only taints missing by key are added.
- **Storage Limits**: All taints for a single node fit within a ConfigMap (1MB limit).
- **Protected Taints**: System taints matching these patterns are never stored or restored:
  - `node.kubernetes.io/*`
  - `node.cloudprovider.kubernetes.io/*`
  - `node-role.kubernetes.io/*`
  - `CriticalAddonsOnly`
- **Finalizer Timeout**: If cleanup fails repeatedly for >1 hour, the finalizer is removed to prevent indefinite blocking.

## Configuration
Environment variables:
- `CONFIGMAP_NAMESPACE` (default: `default`) - Namespace for ConfigMap storage
- `RUST_LOG` (default: `info,kube=warn`) - Log level
- `EXTRA_PROTECTED_TAINT_PREFIXES` (optional) - Comma-separated list of additional taint prefixes to protect (e.g., `myorg.com/,internal.company.io/`)

## Deploy and Run Tests
### Prerequisites
- Install Rust: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- [Install minikube](https://minikube.sigs.k8s.io/docs/start/)
- [Install Docker](https://docs.docker.com/engine/install/)

### Build, Deploy Locally, and Test
```bash
./test.sh
```

This script will:
1. Start minikube (if not running)
2. Build the Docker image inside minikube
3. Apply RBAC, ServiceAccount, and Deployment
4. Wait for the controller to be ready
5. Run integration tests

## Dev Loop Setup
```bash
minikube start
cargo run
# In another terminal:
cargo test
```

## Observability

### Logs
Structured logs with node name and action phase (Apply/Cleanup):
```
INFO Reconciling node 'worker-1' (Apply)
INFO Stored 2 custom taints for node 'worker-1'
```

### Metrics (Prometheus)
- `taints_restored_total{node, key}` - Counter of taints restored per node/key
- `nodes_reconciled_total{phase}` - Counter of reconciliations by phase (apply/cleanup)
- `errors_total{kind, reason}` - Counter of errors by type

### Kubernetes Events
Events are created on Node resources:
- **TaintsRestored**: Lists taint keys that were restored (truncated if >5)
- **NoTaintsToRestore**: Emitted when no restoration was needed

## Testing
The integration test suite covers:
1. **Restore on cycle**: Taint preserved through delete/recreate
2. **No overwrite**: Existing taints on recreated nodes are not modified
3. **Respect deletions**: Taints removed before node deletion are not restored
4. **No system taints restored**: Protected taints are filtered out
5. **Empty taints roundtrip**: Nodes with no taints remain taint-free
6. **Idempotence**: Multiple reconciliations don't cause repeated patches

Run tests:
```bash
cargo test
```

## Further Work
- **High Availability**: Implement leader election for multiple controller replicas
- **Horizontal Scaling**: Shard workload by namespace or label selector
- **Garbage Collection**: Clean up ConfigMaps for permanently deleted nodes
- **Enhanced Metrics**: Add reconciliation latency histograms
- **Admission Webhook**: Validate taint changes before they're applied
- **Backup/Restore**: Export/import ConfigMaps for disaster recovery
- **Batch Operations**: Rate limit or batch reconciliations for large clusters
- **Additional Tests**:
  - Controller crash recovery
  - Mass node deletion (5000+ nodes)
  - ConfigMap tampering (invalid JSON, deletion)
  - Network partition scenarios

## Security
- Runs as non-root user (UID/GID: nonroot)
- Minimal RBAC permissions (Nodes, ConfigMaps, Events)
- No CRDs or cluster-wide write access to other resources

## Storage Schema
ConfigMap name: `node-taints-{sha256(nodeName)}`
- **Namespace**: Configurable via `CONFIGMAP_NAMESPACE`
- **Annotations**: 
  - `nodetaintpreserver.example.com/node-name: <nodeName>`
- **Data**:
  - `preserved_taints_json`: JSON array of `{"key": "...", "value": "...", "effect": "..."}`

## License
MIT

