// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Which build is serving (#55).
//!
//! Two kinds of fact, deliberately kept apart:
//!
//! * **Compiled in** — the crate version and the git commit. Both are fixed at
//!   `cargo build` time (`env!` / `option_env!`), so nothing set on the running
//!   Deployment can change what they say. An env var read at runtime was
//!   rejected because a manifest edit would make it lie about what is running,
//!   which is the exact confusion #55 exists to end.
//! * **Observed from the cluster** — the image digest the kubelet actually
//!   pulled for this Pod, and the operator image. These are never configured
//!   by us; they are read back from Pod/Deployment status.
//!
//! `POD_NAME`/`POD_NAMESPACE` (downward API) are only a *locator* for our own
//! Pod object. The digest itself comes from `status.containerStatuses[].imageID`,
//! which the kubelet writes.

use crate::api::BuildInfo;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, ListParams};
use std::collections::BTreeMap;

/// The crate version. Also the `min_core_version` floor registry packages are
/// checked against (`catalog::core_version`), so the version the UI shows and
/// the one a refusal quotes are the same string by construction.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What a build without a baked-in commit reports. Never a guess from a local
/// `.git` — a dirty tree or a detached checkout would produce a sha that does
/// not describe the binary.
pub const COMMIT_UNKNOWN: &str = "unknown";

/// `VELOX_BUILD_COMMIT` as it was in the environment of `cargo build`
/// (`deploy/build-image.sh` forwards it; `release.yml` sets it to the release
/// commit). `option_env!` is tracked by cargo, so changing it rebuilds.
const COMMIT_RAW: Option<&str> = option_env!("VELOX_BUILD_COMMIT");

/// The git commit this binary was built from, or [`COMMIT_UNKNOWN`].
pub const COMMIT: &str = match COMMIT_RAW {
    Some(c) if !c.is_empty() => c,
    _ => COMMIT_UNKNOWN,
};

/// A malformed commit fails the build rather than shipping a binary that
/// displays garbage as its identity.
const _: () = assert!(
    match COMMIT_RAW {
        Some(c) => c.is_empty() || is_git_sha(c),
        None => true,
    },
    "VELOX_BUILD_COMMIT must be a lowercase hex git sha (7-40 chars) or empty"
);

const fn is_git_sha(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 7 || b.len() > 40 {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        if !matches!(b[i], b'0'..=b'9' | b'a'..=b'f') {
            return false;
        }
        i += 1;
    }
    true
}

/// Shown for a runtime fact that could not be read (no cluster, no locator).
pub const UNAVAILABLE: &str = "unavailable";

/// The container name `deploy/install.yaml` gives the app. Only consulted when
/// the Pod has more than one container (an injected sidecar, say).
const APP_CONTAINER: &str = "veloxsearch";

// Same operator predicate as `bootstrap::deploy_is_operator`. Duplicated rather
// than shared while #54 reworks that scan; fold into one once both land.
const OPERATOR_LABEL_KEY: &str = "app.kubernetes.io/name";
const OPERATOR_NAME: &str = "opensearch-operator";

/// Gather everything `GET /api/build_info` returns. Never fails: each runtime
/// fact degrades to [`UNAVAILABLE`] with a note saying why.
pub async fn collect() -> BuildInfo {
    let mut info = BuildInfo {
        version: VERSION.to_string(),
        commit: COMMIT.to_string(),
        image_digest: UNAVAILABLE.to_string(),
        image_id: String::new(),
        image_note: String::new(),
        operator_image: UNAVAILABLE.to_string(),
        operator_deployment: String::new(),
        operator_note: String::new(),
        catalog_source: redact_userinfo(crate::catalog::Registry::from_env().base()),
    };

    let client = match crate::k8s::client().await {
        Ok(c) => c,
        Err(e) => {
            let note = format!("{e:#}");
            info.image_note = note.clone();
            info.operator_note = note;
            return info;
        }
    };

    let (image, operator) = tokio::join!(own_image(&client), operator_image(&client));
    match image {
        Ok(image_id) => {
            match digest_of(&image_id) {
                Some(d) => info.image_digest = d.to_string(),
                None => info.image_note = format!("imageID {image_id:?} carries no sha256 digest"),
            }
            info.image_id = image_id;
        }
        Err(note) => info.image_note = note,
    }
    match operator {
        Ok(Some(op)) => {
            info.operator_image = op.image;
            info.operator_deployment = format!("{}/{}", op.namespace, op.name);
        }
        Ok(None) => info.operator_note = "no OpenSearch operator Deployment found".to_string(),
        Err(note) => info.operator_note = note,
    }
    info
}

