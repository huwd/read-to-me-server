# read-to-me-server

Send a long-form article (URL, HTML or PDF) to your own server. It extracts the text, narrates it with an open-weights neural voice (Kokoro by default) and publishes it to a **private podcast feed**, so it shows up in the podcast app you already use.

> **Target:** self-hosted, for one person or a household. A public hosted service is not a goal; decisions that would affect one are flagged **⚑ Hosting impact** in the docs.

> Status: **walking skeleton.** A URL runs end to end: fetched through the egress guard, extracted in a sandbox, narrated by Kokoro, encoded and published to a private feed, in containers. Accounts, a client app and observability come next ([issue #12](https://github.com/huwd/read-to-me-server/issues/12), [doc 07](docs/07-roadmap-and-open-questions.md)).

## Running with Docker Compose

```sh
cp .env.example .env          # set RTM_STATIC_TOKEN (openssl rand -hex 32) and RTM_PUBLIC_URL
docker compose up -d          # builds and starts the server and the Kokoro speech service
```

Submit an article, then get your private feed URL and add it to a podcast app:

```sh
curl -H "Authorization: Bearer $RTM_STATIC_TOKEN" -H 'content-type: application/json' \
  -d '{"url":"https://example.com/article"}' localhost:8080/v1/items
curl -X POST -H "Authorization: Bearer $RTM_STATIC_TOKEN" localhost:8080/v1/feed/token
```

The feed URL is shown once; asking again rotates it. Both containers run read-only, unprivileged and with every capability dropped; the speech service has no network route out ([ADR-0015](docs/adr/0015-container-images-and-compose.md)). The first start builds both images from source, which takes a while: the `tts` build compiles espeak-ng and downloads and verifies the Kokoro model files (about 330 MB).

## Running from source

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
