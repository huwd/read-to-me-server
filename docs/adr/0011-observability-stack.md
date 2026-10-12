# ADR-0011: Observability: OpenTelemetry, optional bundled LGTM, local-only analytics

- **Status:** Accepted (2026-10-11)
- **Date:** 2026-10-11
- **Deciders:** project owner
- **Related:** [doc 05 §4](../05-quality-cicd-observability.md#4-observability-stack) (metrics, SLOs, product metrics), [NFR-SEC3](../03-architecture-and-nfrs.md#nfr-sec3), [NFR-M5](../03-architecture-and-nfrs.md#nfr-m5), [ADR-0002](0002-rust-modular-monolith.md)

## Context

The brief asks for full observability before production: health, NFRs and product success metrics. The "production" environments are the owner's instance and other people's self-hosted instances ([doc 05 §1](../05-quality-cicd-observability.md#1-repository--workflow-conventions)). That means:
- observability must work **on the box** with no SaaS;
- it must slot into existing homelab monitoring;
- it must never send data off the instance by default;
- it must not log article content.

## Decision

### Instrumentation (in code)
- **`tracing`** throughout, exported via **OpenTelemetry (OTLP)** for traces, metrics and logs, using `tracing-opentelemetry` and `opentelemetry-otlp`.
- **Trace propagation:** W3C `traceparent` is accepted on requests. The trace context is stored on each job, so asynchronous pipeline stages, the extractor sandbox and the TTS process all join the submission's trace.
- **Prometheus endpoint:** `GET /metrics` exposes the same metrics in Prometheus format for homelabs that already scrape. It's protected by a separate token or bound to an internal interface.
- **Metric names and labels** are as listed in [doc 05 §4.2](../05-quality-cicd-observability.md#42-golden-signal--health-metrics-reduse). Labels are bounded: no URLs, no user IDs, and domains only as a top-N bucket.
- **Health endpoints:** `/healthz` (liveness) and `/readyz` (DB, object store, TTS process and sandbox capabilities) on both server and TTS ([NFR-M5](../03-architecture-and-nfrs.md#nfr-m5)).

### Logging and privacy
- **Structured JSON logs** to stdout. Every line carries `trace_id`, `span_id` and a salted `user_id_hash`.
- **Redaction layer:** a `tracing` filter drops article text, full URLs (keeping the domain only) and anything matching token prefixes (`rtm_pat_`, `rtm_feed_`, `Bearer`).
- **An E2E test** greps all logs for fixture article text and token values, and fails if any are found.
- **Debug content logging:** opt-in per instance, retained for at most 7 days ([NFR-SEC3](../03-architecture-and-nfrs.md#nfr-sec3)).

### Collection and dashboards (optional, bundled)
- **Compose profile `observability`:** a single `grafana/otel-lgtm` container (Grafana, Loki, Tempo, Prometheus/Mimir, OTel collector).
- **Provisioned as code** from the repo:
  - dashboards: pipeline health, NFR/SLO view, security signals, product metrics;
  - alert rules, each linked to a runbook in `docs/runbooks/`.
- **Alert delivery:** Grafana contact points for ntfy, email or a generic webhook. No paging service assumed.
- **External uptime:** documented use of Uptime Kuma or any external checker against the feed URL.
- **Error tracking:** optional GlitchTip (Sentry-compatible, self-hosted). The server's Sentry SDK is off unless a DSN is configured.

### Product analytics
- **Computed locally** from SQLite and metrics; shown on the provisioned "Product" dashboard. Covers submissions, time-to-feed, listen-through from enclosure byte ranges, unattended success rate and prune usage ([doc 05 §4.4](../05-quality-cicd-observability.md#44-product-success-metrics)).
- **No third-party analytics SDKs and no phone-home** from any instance. Project-level adoption is measured only from public signals: image pulls, GitHub activity.

## Alternatives considered

| Option | Why not |
|--------|---------|
| Prometheus metrics + plain logs only | Loses distributed tracing across sandbox and TTS, which is the most useful tool for "why did this item take 40 minutes?". |
| Mandatory bundled observability | Too heavy for a Pi; many homelabs already run Grafana and Prometheus. Optional profile instead. |
| SaaS (Grafana Cloud, Honeycomb, Sentry SaaS) by default | Sends data off the box; conflicts with the privacy posture. Users can still point OTLP at one if they choose. |
| Product analytics via PostHog | Third-party SDK, and not needed for a self-hosted personal tool. ⚑ Hosting impact: would be reconsidered for a public service. |
| Opt-in anonymous usage telemetry | Possible later via a new ADR; not now. Trust matters more than data for a self-hosted project. |

## Consequences

- Full traces, metrics and logs are available with one compose flag, and nothing leaves the instance by default.
- Instrumentation is part of the Definition of Done: a new stage or endpoint ships with a span and metrics ([doc 05 §5](../05-quality-cicd-observability.md#5-definition-of-done-per-story)).
- The LGTM container adds ~1 GB RAM when enabled; documented, and off by default on small hosts.
