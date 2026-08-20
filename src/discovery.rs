// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Server-only cluster introspection.
//!
//! Read-only scan of the K3S cluster: enumerates OpenSearch deployments
//! (OpenSearchCluster CRs) and detects monitorable workloads by matching
//! container images against a recipe catalog. Feeds the "what's in your
//! cluster" / dynamic config page.

use crate::api::{DeploymentInfo, Detected, Discovery};
use crate::k8s::client;
use anyhow::Result;
use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, StatefulSet};
use kube::api::{ApiResource, DynamicObject, GroupVersionKind, ListParams};
use kube::Api;

/// Namespaces we treat as infrastructure and skip when detecting user workloads.
/// The app's own (dynamic) namespace is excluded at the check site.
const SYSTEM_NS: &[&str] = &[
    "kube-system",
    "kube-public",
    "kube-node-lease",
    "longhorn-system",
    "cert-manager",
    "monitoring",
    "traefik",
    "headlamp",
    "velox-agents",
];

/// Recipe catalog: image substring -> (display kind, recipe id if we have one).
fn match_recipe(image: &str) -> Option<(&'static str, Option<&'static str>)> {
    let i = image.to_lowercase();
    // Order matters: more specific first.
    let table: &[(&str, (&str, Option<&str>))] = &[
        ("nginx", ("Nginx", Some("nginx"))),
        ("postgres", ("PostgreSQL", Some("postgres"))),
        ("traefik", ("Traefik", Some("traefik"))),
        // mariadb before mysql: both feed the `mysql` recipe, but a
        // `mariadb` image string never contains "mysql" (and vice versa),
        // so they are independent needles — keeping the more specific one
        // first preserves the table's convention.
        ("mariadb", ("MySQL / MariaDB", Some("mysql"))),
        ("mysql", ("MySQL / MariaDB", Some("mysql"))),
        ("redis", ("Redis", Some("redis"))),
        // "mongo" also matches "mongodb" images.
        ("mongo", ("MongoDB", Some("mongo"))),
        ("rabbitmq", ("RabbitMQ", Some("rabbitmq"))),
        ("kafka", ("Apache Kafka", Some("kafka"))),
    ];
    table
        .iter()
        .find(|(needle, _)| i.contains(needle))
        .map(|(_, v)| *v)
}

fn scan_containers(
    namespace: &str,
    workload: &str,
    images: impl Iterator<Item = String>,
    out: &mut Vec<Detected>,
) {
    if SYSTEM_NS.contains(&namespace) || namespace == crate::k8s::ns() {
        return;
    }
    for image in images {
        if let Some((kind, recipe)) = match_recipe(&image) {
            // Avoid duplicate (kind, namespace, workload) entries.
            if out
                .iter()
                .any(|d| d.kind == kind && d.namespace == namespace && d.workload == workload)
            {
                continue;
            }
            out.push(Detected {
                kind: kind.to_string(),
                namespace: namespace.to_string(),
                workload: workload.to_string(),
                image,
                recipe: recipe.map(str::to_string),
            });
        }
    }
}

