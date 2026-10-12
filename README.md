# read-to-me-server

Send a long-form article (URL, HTML or PDF) to your own server. It extracts the text, narrates it with an open-weights neural voice (Kokoro by default) and publishes it to a **private podcast feed**, so it shows up in the podcast app you already use.

> **Target:** self-hosted, for one person or a household. A public hosted service is not a goal; decisions that would affect one are flagged **⚑ Hosting impact** in the docs.

> Status: **Phase 1 foundations.** A minimal server exists, with health endpoints only; no product features yet. Next milestone: the [walking skeleton](https://github.com/huwd/read-to-me-server/issues/12), a URL running end to end into the feed. See the roadmap in [doc 07](docs/07-roadmap-and-open-questions.md).

## Running the server

```sh
cargo run --bin rtm -- server           # listens on 127.0.0.1:8080
curl localhost:8080/healthz             # {"status":"ok","version":"0.1.0"}
curl localhost:8080/readyz              # {"status":"ready","checks":[]}
```

| Variable | Default | Meaning |
|----------|---------|---------|
| `RTM_LISTEN_ADDR` | `127.0.0.1:8080` | Address to listen on |
| `RTM_LOG` | `info` | Log filter (`tracing` `EnvFilter` syntax, e.g. `rtm_api=debug`) |

Logs are JSON on stdout. Requests are logged by route pattern only, never by raw path or query string, because those will carry tokens. The server stops gracefully on SIGTERM or Ctrl-C.

## Planning documents

| Doc | Contents |
|-----|----------|
| [01 — Product spec & acceptance criteria](docs/01-product-spec.md) | Personas, scope, functional requirements (FR-*), Gherkin acceptance criteria, MVP exit criteria |
| [02 — Viability](docs/02-product-viability.md) | User need, competitor landscape, critique, viability verdict, riskiest assumptions |
| [03 — Architecture & NFRs](docs/03-architecture-and-nfrs.md) | Assumptions, patterns, domain model, extractor contract, re-use/caching, NFRs, self-host topology (+ hosting reference notes) |
| [04 — Tech stack](docs/04-tech-stack.md) | Choices with rationale: Rust server, TTS service, identity, native apps, infra |
| [05 — Quality, CI/CD & observability](docs/05-quality-cicd-observability.md) | Pipeline gates, test strategy, telemetry, SLOs, product metrics, production readiness |
| [06 — Security & pentest](docs/06-security-and-pentest.md) | Threat model, baseline controls, pentest plan mapped to MITRE ATT&CK / CWE / ATLAS |
| [07 — Roadmap & open questions](docs/07-roadmap-and-open-questions.md) | Phases, ADR backlog, decisions needed |
| [ADRs](docs/adr/README.md) | Architecture decision records |
| [Contributing](CONTRIBUTING.md) | DCO sign-off, checks before a PR |

## Licence

Server: [AGPL-3.0-or-later](https://www.gnu.org/licenses/agpl-3.0.html). Native apps and client SDKs: Apache-2.0. Docs: CC-BY-4.0. See [ADR-0010](docs/adr/0010-licence-and-third-party-policy.md) for the policy on third-party licences and model weights. (`LICENSE` file to be added verbatim from gnu.org — [ADR-0010 F2](docs/adr/0010-licence-and-third-party-policy.md#f2).)
