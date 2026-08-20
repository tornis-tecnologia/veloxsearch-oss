// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! #74 golden gate: the canonical registry packages under
//! `integrations/<id>/` in the **external veloxsearch-registry repo**
//! (gitlab.com/tornis-desenvolvimento/veloxsearch-registry — extracted from
//! this repo's `registry/` tree in #72/#105) are **frozen, byte-equivalent**
//! extractions of the in-binary recipe catalog (ADR-039 "Tests"). Every
//! recipe's `pipeline.json` / `index-template.json` / `saved-objects.ndjson` /
//! `agent.conf.tmpl` must stay identical to what `recipes::recipe_os_config`,
//! `recipes::dashboard_objects` + `ensure_index_pattern`, and
//! `agents::fluent_bit_conf_template` generate — until the compiled-in
//! catalog is removed once the catalog client (#75) lands.
//!
//! The gates need a registry checkout to compare against, named by the
//! `VELOX_REGISTRY_PATH` env var (the checkout ROOT — the directory holding
//! `integrations/`). Locally, an unset variable SKIPS with a loud banner (a
//! dev without a checkout should not redden). **In CI it is a hard failure**
//! (#108): `test:registry-gates` clones the registry repo's `main` with the
//! job token, so a missing variable there means the lane lost its checkout
//! and would otherwise go green while proving nothing. The resolved path and
//! package count are printed once so a hollow green is visible in the log.
//!
//! To regenerate the asset files after an intentional recipe change (writes
//! into the external checkout — commit + MR them in the registry repo):
//!
//! ```text
//! VELOX_REGISTRY_PATH=/path/to/veloxsearch-registry \
//!     VELOX_UPDATE_REGISTRY=1 cargo test regenerate_registry_assets
//! ```
//!
//! `manifest.yaml` files are hand-authored (i18n titles, discovery predicates,
//! signature block) and are never rewritten; their load-bearing fields are
//! cross-checked against the catalog here instead.

use crate::agents;
use crate::integrations::{self, Package, Vars};
use crate::recipes;
use std::path::PathBuf;

/// Write PAST libtest's output capture (which swallows `println!`/`eprintln!`
/// from PASSING tests) — a skip nobody sees, or a gate nobody can tell ran,
/// is exactly the hollow green #108 is about.
fn to_stderr(msg: &str) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(msg.as_bytes());
}

/// Are we running inside CI? GitLab sets `CI=true` on every job (as do GitHub
/// Actions and most others). An empty/`0`/`false` value counts as "not CI", so
/// a dev who inherited the variable can still get the local skip.
fn in_ci() -> bool {
    std::env::var("CI").is_ok_and(|v| !matches!(v.trim(), "" | "0" | "false" | "False" | "FALSE"))
}

