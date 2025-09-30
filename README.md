Within a Kubernetes cluster, nodes are often added/deleted as they undergo maintenance with cloud providers. When this happens, metadata stored in the Kubernetes Node object is lost. This is particularly problematic for custom node taints that administrators use to control workload placement on specific nodes.

This service preserves custom Node taints when nodes are deleted from the cluster and re-applies them when nodes return to the cluster. The controller is stateless but uses Kubernetes ConfigMaps for state storage.

## assumptions
- If a node is recreated with specific taints already set, we assume those are the latest and do not overwrite them. Only taints missing by key are added.
- All taints for a single node fit within a ConfigMap, with a 1MB limit.
- System taints matching these patterns are never stored or restored:
  - `node.kubernetes.io/*`
  - `node.cloudprovider.kubernetes.io/*`
  - `node-role.kubernetes.io/*`
  - `CriticalAddonsOnly`
- If cleanup fails repeatedly for over an hour, the finalizer is removed to prevent indefinite blocking.

## features
-  Captures custom taints before node deletion
-  Restores taints without overwriting existing ones
-  Never touches system taints (eg `node.kubernetes.io/*`)
-  Uses annotations to avoid redundant reconciliation
-  Structured logging, Prometheus metrics, and k8s Events
-  Exponential backoff, finalizer timeout protection, non-root container

## config
Env variables:
- `CONFIGMAP_NAMESPACE` (default: `default`) - Namespace for ConfigMap storage
- `RUST_LOG` (default: `info,kube=warn`) - log level
- `EXTRA_PROTECTED_TAINT_PREFIXES` (optional) - list of additional taint prefixes to protect (e.g., `myorg.com/,internal.company.io/`)

## deploy & run tests
### prerequisites
- Install Rust: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- [Install minikube](https://minikube.sigs.k8s.io/docs/start/)
- [Install Docker](https://docs.docker.com/engine/install/)

### build and deploy locally 
```bash
./test.sh
```

this will:
1. Start minikube (if not running)
2. Build the Docker image inside minikube
3. Apply RBAC, ServiceAccount, and Deployment
4. Wait for the controller to be ready
5. Run integration tests

## dev loop setup
```bash
minikube start
cargo run
# and in another terminal:
cargo test
```

### quick start

```bash
cd /Users/kushalt/label-preserver

# run linting
./lint.sh

# build release
cargo build --release

# testing with minikube
./test.sh

# production deployment

kubectl apply -f serviceaccount.yaml
kubectl apply -f rbac.yaml
kubectl apply -f deployment.yaml
```
