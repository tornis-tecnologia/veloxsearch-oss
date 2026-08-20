## What this changes

<!-- One or two sentences. What behaviour is different after this PR? -->

## Why

<!-- The reasoning. Link the issue or ADR this comes from: Fixes #123 -->

## How it was verified

<!-- What you actually ran. "cargo test" alone is fine for a pure change;
     say which cluster/distro if it touches the Kubernetes layer. -->

## Checklist

- [ ] Commits are signed off (`git commit -s`) — see [DCO](../CONTRIBUTING.md#developer-certificate-of-origin)
- [ ] `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings` are clean
- [ ] `cargo test` passes
- [ ] New source files carry the two-line SPDX header
- [ ] New comments explain *why*, and cite the ADR or issue where relevant
- [ ] Docs updated in this PR (both `.md` and `.pt-BR.md` if one changed)
- [ ] New env vars are prefixed `VELOX_` and default to prior behaviour
- [ ] If a UI field or tab structure changed, `tests/*_check.py` still match
- [ ] If an endpoint was added: DTO + handler + `routes()` + `frontend/api.jsx`, methods matching

## Anything reviewers should look at closely

<!-- Optional. A tradeoff you are unsure about, an alternative you rejected. -->
