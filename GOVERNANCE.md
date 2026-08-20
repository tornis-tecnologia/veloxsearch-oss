# Governance

VeloxSearch is a small project with a clear owner. This document says who
decides what, so nobody has to guess.

## Roles

**Maintainers** review and merge pull requests, triage issues, cut releases, and
hold the signing keys. They are listed in
[`.github/CODEOWNERS`](.github/CODEOWNERS).

**Contributors** are everyone who opens an issue, a pull request, a translation
or a registry integration. No formal status is required, and no CLA is
requested — a [DCO sign-off](CONTRIBUTING.md#developer-certificate-of-origin) on
each commit is enough.

**The project lead** is Tornis Desenvolvimento, which founded VeloxSearch and
owns the trademark, the registry signing keys and the published container image.
The lead has the final call on direction, and on anything a maintainer
disagreement cannot resolve. This is a benevolent-dictator model, stated plainly
rather than implied.

## How decisions are made

Most changes need no ceremony: open a PR, a maintainer reviews it, it merges.

Three categories need more:

**Architectural decisions** are recorded as ADRs in [`docs/adr/`](docs/adr/).
Anything that changes a boundary — the ownership model in `src/scope.rs`, the
signing path in `src/catalog.rs`, the platform contract in
`docs/REQUIREMENTS.md`, the API shape — gets an ADR before the implementation,
not after. Propose one as a PR against `docs/adr/`; discussion happens on that
PR.

**Security-relevant changes** (auth, scope, signing, RBAC in
`deploy/install.yaml`) need a maintainer review from someone who did not write
the change. Where that is not possible, the reasoning is written into the PR so
it can be audited later.

**Breaking changes** to the install manifest, the API, or the integration
package format need an ADR, a `CHANGELOG.md` entry, and a migration note in the
release. Feature flags default off precisely so that most changes never become
breaking ones.

Disagreements are settled in the open, on the issue or PR. If a discussion
stalls, a maintainer decides and records why. Silence is not consent: a stalled
proposal is stalled, not approved.

## Becoming a maintainer

There is no committee. Merge a handful of substantial changes, review other
people's PRs usefully, and stay around — a maintainer will invite you. What is
being assessed is judgment about this codebase's conventions (see
[CONTRIBUTING.md](CONTRIBUTING.md)), not volume of commits.

Maintainers who go inactive for a long stretch are moved to emeritus in
`CODEOWNERS`. That is bookkeeping, not a judgement, and it is reversible.

## Releases

Releases are cut by a maintainer from `main`. Versioning is semantic once the
project reaches 1.0; before that, minor versions may carry breaking changes,
which the `CHANGELOG.md` calls out explicitly. The release process lives in
[docs/DEPLOY.md](docs/DEPLOY.md).

## Relationship to the commercial product

Tornis Desenvolvimento may offer commercial support and hosted operation of
VeloxSearch. That does not create a second codebase: what is in this repository
is the product, under AGPL-3.0-only. If that ever changes, it will be announced
here first, and the change cannot retroactively affect code already published
under the AGPL.