/// The veloxsearch-registry checkout ROOT named by `VELOX_REGISTRY_PATH`, or
/// `None` (with a loud skip banner) when the env var is unset **locally**.
///
/// Three outcomes, by design (#108):
/// * unset, not CI → `None`, loud skip banner — a dev without a checkout
///   should not redden.
/// * unset, in CI → **panic**. The lane lost its checkout; skipping would let
///   it go green while proving nothing, the one failure these gates exist to
///   prevent.
/// * set but not pointing at a checkout → **panic** everywhere. A typo must
///   never silently disable the gates.
///
/// On success the resolved path and package count are printed once, so the job
/// log shows what the gates actually compared against.
pub(crate) fn registry_root() -> Option<PathBuf> {
    let root = match std::env::var("VELOX_REGISTRY_PATH") {
        Ok(root) if !root.trim().is_empty() => PathBuf::from(root),
        _ => {
            assert!(
                !in_ci(),
                "registry gate: VELOX_REGISTRY_PATH is unset (or empty) while CI is set — \
                 this is a HARD FAILURE, never a skip (#108). A CI lane without the registry \
                 checkout proves nothing, so it must not go green. The test:registry-gates job \
                 clones gitlab.com/tornis-desenvolvimento/veloxsearch-registry into \
                 $VELOX_REGISTRY_PATH before running the tests — restore that clone (or the \
                 variable) instead of skipping."
            );
            to_stderr(
                "\n==============================================================\n\
                 SKIPPED registry gate: VELOX_REGISTRY_PATH is not set.\n\
                 The canonical packages live in the external registry repo\n\
                 (gitlab.com/tornis-desenvolvimento/veloxsearch-registry, #105).\n\
                 To run the gates locally:\n\
                 git clone git@gitlab.com:tornis-desenvolvimento/veloxsearch-registry.git\n\
                 VELOX_REGISTRY_PATH=/path/to/veloxsearch-registry cargo test\n\
                 In CI this is a hard failure, not a skip (#108).\n\
                 ==============================================================\n",
            );
            return None;
        }
    };
    let dir = root.join("integrations");
    assert!(
        dir.is_dir(),
        "VELOX_REGISTRY_PATH is set but {} is not a directory — point it at \
         the ROOT of a veloxsearch-registry checkout",
        dir.display()
    );
    // Once per test binary: every gate calls this, and seven identical lines
    // would just be noise.
    static ANNOUNCED: std::sync::Once = std::sync::Once::new();
    ANNOUNCED.call_once(|| {
        let packages = match std::fs::read_dir(&dir) {
            Ok(entries) => entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .count()
                .to_string(),
            Err(e) => format!("unreadable — {e}"),
        };
        to_stderr(&format!(
            "\nregistry gate: VELOX_REGISTRY_PATH={} -> {} ({} packages)\n",
            root.display(),
            dir.display(),
            packages,
        ));
    });
    Some(root)
}

/// `integrations/` inside the checkout `registry_root()` resolves.
/// `pub(crate)` because the `integrations` engine tests resolve their nginx
/// package through it too.
pub(crate) fn registry_dir() -> Option<PathBuf> {
    Some(registry_root()?.join("integrations"))
}

/// The `saved-objects.ndjson` content for a recipe in TEMPLATE form: the
/// index-pattern line (title parameterized on `{index}`, matching what
/// `ensure_index_pattern` creates) followed by each `dashboard_objects`
/// object, one compact JSON document per line, dashboard last.
fn expected_ndjson(recipe: &str) -> String {
    let index = recipes::recipe_index(recipe);
    let mut lines = vec![format!(
        r#"{{"type":"index-pattern","id":"{index}","attributes":{{"title":"{{index}}*","timeFieldName":"@timestamp"}}}}"#
    )];
    for obj in recipes::dashboard_objects(recipe) {
        lines.push(obj.to_string());
    }
    lines.join("\n") + "\n"
}

fn pretty(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v).expect("serializing recipe JSON") + "\n"
}

fn read(dir: &std::path::Path, recipe: &str, file: &str) -> String {
    std::fs::read_to_string(dir.join(recipe).join(file)).unwrap_or_else(|e| {
        panic!(
            "integrations/{recipe}/{file} (in $VELOX_REGISTRY_PATH): {e} \
             (regenerate with VELOX_UPDATE_REGISTRY=1 cargo test)"
        )
    })
}

/// Fixed off-cluster variable values for render checks.
fn test_vars(recipe: &str) -> Vars {
    Vars::new(
        &golden_deployment(),
        recipes::recipe_index(recipe),
        recipe,
        "admin",
        "s3cr3t-pw",
    )
}

/// The deployment the golden gates render against. Its namespace is the app
/// namespace `fluent_bit_conf` resolves off the handle (#80), so the frozen
/// assets and the in-binary output still agree byte for byte.
fn golden_deployment() -> crate::scope::Deployment {
    crate::scope::Deployment::for_test("logs-ab12", crate::k8s::ns(), None)
}

