---
name: deployment-operations
description: Build, deploy, scale, observe, and gracefully operate Signal Fish Server. Use for Dockerfiles, container images, Kubernetes or hosting configuration, health checks, shutdown and draining, rollout strategy, load shedding, circuit breakers, metrics, tracing, or production reliability.
---

<!-- markdownlint-disable MD013 -->

# Deployment Operations

Preserve the single-binary, zero-external-runtime-dependency design while making lifecycle behavior observable and testable.

## Route the task

- Read [container-Docker.md](references/container-docker.md) for Docker and image changes.
- Read [deployment-strategies.md](references/deployment-strategies.md) for rollout, orchestration, and service deployment.
- Read [graceful-degradation-deployment.md](references/graceful-degradation-deployment.md) for health checks, draining, and shutdown.
- Read [graceful-degradation-service-levels.md](references/graceful-degradation-service-levels.md) for load shedding, circuit breakers, and availability behavior.
- Read [observability-and-logging.md](references/observability-and-logging.md) for metrics, tracing, and structured logging.
- Invoke `$web-service-security` for container hardening or abuse controls.

Validate locally with the repository's Docker and configuration checks before relying on a live environment.
