#!/usr/bin/env bash

# build-docker.sh - Build the Signal Fish Server Docker image
#
# Builds the Docker image from the project's Dockerfile. Optionally accepts
# a tag argument; defaults to "latest".
#
# Usage:
#   ./scripts/build-docker.sh              # Build as signal-fish-server:latest
#   ./scripts/build-docker.sh v1.2.3       # Build as signal-fish-server:v1.2.3

set -euo pipefail

# Move to repo root so docker build context is correct
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$REPO_ROOT"

IMAGE_NAME="signal-fish-server"
TAG="${1:-latest}"

echo "Building Signal Fish Docker image..."
echo "Image: ${IMAGE_NAME}:${TAG}"

# Build the Docker image
docker build -t "${IMAGE_NAME}:${TAG}" .

echo ""
echo "Docker build completed successfully!"
echo "To run the container:"
echo "  docker run -p 3536:3536 ${IMAGE_NAME}:${TAG}"
