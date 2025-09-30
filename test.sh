#!/usr/bin/env bash

APP_NAME="node-taint-preserver"

# Kill the pod on exit
trap "kubectl delete deployment ${APP_NAME} && kubectl get pods --no-headers -o custom-columns=\":metadata.name\" | grep ${APP_NAME} | xargs -I {} kubectl delete pod {} --grace-period=0 --force --wait=true" EXIT

set -e

# Start minikube Docker container if it's not running
if minikube status > /dev/null 2>&1; then
  echo "Minikube is already running."
else
  echo "Starting minikube..."
  minikube start
fi

eval $(minikube -p minikube docker-env)

# Build Docker image
docker build -t $APP_NAME .

kubectl apply -f serviceaccount.yaml
kubectl apply -f rbac.yaml
kubectl apply -f deployment.yaml --force
kubectl rollout status deployment/$APP_NAME --timeout=60s
kubectl get deployments
kubectl describe deployment $APP_NAME
kubectl describe pod
kubectl get pods -l app=$APP_NAME
# Wait for the pod to be ready
kubectl wait --for=condition=ready pod -l app=$APP_NAME --timeout=60s
cargo test

