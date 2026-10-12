# 02 — Problematising the Proposition: Need, Market, Viability

> Market notes reflect the landscape as known in mid-2026. This space moves fast and has a graveyard; re-verify competitor status before relying on it.
>
> **Decision (2026-10-11):** a hosted service is **not pursued** ([doc 07 Q2](07-roadmap-and-open-questions.md#q2)). The hosted analysis below is kept as the rationale for that decision and as context for ⚑ Hosting impact notes elsewhere.

## 1. Is there a real user need?

**Yes, but it's a "vitamin" for most people and a "painkiller" for a few.**

| Need | Strength | Evidence / reasoning |
|------|----------|----------------------|
| "I save articles and never read them" (read-it-later backlog) | Strong, widespread | The whole read-it-later category exists because of it. Typical save-to-read ratios are very low. |
| Converting dead time (commute, gym, chores, driving) into reading time | Strong | Podcast and audiobook listening is mainstream; the same people have a reading backlog. |
| Accessibility: dyslexia, low vision, ADHD, screen fatigue, migraine | Strong (painkiller) | Text-to-speech is an assistive technology first. These users have the highest willingness to pay and retention. |
| Using *my existing podcast player* rather than another app | Moderate, under-served | Most TTS products force you into their own player. Podcast apps already solve speed, offline, resume, CarPlay/Android Auto, smart-speed, sleep timers, and cross-device sync. |
| Privacy / self-ownership of reading history | Niche but real | Reading history is sensitive. Self-hosters (r/selfhosted, homelab community) value this explicitly. |

**Jobs-to-be-done framing:** *"When I find something long and worthwhile but I don't have time to read it now, I want it to turn up in my listening queue, so that I actually consume it during time I'd otherwise waste."*

The critical insight is **delivery, not synthesis**: TTS is now a commodity; the value is frictionless capture (share sheet) → automatic arrival in a queue you already check daily (your podcast app).

## 2. Existing products

| Product | Model | Podcast feed? | Self-host? | Notes |
|---------|-------|---------------|------------|-------|
| **OS-native read-aloud** (iOS Safari "Listen to Page" / Spoken Content, Chrome/Android "Listen to this page", Edge Read Aloud) | Free, built in | No | n/a | The strongest "do nothing" competitor. Good enough for many. Not queued, not offline, tied to the page/tab, quality varies. |
| **ElevenReader** (ElevenLabs) | Freemium app | No (own player) | No | Very high voice quality, free tier generous. ElevenLabs acquired Omnivore's team in 2024. The quality benchmark. |
| **Speechify** | Expensive subscription | No | No | Market leader by revenue, aggressive pricing, own player. Strong accessibility positioning. |
| **Readwise Reader**, **Matter**, **Instapaper** | Read-it-later with built-in TTS | No (own player) | No | TTS is a feature inside a reading app. Good if you already live in that app. |
| **Audioread**, **Listen Later** and similar | Paid, small | **Yes** | No | Closest direct competitors: send article → private podcast feed. Small indie products; validates the concept but shows the ceiling. |
| **NotebookLM Audio Overviews** and similar | Free/bundled | No | No | Generative *summaries/discussions*, not verbatim narration. Different job, but competes for the same listening minutes. |
| **Pocket** (shut down 2025), **Omnivore** (shut down 2024), **Narro** (defunct) | — | — | — | The graveyard. Read-it-later is hard to monetise; incumbents with distribution have exited. |
| **Wallabag**, **Linkding**, **Readeck**, **Karakeep** (self-hosted) | OSS | No (mostly) | Yes | Self-hosted read-it-later with no or basic TTS. Integration targets, not competitors. |
| Various GitHub "article-to-podcast" scripts | OSS scripts | Yes | Yes | Prove demand among developers; typically unmaintained, insecure (SSRF), cloud-TTS dependent. |

### Gap analysis
The intersection **{ verbatim narration × private podcast feed × self-hostable × open-weights local TTS × extensible extraction }** is, as far as I know, not well served by a maintained, secure, polished product. That's a real but **narrow** gap.

## 3. Critique of the proposition (the uncomfortable bits)

1. **"Good enough" free alternatives.** OS-level read-aloud is free and improving. If the user is already holding the phone on the page, a server pipeline adds latency without obvious benefit. *Our answer:* the value is the queue + podcast-app UX + offline + hands-free, not the voice.
2. **Voice quality gap.** Open-weights models (Kokoro and peers) are good but not ElevenLabs-good, particularly on long-form prosody and unusual names. People tolerate a lot of voice imperfection for content they care about, but it lowers the ceiling for any paid product (⚑ Hosting impact). *Mitigation:* pluggable engines; allow a BYO-API-key cloud engine.
3. **Extraction is the real quality problem.** Users forgive a slightly robotic voice; they don't forgive hearing "Accept cookies. Subscribe to our newsletter. Related articles…" or the article cutting off halfway. Paywalls and JS-rendered sites will fail server-side fetch. *Mitigation:* triage/adapter pattern, golden corpus, upload-the-HTML path, report-a-problem loop.
4. **The read-it-later market has a graveyard.** Pocket, Omnivore, Narro — low willingness to pay, high churn. A hosted business here is hard. *Implication:* treat this as an excellent self-hosted OSS project. A hosted service is not pursued.
5. **Copyright and legal exposure (mainly a hosting concern).** Generating and *storing* audio of copyrighted articles for a single user who fetched it themselves is analogous to a personal reading aid (and many jurisdictions have accessibility exceptions). A **central service that caches and re-serves the same audio across users** looks more like redistribution. Paywalled content makes this sharper. *Implication for self-host:* the operator is the user, re-use between users on one instance is keyed only on content the requester already has ([FR-CACHE-03](01-product-spec.md#fr-cache-03)), and there is no "browse popular" feature. ⚑ Hosting impact: a central service would need legal advice and a takedown process before launch.
6. **Podcast feeds are a leaky auth model.** Feed URLs with tokens get pasted into podcast apps that sync them to the vendor's cloud (and some public directories crawl them). That's acceptable for personal content but must be explained to users and mitigated (`itunes:block`, `podcast:locked`, rotatable tokens).
7. **Native apps may be low leverage.** iOS Shortcuts and Android "HTTP Shortcuts"-style apps can POST a URL with a PAT from the share sheet today. Two native apps are a large, ongoing maintenance cost. *Recommendation:* ship server + PAT + Shortcut first; build native apps when (a) OAuth-based sign-in for non-technical users matters (e.g. household members on an instance), or (b) management UX (prune, status) becomes the bottleneck. A small responsive web UI could cover management sooner and cheaper.
8. **Compute cost vs. price.** TTS of a 5,000-word article is ~30–35 min of audio. On self-host, cost is effectively zero. ⚑ Hosting impact: on a hosted service, open-weights on GPU is cheap (order of a cent or less per audio hour of compute *if* utilisation is decent; benchmark before trusting), but cold starts and idle GPUs dominate at low volume. Cloud TTS APIs are orders of magnitude more expensive per audio hour and would break a cheap subscription.

## 4. Viability verdict

| Lens | Verdict |
|------|---------|
| **As a personal tool** | Highly viable. Clear daily utility, modest build, low running cost. |
| **As an open-source self-hosted project** | Viable and differentiated. Good fit for the self-hosted community; a well-engineered, secure, podcast-native, local-TTS project with extensible extractors would be welcomed. Success metric = adoption/stars/contributors, not revenue. |
| **As a hosted commercial service** | Marginal. Crowded market with well-funded incumbents (ElevenLabs, Speechify) and free OS features; low WTP; legal exposure around caching. Plausible as a low-cost "we host it for you" tier (à la many OSS projects: €3–5/month) funding the project, aimed at privacy-minded and accessibility users. Validate demand with a waitlist before building multi-tenancy. **Not pursued ([doc 07 Q2](07-roadmap-and-open-questions.md#q2)).** |
| **Learning vehicle (Rust, native apps, production engineering)** | Excellent. Real concurrency, untrusted-input parsing, ML inference integration, storage, auth, mobile — a complete system. |

### Riskiest assumptions to test first (in order)
1. **Extraction quality across *your* reading sources** is good enough (≥ 80% clean, unattended) — test with a 1-week spike over your last 100 saved articles before any further investment.
2. **Kokoro (or similar) is pleasant enough to listen to for 30+ minutes** — listen to 5 long articles end to end at 1.0× and 1.5×.
3. **Podcast-feed delivery is actually preferred** over an in-app player — dogfood for 4 weeks.
4. ~~(Hosted only) People will pay~~ — not tested; hosting is not pursued.

### Differentiators to lean into
- Lives in the podcast app you already use.
- Private by default, self-hostable, no cloud TTS required.
- Free transcripts and chapter markers (we already have the text — a feature closed apps rarely expose in a portable way).
- Extensible extraction with a public adapter interface — community can add site adapters.
- Integrations with self-hosted read-it-later tools (Wallabag/Readeck/Karakeep webhooks: "tag `listen` → episode").
