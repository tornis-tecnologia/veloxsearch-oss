// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Access configuration (ADR-027): how users reach each deployment's
//! dashboards. Persisted in ConfigMap `veloxsearch-config` in the app
//! namespace; an absent ConfigMap means port-forward — the zero-assumption
//! default for generic installs. An existing production install ships the ConfigMap with
//! `ingress` / `veloxsearch.ai`, so its behavior is unchanged.

use anyhow::{Context, Result};
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::api::networking::v1::IngressClass;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{Api, ListParams, Patch, PatchParams};

use crate::k8s::ns;

pub(crate) const CONFIG_MAP: &str = "veloxsearch-config";

#[derive(Clone, Debug, PartialEq)]
pub struct AccessConfig {
    /// "portforward" | "ingress"
    pub mode: String,
    pub base_domain: String,
    pub ingress_class: String,
    /// Name of a `kubernetes.io/tls` Secret in the app namespace to terminate
    /// TLS on the dashboards Ingresses (issue #54). Empty = no `spec.tls`
    /// block — exactly today's behavior, where TLS is whatever the edge
    /// (HAProxy / cloud LB / ingress controller default cert) provides.
    /// Issuer-agnostic: the Secret can come from `kubectl create secret tls`,
    /// a corporate PKI, or a cert-manager Certificate targeting this name.
    pub tls_secret: String,
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self {
            mode: "portforward".into(),
            base_domain: String::new(),
            ingress_class: "traefik".into(),
            tls_secret: String::new(),
        }
    }
}

impl AccessConfig {
    pub fn ingress_enabled(&self) -> bool {
        self.mode == "ingress" && !self.base_domain.is_empty()
    }

    /// Ingress host for a deployment's dashboards, when ingress mode is on.
    pub fn dashboard_host(&self, name: &str) -> Option<String> {
        self.ingress_enabled()
            .then(|| format!("{name}.{}", self.base_domain))
    }

    /// Browser URL for a deployment's dashboards (ingress mode only).
    pub fn dashboard_url(&self, name: &str) -> Option<String> {
        self.dashboard_host(name).map(|h| format!("https://{h}"))
    }

    /// Second, explicitly-named host for the dashboards: `<name>-dashboard.<domain>`.
    ///
    /// An **alias**, not a rename. `dashboard_host` is what SSO redirect URIs
    /// were registered with at the customer's IdP (ADR-045), so changing it
    /// would break every configured provider on the next login. Both hosts hit
    /// the same Ingress; retiring the short one is a separate, deliberate call.
    pub fn dashboard_alias_host(&self, name: &str) -> Option<String> {
        self.ingress_enabled()
            .then(|| format!("{name}-dashboard.{}", self.base_domain))
    }

    /// Public host for the deployment's **OpenSearch API** — the endpoint a
    /// client library or a direct `_bulk` writer targets, as opposed to the
    /// Dashboards UI.
    pub fn opensearch_host(&self, name: &str) -> Option<String> {
        self.ingress_enabled()
            .then(|| format!("{name}-opensearch.{}", self.base_domain))
    }

    pub fn opensearch_url(&self, name: &str) -> Option<String> {
        self.opensearch_host(name).map(|h| format!("https://{h}"))
    }

    /// The copy-paste alternative that works on any cluster. Takes the
    /// deployment's OWN namespace: since ADR-044 that is the tenant's, not the
    /// app's, and a command naming the wrong namespace silently does nothing.
    pub fn portforward_cmd(namespace: &str, name: &str) -> String {
        format!("kubectl -n {namespace} port-forward svc/{name}-dashboards 5601:5601")
    }
}

/// Read the access config. `Ok(default)` when the ConfigMap simply doesn't
/// exist; `Err` only on real API failures (so callers don't flap deployments
/// into portforward mode during a transient outage).
pub async fn get() -> Result<AccessConfig> {
    let client = crate::k8s::client().await?;
    let api: Api<ConfigMap> = Api::namespaced(client, ns());
    let Some(cm) = api
        .get_opt(CONFIG_MAP)
        .await
        .context("reading access config")?
    else {
        return Ok(AccessConfig::default());
    };
    let d = cm.data.unwrap_or_default();
    let pick = |k: &str, fallback: &str| d.get(k).cloned().unwrap_or_else(|| fallback.to_string());
    Ok(AccessConfig {
        mode: pick("access_mode", "portforward"),
        base_domain: pick("base_domain", ""),
        ingress_class: pick("ingress_class", "traefik"),
        tls_secret: pick("tls_secret", ""),
    })
}

/// Public wildcard-DNS suffix used when the operator names no domain.
///
/// `sslip.io` resolves `<anything>.1.2.3.4.sslip.io` to `1.2.3.4`, so a cluster
/// with no DNS of its own still gets working per-deployment hostnames. It is a
/// convenience for evaluation and lab clusters, not a production answer: it
/// depends on a third-party public resolver, and the addresses it produces
/// leak the ingress IP to anyone who reads them.
pub const FALLBACK_DNS_SUFFIX: &str = "sslip.io";