/// Regenerator, not an assertion: with `VELOX_UPDATE_REGISTRY=1` it rewrites
/// every package's four asset files from the in-binary catalog (manifests are
/// hand-authored and never touched). Without the env var it is a no-op, so a
/// normal `cargo test` never writes into the tree.
#[test]
fn regenerate_registry_assets() {
    if std::env::var("VELOX_UPDATE_REGISTRY").is_err() {
        return;
    }
    let root = registry_dir()
        .expect("VELOX_UPDATE_REGISTRY=1 requires VELOX_REGISTRY_PATH — nowhere to write");
    for recipe in recipes::RECIPES {
        let dir = root.join(recipe);
        std::fs::create_dir_all(&dir).expect("creating package dir");
        if let Some((pipeline, template)) = recipes::recipe_os_config(recipe) {
            std::fs::write(dir.join("pipeline.json"), pretty(&pipeline)).unwrap();
            std::fs::write(dir.join("index-template.json"), pretty(&template)).unwrap();
        }
        std::fs::write(dir.join("saved-objects.ndjson"), expected_ndjson(recipe)).unwrap();
        std::fs::write(
            dir.join("agent.conf.tmpl"),
            agents::fluent_bit_conf_template(recipe),
        )
        .unwrap();
    }
}

/// One package directory per catalog id — no missing package, no stray extra.
#[test]
fn registry_holds_exactly_the_recipe_catalog() {
    let Some(dir) = registry_dir() else { return };
    let mut dirs: Vec<String> = std::fs::read_dir(dir)
        .expect("$VELOX_REGISTRY_PATH/integrations exists")
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    dirs.sort();
    let mut want: Vec<String> = recipes::RECIPES.iter().map(|r| r.to_string()).collect();
    want.sort();
    assert_eq!(dirs, want, "registry tree drifted from RECIPES");
}

/// Every package loads through the engine loader and its manifest carries the
/// catalog identity: id, index (`recipe_index`), dashboard (`dashboard_id`),
/// and pipeline/template assets present iff the recipe configures OpenSearch.
#[test]
fn packages_load_and_carry_the_catalog_identity() {
    let Some(dir) = registry_dir() else { return };
    for recipe in recipes::RECIPES {
        let pkg = Package::load_from_dir(&dir.join(recipe)).expect(recipe);
        assert_eq!(pkg.manifest.id, *recipe, "{recipe}: manifest id");
        assert_eq!(
            pkg.manifest.index,
            recipes::recipe_index(recipe),
            "{recipe}: manifest index"
        );
        assert_eq!(
            pkg.manifest.dashboard,
            recipes::dashboard_id(recipe),
            "{recipe}: manifest dashboard slug"
        );
        let has_os = recipes::recipe_os_config(recipe).is_some();
        assert_eq!(pkg.pipeline.is_some(), has_os, "{recipe}: pipeline asset");
        assert_eq!(
            pkg.index_template.is_some(),
            has_os,
            "{recipe}: index-template asset"
        );
    }
}

/// ADR-039 golden gate: each package's `pipeline.json` / `index-template.json`,
/// rendered by the engine, is byte-equivalent (canonical serde serialization —
/// the exact bytes PUT on the wire) to the pre-extraction Rust output.
#[test]
fn pipeline_and_template_are_frozen_rust_output() {
    let Some(dir) = registry_dir() else { return };
    for recipe in recipes::RECIPES {
        let Some((pipeline, template)) = recipes::recipe_os_config(recipe) else {
            continue;
        };
        let vars = test_vars(recipe);
        for (file, want) in [
            ("pipeline.json", pipeline),
            ("index-template.json", template),
        ] {
            let raw = read(&dir, recipe, file);
            let rendered = integrations::render(&raw, &vars)
                .unwrap_or_else(|e| panic!("{recipe}/{file}: {e}"));
            let got: serde_json::Value = serde_json::from_str(&rendered)
                .unwrap_or_else(|e| panic!("{recipe}/{file}: invalid JSON: {e}"));
            assert_eq!(
                got.to_string(),
                want.to_string(),
                "{recipe}/{file} drifted from recipe_os_config"
            );
        }
    }
}