/// The kubelet-reported `imageID` of our own container.
async fn own_image(client: &kube::Client) -> Result<String, String> {
    let name = std::env::var("POD_NAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "POD_NAME is not set (downward API) — cannot locate this Pod".to_string())?;
    let pods: Api<Pod> = Api::namespaced(client.clone(), crate::k8s::ns());
    let pod = pods
        .get(name.trim())
        .await
        .map_err(|e| format!("reading Pod {}/{}: {e}", crate::k8s::ns(), name.trim()))?;
    let statuses: Vec<(String, String)> = pod
        .status
        .and_then(|s| s.container_statuses)
        .unwrap_or_default()
        .into_iter()
        .map(|c| (c.name, c.image_id))
        .collect();
    pick_container_image_id(&statuses)
}

/// One container → that one. Several → the one named [`APP_CONTAINER`]. The
/// digest is never taken from a container we cannot tell is ours.
fn pick_container_image_id(statuses: &[(String, String)]) -> Result<String, String> {
    let chosen = match statuses {
        [] => return Err("Pod reports no container statuses yet".to_string()),
        [(_, id)] => id,
        many => match many.iter().find(|(n, _)| n == APP_CONTAINER) {
            Some((_, id)) => id,
            None => {
                return Err(format!(
                    "Pod has {} containers and none is named {APP_CONTAINER:?}",
                    many.len()
                ))
            }
        },
    };
    if chosen.is_empty() {
        return Err("container has not reported an imageID yet".to_string());
    }
    Ok(chosen.clone())
}

/// `sha256:<hex>` out of an `imageID`, whatever runtime wrote it:
/// `docker.io/org/img@sha256:…` (containerd), `docker-pullable://img@sha256:…`,
/// `docker://sha256:…` or a bare `sha256:…`.
fn digest_of(image_id: &str) -> Option<&str> {
    let d = &image_id[image_id.rfind("sha256:")?..];
    let hex = &d["sha256:".len()..];
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(d)
}

#[derive(Debug, PartialEq, Eq)]
struct OperatorImage {
    namespace: String,
    name: String,
    image: String,
}

async fn operator_image(client: &kube::Client) -> Result<Option<OperatorImage>, String> {
    let api: Api<Deployment> = Api::all(client.clone());
    let list = api
        .list(&ListParams::default())
        .await
        .map_err(|e| format!("listing Deployments: {e}"))?;
    let seen: Vec<OperatorCandidate> = list
        .items
        .into_iter()
        .map(|d| OperatorCandidate {
            namespace: d.metadata.namespace.unwrap_or_default(),
            name: d.metadata.name.unwrap_or_default(),
            labels: d.metadata.labels.unwrap_or_default(),
            images: d
                .spec
                .and_then(|s| s.template.spec)
                .map(|p| p.containers.into_iter().filter_map(|c| c.image).collect())
                .unwrap_or_default(),
        })
        .collect();
    Ok(pick_operator(&seen, crate::k8s::ns()))
}

struct OperatorCandidate {
    namespace: String,
    name: String,
    labels: BTreeMap<String, String>,
    images: Vec<String>,
}

/// The operator in our own namespace wins (the one bootstrap installed); else
/// the first foreign one in a stable order, so the answer does not flap with
/// list order. Within it, the container whose image names the operator — the
/// chart also runs a kube-rbac-proxy beside it.
fn pick_operator(seen: &[OperatorCandidate], own_ns: &str) -> Option<OperatorImage> {
    let is_operator = |c: &&OperatorCandidate| {
        c.labels.get(OPERATOR_LABEL_KEY).map(String::as_str) == Some(OPERATOR_NAME)
            || c.name.contains(OPERATOR_NAME)
    };
    let op = seen
        .iter()
        .filter(is_operator)
        .min_by_key(|c| (c.namespace != own_ns, &c.namespace, &c.name))?;
    let image = op
        .images
        .iter()
        .find(|i| i.contains(OPERATOR_NAME))
        .or_else(|| op.images.first())?;
    Some(OperatorImage {
        namespace: op.namespace.clone(),
        name: op.name.clone(),
        image: image.clone(),
    })
}

/// A registry base may be a mirror URL with credentials in it
/// (`https://user:pass@host/…`). The source is worth showing; the password is not.
fn redact_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => format!("{scheme}://***@{}", &rest[at + 1..]),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_the_crate_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    /// Whatever this test binary was built with, the commit is either a real
    /// sha or the literal "unknown" — never empty, never anything else.
    #[test]
    fn commit_is_a_sha_or_unknown() {
        assert!(COMMIT == COMMIT_UNKNOWN || is_git_sha(COMMIT), "{COMMIT:?}");
    }

    #[test]
    fn git_sha_shape() {
        assert!(is_git_sha("9897468"));
        assert!(is_git_sha("9897468a0c4b1f3e2d5c6b7a8f9e0d1c2b3a4f5e"));
        assert!(!is_git_sha("989746")); // too short to mean anything
        assert!(!is_git_sha("9897468A")); // git prints lowercase
        assert!(!is_git_sha("main"));
        assert!(!is_git_sha("9897468a0c4b1f3e2d5c6b7a8f9e0d1c2b3a4f5e0"));
    }

    const HEX: &str = "4d2c5e0b1a3f6e7d8c9b0a1f2e3d4c5b6a7f8e9d0c1b2a3f4e5d6c7b8a9f0e1d";

    #[test]
    fn digest_from_every_runtime_format() {
        let want = format!("sha256:{HEX}");
        for id in [
            format!("docker.io/tornistecnologia/veloxsearch-oss@sha256:{HEX}"),
            format!("docker-pullable://tornistecnologia/veloxsearch-oss@sha256:{HEX}"),
            format!("docker://sha256:{HEX}"),
            format!("sha256:{HEX}"),
        ] {
            assert_eq!(digest_of(&id), Some(want.as_str()), "{id}");
        }
    }

    #[test]
    fn no_digest_is_not_invented() {
        assert_eq!(digest_of(""), None);
        assert_eq!(digest_of("veloxsearch:dev"), None);
        assert_eq!(digest_of("docker://sha256:abc"), None);
    }

    fn statuses(rows: &[(&str, &str)]) -> Vec<(String, String)> {
        rows.iter()
            .map(|(n, i)| (n.to_string(), i.to_string()))
            .collect()
    }

    #[test]
    fn single_container_is_ours_whatever_its_name() {
        assert_eq!(
            pick_container_image_id(&statuses(&[("app", "sha256:x")])),
            Ok("sha256:x".to_string())
        );
    }

    #[test]
    fn with_a_sidecar_only_the_named_container_counts() {
        let s = statuses(&[
            ("istio-proxy", "sha256:proxy"),
            ("veloxsearch", "sha256:app"),
        ]);
        assert_eq!(pick_container_image_id(&s), Ok("sha256:app".to_string()));
        let ambiguous = statuses(&[("a", "sha256:a"), ("b", "sha256:b")]);
        assert!(pick_container_image_id(&ambiguous).is_err());
    }

    #[test]
    fn a_container_without_an_image_id_is_unavailable() {
        assert!(pick_container_image_id(&[]).is_err());
        assert!(pick_container_image_id(&statuses(&[("veloxsearch", "")])).is_err());
    }

    fn candidate(ns: &str, name: &str, labelled: bool, images: &[&str]) -> OperatorCandidate {
        let mut labels = BTreeMap::new();
        if labelled {
            labels.insert(OPERATOR_LABEL_KEY.to_string(), OPERATOR_NAME.to_string());
        }
        OperatorCandidate {
            namespace: ns.to_string(),
            name: name.to_string(),
            labels,
            images: images.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn our_operator_wins_and_its_proxy_container_is_skipped() {
        let seen = [
            candidate(
                "aaa-foreign",
                "opensearch-operator",
                false,
                &["x/opensearch-operator:2.0"],
            ),
            candidate(
                "veloxsearch-system",
                "opensearch-operator",
                true,
                &[
                    "kube-rbac-proxy:0.15",
                    "opensearchproject/opensearch-operator:3.0.0-alpha",
                ],
            ),
            candidate("veloxsearch-system", "veloxsearch", false, &["velox:1"]),
        ];
        assert_eq!(
            pick_operator(&seen, "veloxsearch-system"),
            Some(OperatorImage {
                namespace: "veloxsearch-system".into(),
                name: "opensearch-operator".into(),
                image: "opensearchproject/opensearch-operator:3.0.0-alpha".into(),
            })
        );
    }

    #[test]
    fn a_foreign_operator_is_reported_when_there_is_no_own_one() {
        let seen = [
            candidate("zzz", "my-controller", true, &["custom/controller:1"]),
            candidate(
                "ops",
                "opensearch-operator-controller-manager",
                false,
                &["o/opensearch-operator:2.8"],
            ),
        ];
        let op = pick_operator(&seen, "veloxsearch-system").unwrap();
        assert_eq!(
            (op.namespace.as_str(), op.image.as_str()),
            ("ops", "o/opensearch-operator:2.8")
        );
    }

    #[test]
    fn no_operator_is_none() {
        let seen = [candidate(
            "veloxsearch-system",
            "veloxsearch",
            false,
            &["velox:1"],
        )];
        assert_eq!(pick_operator(&seen, "veloxsearch-system"), None);
    }

    #[test]
    fn registry_credentials_are_redacted() {
        assert_eq!(
            redact_userinfo("https://bot:s3cr3t@git.example.com/reg/raw/main"),
            "https://***@git.example.com/reg/raw/main"
        );
        let plain = "https://raw.githubusercontent.com/tornis-tecnologia/veloxsearch-registry/main";
        assert_eq!(redact_userinfo(plain), plain);
        assert_eq!(
            redact_userinfo("file:///srv/veloxsearch-registry"),
            "file:///srv/veloxsearch-registry"
        );
        // An '@' in the path is not userinfo.
        assert_eq!(redact_userinfo("https://h/a@b"), "https://h/a@b");
    }
}