/// Base domain to use when the operator left the field empty.
///
/// Resolved and **stored**, not computed on every read: the Settings screen
/// then shows the domain that is actually in force, and a later change of the
/// ingress IP does not silently move every deployment's URL.
pub async fn default_base_domain() -> Option<String> {
    crate::k8s::ingress_endpoint_ip()
        .await
        .map(|ip| format!("{ip}.{FALLBACK_DNS_SUFFIX}"))
}

pub async fn set(cfg: &AccessConfig) -> Result<()> {
    let mut cfg = cfg.clone();
    if cfg.mode == "ingress" && cfg.base_domain.trim().is_empty() {
        // Empty is a request for the default, not an error: the screen says so,
        // and refusing here would make "I have no domain" a dead end.
        cfg.base_domain = default_base_domain().await.ok_or_else(|| {
            anyhow::anyhow!(
                "ingress mode needs a base domain, and no ingress IP could be \
                 detected to build a {FALLBACK_DNS_SUFFIX} one"
            )
        })?;
    }
    let cfg = &cfg;
    let client = crate::k8s::client().await?;
    let api: Api<ConfigMap> = Api::namespaced(client, ns());
    let cm = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": CONFIG_MAP, "namespace": ns() },
        "data": {
            "access_mode": cfg.mode,
            "base_domain": cfg.base_domain.trim(),
            "ingress_class": cfg.ingress_class,
            "tls_secret": cfg.tls_secret.trim(),
        }
    });
    api.patch(
        CONFIG_MAP,
        &PatchParams::apply("veloxsearch").force(),
        &Patch::Apply(&cm),
    )
    .await
    .context("saving access config")?;
    Ok(())
}

/// Bring every deployment in `scope` in line with `cfg`'s routes: create or
/// re-apply each one's OpenSearch API and Dashboards Ingress. A no-op outside
/// ingress mode — it returns before touching the cluster. Per-deployment
/// failures are logged and skipped so one broken deployment cannot hide the
/// rest; only failing to build a client at all is an error.
pub async fn backfill_ingresses(cfg: &AccessConfig, scope: &crate::scope::Scope) -> Result<()> {
    if !cfg.ingress_enabled() {
        return Ok(());
    }
    let client = crate::k8s::client().await?;
    for dep in crate::k8s::scoped_deployments(scope)
        .await
        .unwrap_or_default()
    {
        if let Err(e) = crate::k8s::ensure_opensearch_ingress(&client, cfg, &dep).await {
            tracing::warn!("opensearch ingress for {dep}: {e:#}");
        }
        if let Err(e) = crate::k8s::ensure_dashboards_ingress(&client, cfg, &dep).await {
            tracing::warn!("backfilling dashboards ingress for {dep}: {e:#}");
        }
    }
    Ok(())
}

/// Startup reconcile of deployment routes against the stored access config.
///
/// Until this ran at startup, only saving Settings → Access backfilled the
/// Ingresses. An access ConfigMap that arrived any other way — restored after a
/// cluster rebuild, applied by GitOps, edited with kubectl — left every existing
/// deployment on port-forward until someone pressed Save on an unchanged form.
/// Runs as the installation admin (the one scope that covers every tenant), and
/// is best-effort: off-cluster or during an API outage it logs and gives up,
/// never blocking the server.
pub async fn backfill_on_startup() {
    let cfg = match get().await {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::debug!("startup ingress backfill skipped: {e:#}");
            return;
        }
    };
    if let Err(e) = backfill_ingresses(&cfg, &crate::scope::Scope::Admin).await {
        tracing::warn!("startup ingress backfill failed: {e:#}");
    } else if cfg.ingress_enabled() {
        tracing::info!(
            "startup ingress backfill done (base domain {})",
            cfg.base_domain
        );
    }
}

/// IngressClasses present on the cluster (REQUIREMENTS.md R8) — drives both
/// the conformity report and which access modes the Settings UI offers.
pub async fn ingress_classes() -> Result<Vec<String>> {
    let client = crate::k8s::client().await?;
    let api: Api<IngressClass> = Api::all(client);
    let list = api
        .list(&ListParams::default())
        .await
        .context("listing ingress classes")?;
    Ok(list
        .items
        .into_iter()
        .filter_map(|c| c.metadata.name)
        .collect())
}

/// The CRD behind the Traefik-native OpenSearch API route: with ingress class
/// `traefik`, `k8s::ensure_opensearch_ingress` applies an `IngressRoute`, not
/// a plain Ingress.
pub const INGRESSROUTE_CRD: &str = "ingressroutes.traefik.io";