pub async fn discover() -> Result<Discovery> {
    let client = client().await?;

    // 1. OpenSearch deployments (CRs across all namespaces).
    let gvk = GroupVersionKind::gvk("opensearch.org", "v1", "OpenSearchCluster");
    let ar = ApiResource::from_gvk(&gvk);
    let os_api: Api<DynamicObject> = Api::all_with(client.clone(), &ar);
    let mut deployments = Vec::new();
    if let Ok(list) = os_api.list(&ListParams::default()).await {
        for obj in list {
            let status = obj.data.get("status");
            deployments.push(DeploymentInfo {
                name: obj.metadata.name.clone().unwrap_or_default(),
                namespace: obj.metadata.namespace.clone().unwrap_or_default(),
                health: status
                    .and_then(|s| s.get("health"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                phase: status
                    .and_then(|s| s.get("phase"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Pending")
                    .to_string(),
            });
        }
    }

    // 2. Detect monitorable workloads (Deployments / StatefulSets / DaemonSets).
    let mut detected = Vec::new();

    let deploys: Api<Deployment> = Api::all(client.clone());
    if let Ok(list) = deploys.list(&ListParams::default()).await {
        for d in list {
            let ns = d.metadata.namespace.clone().unwrap_or_default();
            let name = d.metadata.name.clone().unwrap_or_default();
            let images = d
                .spec
                .and_then(|s| s.template.spec)
                .map(|p| p.containers.into_iter().filter_map(|c| c.image))
                .into_iter()
                .flatten();
            scan_containers(&ns, &name, images, &mut detected);
        }
    }

    let stss: Api<StatefulSet> = Api::all(client.clone());
    if let Ok(list) = stss.list(&ListParams::default()).await {
        for s in list {
            let ns = s.metadata.namespace.clone().unwrap_or_default();
            let name = s.metadata.name.clone().unwrap_or_default();
            let images = s
                .spec
                .and_then(|sp| sp.template.spec)
                .map(|p| p.containers.into_iter().filter_map(|c| c.image))
                .into_iter()
                .flatten();
            scan_containers(&ns, &name, images, &mut detected);
        }
    }

    let dss: Api<DaemonSet> = Api::all(client.clone());
    if let Ok(list) = dss.list(&ListParams::default()).await {
        for ds in list {
            let ns = ds.metadata.namespace.clone().unwrap_or_default();
            let name = ds.metadata.name.clone().unwrap_or_default();
            let images = ds
                .spec
                .and_then(|sp| sp.template.spec)
                .map(|p| p.containers.into_iter().filter_map(|c| c.image))
                .into_iter()
                .flatten();
            scan_containers(&ns, &name, images, &mut detected);
        }
    }

    detected.sort_by(|a, b| (&a.namespace, &a.workload).cmp(&(&b.namespace, &b.workload)));

    // 3. Telemetry the cluster ALREADY emits (#65). Same scan, same response:
    // the wizard's monitor step reads one payload, not two. Never fails the
    // caller — no telemetry, no manifest and no permission all yield an empty
    // list, and the rest of discovery is unaffected.
    let telemetry = crate::telemetry::discover(&client).await;

    Ok(Discovery {
        deployments,
        detected,
        telemetry,
    })
}

#[cfg(test)]
mod tests {
    use super::match_recipe;

    /// Every round-2 image substring resolves to its catalog recipe id, with a
    /// human display kind. Images carry realistic registry/tag noise so the
    /// `contains` match is exercised, not just bare names.
    #[test]
    fn match_recipe_detects_round2_recipes() {
        let cases: &[(&str, &str, &str)] = &[
            ("redis:7.2-alpine", "Redis", "redis"),
            ("docker.io/library/mysql:8.0", "MySQL / MariaDB", "mysql"),
            ("mariadb:10.11", "MySQL / MariaDB", "mysql"),
            ("traefik:v3.0", "Traefik", "traefik"),
            ("mongo:7", "MongoDB", "mongo"),
            ("registry/mongodb-community:6.0", "MongoDB", "mongo"),
            ("rabbitmq:3.13-management", "RabbitMQ", "rabbitmq"),
            ("bitnami/kafka:3.7", "Apache Kafka", "kafka"),
        ];
        for (image, kind, recipe) in cases {
            assert_eq!(
                match_recipe(image),
                Some((*kind, Some(*recipe))),
                "image {image} should map to {kind}/{recipe}"
            );
        }
    }

    /// mariadb and mysql both feed the `mysql` recipe; neither image string is a
    /// substring of the other, so ordering can't shadow either match.
    #[test]
    fn match_recipe_mariadb_and_mysql_share_recipe() {
        assert_eq!(match_recipe("mariadb:11"), Some(("MySQL / MariaDB", Some("mysql"))));
        assert_eq!(match_recipe("mysql:8"), Some(("MySQL / MariaDB", Some("mysql"))));
    }

    /// Pre-existing entries still resolve, and an unknown image yields nothing.
    #[test]
    fn match_recipe_existing_and_unknown() {
        assert_eq!(match_recipe("nginx:1.27"), Some(("Nginx", Some("nginx"))));
        assert_eq!(match_recipe("postgres:16"), Some(("PostgreSQL", Some("postgres"))));
        assert_eq!(match_recipe("busybox:latest"), None);
    }
}