/// ADR-039 golden gate: `saved-objects.ndjson` is the frozen template, and its
/// rendering is byte-equivalent per line to the Rust `dashboard_objects`
/// output (plus the leading `ensure_index_pattern` object).
#[test]
fn saved_objects_are_frozen_rust_output() {
    let Some(dir) = registry_dir() else { return };
    for recipe in recipes::RECIPES {
        let raw = read(&dir, recipe, "saved-objects.ndjson");
        assert_eq!(
            raw,
            expected_ndjson(recipe),
            "{recipe}/saved-objects.ndjson drifted"
        );

        let rendered = integrations::render(&raw, &test_vars(recipe))
            .unwrap_or_else(|e| panic!("{recipe}/saved-objects.ndjson: {e}"));
        let index = recipes::recipe_index(recipe);
        let mut lines = rendered.lines();
        assert_eq!(
            lines.next().unwrap(),
            format!(
                r#"{{"type":"index-pattern","id":"{index}","attributes":{{"title":"{index}*","timeFieldName":"@timestamp"}}}}"#
            ),
            "{recipe}: rendered index-pattern line"
        );
        for (obj, line) in recipes::dashboard_objects(recipe).iter().zip(lines) {
            assert_eq!(line, obj.to_string(), "{recipe}: saved-object line drifted");
        }
    }
}

/// ADR-039 golden gate + interpolation.md round-trip obligation: each
/// package's `agent.conf.tmpl` is byte-identical to the in-binary template,
/// and its rendering is byte-identical to the config `fluent_bit_conf`
/// deploys for the built-in recipe.
#[test]
fn agent_config_is_frozen_rust_output() {
    let Some(dir) = registry_dir() else { return };
    for recipe in recipes::RECIPES {
        let raw = read(&dir, recipe, "agent.conf.tmpl");
        assert_eq!(
            raw,
            agents::fluent_bit_conf_template(recipe),
            "{recipe}/agent.conf.tmpl drifted"
        );

        // Round-trip with the same namespace `fluent_bit_conf` resolves.
        let live_vars = Vars::new(
            &golden_deployment(),
            recipes::recipe_index(recipe),
            recipe,
            "admin",
            "s3cr3t-pw",
        );
        let rendered = integrations::render(&raw, &live_vars)
            .unwrap_or_else(|e| panic!("{recipe}/agent.conf.tmpl: {e}"));
        assert_eq!(
            rendered,
            agents::fluent_bit_conf(&golden_deployment(), recipe, "admin", "s3cr3t-pw"),
            "{recipe}: rendered agent config != built-in fluent_bit_conf"
        );
        assert!(
            !rendered.contains('{'),
            "{recipe}: unrendered token in agent config"
        );
    }
}

/// Clean-install ⇒ clean-uninstall for every package (engine accounting), and
/// the manifest's `teardown` equals the in-binary `recipe_teardown` inventory
/// `disable()` uses — both uninstall paths remove the same set.
#[test]
fn manifest_teardown_matches_engine_and_builtin() {
    use std::collections::BTreeSet;
    let Some(dir) = registry_dir() else { return };
    for recipe in recipes::RECIPES {
        let pkg = Package::load_from_dir(&dir.join(recipe)).expect(recipe);
        let created = integrations::created_resources(&pkg).expect(recipe);
        let removed = integrations::teardown_resources(&pkg.manifest.teardown);
        assert_eq!(created, removed, "{recipe}: install/teardown set mismatch");

        let (builtin, kinds) = recipes::recipe_teardown(recipe);
        assert_eq!(
            pkg.manifest.teardown.ingest_pipeline, builtin.ingest_pipeline,
            "{recipe}: teardown pipeline id"
        );
        assert_eq!(
            pkg.manifest.teardown.index_template, builtin.index_template,
            "{recipe}: teardown template id"
        );
        assert_eq!(
            pkg.manifest.teardown.index_pattern, builtin.index_pattern,
            "{recipe}: teardown index-pattern id"
        );
        let manifest_saved: BTreeSet<_> = pkg.manifest.teardown.saved_objects.iter().collect();
        let builtin_saved: BTreeSet<_> = builtin.saved_objects.iter().collect();
        assert_eq!(
            manifest_saved, builtin_saved,
            "{recipe}: saved-object teardown drifted"
        );
        assert!(
            kinds.get(recipes::dashboard_id(recipe)).map(String::as_str) == Some("dashboard"),
            "{recipe}: dashboard id missing from builtin teardown kinds"
        );
    }
}
