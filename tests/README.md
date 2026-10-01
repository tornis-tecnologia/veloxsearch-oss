# End-to-end checks

Standalone Python scripts, **not** a Rust test target — `cargo test` does not
run them. Each one drives a live VeloxSearch against a live cluster and exits
non-zero on the first thing that is wrong.

They drive **rendered widgets, not URLs**: the app has no router, so there is no
path to navigate to. That is why renaming a form field or reshaping a `nav.tabs`
structure can break these even when nothing about routing changed. Check them
when you reshape a screen.

## What each one needs

| Script | Needs | What it checks |
| --- | --- | --- |
| `manifest_changes_check.py` | stdlib only, **no cluster needed** | `deploy/manifest-changes.sh` classifies the fixtures in `fixtures/manifest-changes/` (image-only, rbac, config, other) byte for byte. CI runs it in `manifest-version` |
| `smoke_check.py <base>` | stdlib only | Install-and-boot: the app is up and serving. The minikube CI lane. |
| `day2_check.py <base> <user> <pw>` | stdlib only | Day-2 operations against a live cluster |
| `upgrade_check.py <subcommand> …` | stdlib + `kubectl` | The N-1 → N upgrade contract (ADR-057): driven step by step by `.github/workflows/upgrade.yml` on a throwaway minikube. Creates an admin and a deployment — never run it against a cluster you care about |
| `firstrun_check.py <base> <user> <pw> pass\|reject [shot.png]` | Playwright | The first-run conformity gate, in both outcomes |
| `journey_check.py <base> <user> <pw>` | Playwright | The create-deployment journey, submitted with a double click that must yield one deployment |
| `create_submit_check.py <base> <user> <pw>` | Playwright, **no cluster needed** | The create button disables from the first click and sends one request; it holds `create_cluster` in the browser, so it never provisions anything (#56) |
| `profile_dialog_check.py <base> <user> <pw>` | Playwright, **no cluster needed** | The Capacity view's cluster-profile dialog (ADR-060): one request per names setting, and the saved file is byte-for-byte the preview. It answers `cluster_profile` and `cluster_capacity` in the browser |
| `frontend_robustness_check.py <base> [shots_dir]` | Playwright, **no cluster or backend needed** | Copy buttons report the truth with the Clipboard API denied or missing, render errors stay inside an error boundary, and tooltips are translated in pt/en/es (#141). It answers every `/api/*` call in the browser, so a static server over `frontend/build` is enough: `python3 -m http.server -d frontend/build 8099` |
| `access_settings_check.py <base> <user> <pw>` | Playwright, **no cluster needed** | Settings → Access shows why an ingress class the cluster lacks cannot be saved, and disables Save until one it has (or port-forward) is picked (#139). It answers `access_settings` and `save_access_settings` in the browser |
| `browser_check.py <base> <user> <pw>` | Playwright | Browser smoke plus a network gate: the console must stay free of hydration and panic errors |

`<base>` is the URL the app is reachable at, e.g. `http://localhost:3000` behind
a port-forward.

## Running them

The stdlib ones need nothing installed:

```sh
python3 tests/smoke_check.py http://localhost:3000
python3 tests/day2_check.py  http://localhost:3000 admin "$PASSWORD"
```

The Playwright ones need a browser:

```sh
python3 -m venv .venv && . .venv/bin/activate
pip install -r tests/requirements.txt
playwright install chromium

python3 tests/journey_check.py http://localhost:3000 admin "$PASSWORD"
```

`firstrun_check.py` takes the expected outcome as an argument, because both
outcomes are correct behaviour on the right cluster:

```sh
# on a cluster that satisfies R1-R8
python3 tests/firstrun_check.py http://localhost:3000 admin "$PW" pass

# on a cluster that fails a requirement (undersized, foreign operator)
python3 tests/firstrun_check.py http://localhost:3000 admin "$PW" reject shot.png
```

The optional screenshot path is written on failure, which is usually the fastest
way to see what the gate actually said.

## Getting a cluster to run them against

See [../docs/DEVELOPMENT.md](../docs/DEVELOPMENT.md#running-against-minikube).
`smoke_check.py` is the only one designed to pass on an in-CI minikube; the rest
assume a cluster that meets the full platform contract in
[../docs/REQUIREMENTS.md](../docs/REQUIREMENTS.md), which a CI minikube
deliberately does not.

## Satellite services

`tests/satellites/` holds two throwaway services to point a deployment at, for
the surfaces that need something on the other end: OpenLDAP for the ADR-045
auth-provider axis and MinIO for the ADR-049 snapshot repository. Plain
manifests, `kubectl apply -f`, no distribution-specific objects.

```sh
kubectl apply -f tests/satellites/openldap.yaml
kubectl apply -f tests/satellites/minio.yaml
```

Credentials are in the clear in those manifests on purpose — they are fixtures.
See [satellites/README.md](satellites/README.md) for the values to paste into
each form, and for the one thing that is not obvious: with
`VELOX_MULTITENANT_AUTH` on, the ADR-044 tenant NetworkPolicy set has no LDAP
egress hole, so the LDAP fixture needs `netpol-tenant-egress-ldap.yaml` applied
to the tenant namespace.

## Fixtures

`tests/fixtures/integrations/nginx/` is a complete integration package —
manifest, pipeline, index template, saved objects, agent config template. It is
what the apply-engine tests in `src/integrations.rs` run against, and it doubles
as a worked example of the package format documented in
[../docs/integrations/](../docs/integrations/).
