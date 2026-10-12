# Contributing

Thanks for your interest in read-to-me.

## Licence and sign-off (DCO)

The server is licensed under **AGPL-3.0-or-later** (see [ADR-0010](docs/adr/0010-licence-and-third-party-policy.md)). Contributions are accepted under the same licence, using the [Developer Certificate of Origin 1.1](https://developercertificate.org/). There's no CLA.

Every commit must carry a sign-off line, certifying that you wrote the change or otherwise have the right to submit it under the project's licence:

```
Signed-off-by: Your Name <you@example.com>
```

`git commit -s` adds it automatically. CI rejects pull requests containing commits without a valid sign-off.

## Before opening a pull request

- Read the relevant planning docs in [`docs/`](docs/) and any ADR your change touches.
- Run the same checks CI runs (`just ci` once the toolchain lands).
- Reference requirement IDs (`FR-*`, `NFR-*`) in tests and in the PR description.
- New third-party dependencies must pass the licence allow-list in [`deny.toml`](deny.toml).

## Security issues

Please don't open public issues for vulnerabilities. See `SECURITY.md` (to be added in Iteration 0) for private disclosure.
