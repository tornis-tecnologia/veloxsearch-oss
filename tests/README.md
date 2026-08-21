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
| `smoke_check.py <base>` | stdlib only | Install-and-boot: the app is up and serving. The minikube CI lane. |
| `day2_check.py <base> <user> <pw>` | stdlib only | Day-2 operations against a live cluster |
| `firstrun_check.py <base> <user> <pw> pass\|reject [shot.png]` | Playwright | The first-run conformity gate, in both outcomes |
| `journey_check.py <base> <user> <pw>` | Playwright | The create-deployment journey |
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

## Fixtures

`tests/fixtures/integrations/nginx/` is a complete integration package —
manifest, pipeline, index template, saved objects, agent config template. It is
what the apply-engine tests in `src/integrations.rs` run against, and it doubles
as a worked example of the package format documented in
[../docs/integrations/](../docs/integrations/).