/// Why the routes `cfg` asks for cannot be applied on a cluster that offers
/// `classes` and does (or does not) serve [`INGRESSROUTE_CRD`] — `None` when
/// they can. Pure, so the rule is tested without a cluster.
///
/// #139: ingress mode used to be saved with a class the cluster did not have,
/// and every create after that failed on the route. This is the one rule both
/// the Settings save and the create/save preflight enforce.
pub fn route_blocker(
    cfg: &AccessConfig,
    classes: &[String],
    ingressroute_crd: bool,
) -> Option<String> {
    if cfg.mode != "ingress" {
        return None;
    }
    let class = cfg.ingress_class.as_str();
    if !classes.iter().any(|c| c == class) {
        return Some(if classes.is_empty() {
            "ingress mode needs an ingress controller, and this cluster has no \
             IngressClass: install one (Traefik, ingress-nginx, ...) or use \
             port-forward access"
                .to_string()
        } else {
            format!(
                "ingress class '{class}' does not exist on this cluster; available: {}",
                classes.join(", ")
            )
        });
    }
    if class == "traefik" && !ingressroute_crd {
        return Some(format!(
            "ingress class 'traefik' exists, but the Traefik CRD {INGRESSROUTE_CRD} is \
             not installed, and the OpenSearch API route is a Traefik IngressRoute: \
             install Traefik's CRDs or use port-forward access"
        ));
    }
    None
}

/// [`route_blocker`] against the live cluster. `Err` only when the cluster
/// could not be asked — never read as "nothing installed".
pub async fn route_preflight(cfg: &AccessConfig) -> Result<Option<String>> {
    if cfg.mode != "ingress" {
        return Ok(None);
    }
    let classes = ingress_classes().await?;
    let ingressroute_crd = cfg.ingress_class == "traefik" && {
        let client = crate::k8s::client().await?;
        let api: Api<CustomResourceDefinition> = Api::all(client);
        api.get_opt(INGRESSROUTE_CRD)
            .await
            .context("looking up the Traefik IngressRoute CRD")?
            .is_some()
    };
    Ok(route_blocker(cfg, &classes, ingressroute_crd))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ingress(class: &str) -> AccessConfig {
        AccessConfig {
            mode: "ingress".into(),
            base_domain: "example.test".into(),
            ingress_class: class.into(),
            tls_secret: String::new(),
        }
    }

    /// #139: the v0.12.0 report — no IngressClass at all, `traefik` saved
    /// anyway — must be refused, and so must a class the cluster lacks.
    #[test]
    fn ingress_mode_needs_a_class_the_cluster_has() {
        let none: Vec<String> = vec![];
        let why = route_blocker(&ingress("traefik"), &none, false).expect("refused");
        assert!(why.contains("no IngressClass"), "{why}");

        let nginx = vec!["nginx".to_string()];
        let why = route_blocker(&ingress("traefik"), &nginx, true).expect("refused");
        assert!(
            why.contains("'traefik' does not exist") && why.contains("nginx"),
            "{why}"
        );

        assert_eq!(route_blocker(&ingress("nginx"), &nginx, false), None);
    }

    /// The OpenSearch route on Traefik is an IngressRoute: the class alone is
    /// not enough, its CRD has to be served too. Other classes never need it.
    #[test]
    fn traefik_needs_its_ingressroute_crd() {
        let traefik = vec!["traefik".to_string()];
        let why = route_blocker(&ingress("traefik"), &traefik, false).expect("refused");
        assert!(why.contains(INGRESSROUTE_CRD), "{why}");
        assert_eq!(route_blocker(&ingress("traefik"), &traefik, true), None);
    }

    /// Port-forward creates no route, so nothing about ingress can block it —
    /// that is also the way out of a broken ingress configuration.
    #[test]
    fn portforward_is_never_blocked() {
        assert_eq!(route_blocker(&AccessConfig::default(), &[], false), None);
    }

    #[tokio::test]
    async fn backfill_is_a_no_op_outside_ingress_mode() {
        // Port-forward mode (and ingress mode without a domain) must return
        // before building a client: this test has no cluster, so reaching the
        // API would fail.
        let portforward = AccessConfig::default();
        assert!(
            backfill_ingresses(&portforward, &crate::scope::Scope::Admin)
                .await
                .is_ok()
        );
        let no_domain = AccessConfig {
            mode: "ingress".into(),
            ..AccessConfig::default()
        };
        assert!(backfill_ingresses(&no_domain, &crate::scope::Scope::Admin)
            .await
            .is_ok());
    }

    #[test]
    fn the_fallback_domain_is_built_from_the_ingress_ip() {
        // sslip.io resolves `<anything>.1.2.3.4.sslip.io` to 1.2.3.4, so the
        // suffix must be the IP followed by the domain — the other order does
        // not resolve at all.
        //
        // RFC 5737 TEST-NET-1, not an RFC1918 address: the OSS export refuses
        // to ship a tree containing internal-looking IPs (#106), and a fixture
        // is exactly where one gets in unnoticed.
        let d = format!("192.0.2.10.{FALLBACK_DNS_SUFFIX}");
        assert_eq!(d, "192.0.2.10.sslip.io");

        // And a deployment host under it is still a legal DNS name.
        let host = format!("velox-test-traces.{d}");
        assert!(host.split('.').all(|l| !l.is_empty() && l.len() <= 63));
        assert!(host.len() <= 253);
    }
}
