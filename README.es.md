<div align="center">

<img src=".github/assets/logo.svg" width="92" alt="Logo de VeloxSearch" />

# VeloxSearch

**Convierte un clúster Kubernetes vacío en una plataforma OpenSearch
gestionada.**

[![CI](https://github.com/tornis-tecnologia/veloxsearch-oss/actions/workflows/ci.yml/badge.svg)](https://github.com/tornis-tecnologia/veloxsearch-oss/actions/workflows/ci.yml)
[![Licencia: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)
[![Descargas en Docker](https://img.shields.io/docker/pulls/tornistecnologia/veloxsearch-oss?logo=docker&label=pulls)](https://hub.docker.com/r/tornistecnologia/veloxsearch-oss)
[![rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-dea584?logo=rust)](Cargo.toml)
[![kubernetes ≥ 1.30](https://img.shields.io/badge/kubernetes%20%E2%89%A5%201.30-326ce5?logo=kubernetes&logoColor=white)](docs/REQUIREMENTS.md)
[![DCO](https://img.shields.io/badge/DCO-required-8e44ad)](CONTRIBUTING.md)

*Read in English: [README.md](README.md) · Leia em português: [README.pt-BR.md](README.pt-BR.md)*

</div>

VeloxSearch es un plano de control con interfaz web que se ejecuta dentro de tu
propio clúster Kubernetes. Lo apuntas al clúster y abres el navegador: comprueba
que el clúster da la talla, instala lo que falta (cert-manager, el operador de
OpenSearch, Longhorn), crea despliegues de OpenSearch con un asistente de cuatro
pasos, conecta la recolección de registros y después se ocupa del trabajo del
día 2 — actualizaciones de versión, snapshots, rotación de credenciales.

**Código abierto bajo la [GNU AGPL-3.0-only](LICENSE).** Alojarlo por tu cuenta
— para tu equipo o tu empresa, también con fines comerciales — es gratis y no
depende de nada por nuestra parte. La única obligación: si modificas VeloxSearch
y dejas que otras personas usen tu versión a través de la red, debes ofrecerles
el código fuente de esa versión. [Detalles más abajo](#licencia).

## Instalación

```bash
kubectl apply -f https://github.com/tornis-tecnologia/veloxsearch-oss/releases/latest/download/install.yaml
```

Después, `kubectl -n veloxsearch-system port-forward svc/veloxsearch 3000:80`,
abre <http://localhost:3000> y crea la cuenta de administrador — la app sigue
sola desde ahí. En un clúster con IngressClass por defecto (un k3s recién
instalado, por ejemplo) también responde en `http://<ip-del-nodo>/`, sin
port-forward.

> **¿Empiezas de cero, sin un clúster Kubernetes?** Sigue
> [`docs/INSTALL.md`](docs/INSTALL.md#0-no-kubernetes-cluster-yet) — de una
> máquina Linux o un portátil a la interfaz en marcha — o la
> [guía de usuario en el sitio](https://get.veloxsearch.ai/docs/es).

## Pantallas

<table>
  <tr>
    <td width="50%"><a href=".github/assets/screens/conformity.png"><img src=".github/assets/screens/conformity.png" alt="Pantalla de conformidad: los requisitos R1 a R8 superados en un clúster k3s de tres nodos, con cert-manager y el operador de OpenSearch en cola para el bootstrap" /></a></td>
    <td width="50%"><a href=".github/assets/screens/deployment-overview.png"><img src=".github/assets/screens/deployment-overview.png" alt="Visión general de un despliegue verde llamado prod-logs: OpenSearch 3.8.0, tres de tres nodos listos, y las direcciones de Dashboards y de la API" /></a></td>
  </tr>
  <tr>
    <td align="center"><sub>El clúster se comprueba antes de instalar nada</sub></td>
    <td align="center"><sub>Un despliegue verde y sus direcciones</sub></td>
  </tr>
  <tr>
    <td width="50%"><a href=".github/assets/screens/create-purpose.png"><img src=".github/assets/screens/create-purpose.png" alt="Asistente de creación, paso 1 de 4: el nombre del despliegue y la elección de su propósito — Observability, Security o Search — con lo que cada uno conserva, recolecta y configura" /></a></td>
    <td width="50%"><a href=".github/assets/screens/create-review.png"><img src=".github/assets/screens/create-review.png" alt="Asistente de creación, paso de revisión: nombre, propósito, tamaño (medium, tres nodos, 10 GiB) y copia de seguridad, con el botón Create cluster" /></a></td>
  </tr>
  <tr>
    <td align="center"><sub>Crear, paso 1: el propósito fija la retención y los valores por defecto</sub></td>
    <td align="center"><sub>Crear, paso 4: revisión antes de aprovisionar nada</sub></td>
  </tr>
</table>

<sub>Las capturas muestran la interfaz en inglés; también está disponible en español y portugués.</sub>

## Por qué VeloxSearch

- **Un asistente en lugar de una carpeta de YAML.** Propósito → tamaño →
  snapshot → revisión. Los presets de dimensionamiento vienen del backend; el
  propósito que eliges fija por ti la retención, los detectores y los valores por
  defecto de los índices.
- **Comprueba antes de tocar.** Ocho requisitos numerados se verifican de
  entrada. Un clúster fuera del perímetro recibe un rechazo claro que dice qué
  falló — nunca una instalación a medias.
- **Instala sus propios requisitos previos y luego devuelve las llaves.**
  cert-manager, el operador de OpenSearch y Longhorn llegan solos, y la app
  **revoca su propio binding de cluster-admin** al terminar.
- **Registros fluyendo sin escribir canalizaciones.** Integraciones de un clic
  para nginx, postgres, redis, mysql, traefik, mongo, rabbitmq, kafka y
  Kubernetes entregan juntos la canalización de ingesta, la plantilla de índice,
  la política de retención y el agente de recolección.
- **El día 2 viene incluido.** Actualizaciones de versión nodo a nodo (esperando
  el verde entre uno y otro y rechazando los downgrades que el operador no sabe
  deshacer), programaciones de snapshot en S3, rotación de la contraseña de
  administrador y una pila OpenTelemetry opcional.
- **Un estado que se explica solo.** Una operación atascada se explica con hechos
  del clúster — qué shard, qué nodo, cuánto tiempo — en lugar de un spinner.
- **Tu clúster, tus datos.** Nada se ejecuta fuera de tu infraestructura, y el
  estado de los despliegues vive en objetos Kubernetes que puedes inspeccionar
  con `kubectl`.

**Hacia dónde va:** [`docs/ROADMAP.md`](docs/ROADMAP.md) recoge en qué se está
trabajando, qué viene después y qué deliberadamente no está previsto.

**¿Quieres verlo en tu propio clúster?** [Solicita una demo](https://get.veloxsearch.ai/es#demo).

---

## ¿Es para ti?

**Probablemente encaja si…**

- quieres OpenSearch en tu propio Kubernetes, no un servicio de búsqueda alojado
- ejecutas k3s / k0s / kubeadm / minikube sobre hardware que tú controlas
- prefieres avanzar por un asistente antes que mantener a mano CRs del operador,
  políticas ISM, plantillas de índice y configuraciones de Fluent Bit

**Probablemente no encaja si…**

- necesitas un servicio gestionado en la nube — esto se instala en *tu* clúster
- tu clúster es **brownfield**: un operador de OpenSearch ya existente, o un
  cert-manager anterior a 1.16, queda fuera del alcance de la v1 y el instalador
  rehúsa en lugar de pelearse con él
- estás en **arm64**, Kubernetes **< 1.30**, OpenShift o nodos Windows
- necesitas elegir tu propia StorageClass — los despliegues están fijados a
  Longhorn a propósito
- necesitas instalaciones air-gapped — el bootstrap descarga imágenes de
  docker.io, quay.io y cr.fluentbit.io

**Requisitos, en una frase:** Kubernetes **≥ 1.30**, **amd64**, **≥ 8 GiB** de
RAM asignable y **2 vCPU** libres (12 GiB / 4 vCPU / 60 GB recomendados para un
nodo único cómodo), salida hacia registries, cluster-admin **solo en el momento
de la instalación**, y ningún operador de OpenSearch ya en marcha. Longhorn es el
único almacenamiento soportado para los despliegues; si a un nodo le faltan sus
paquetes, la interfaz nombra el nodo y te da el comando. El contrato completo —
cada requisito, su sonda y su mensaje de rechazo — está en
[`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md).

---

## Cómo funciona

```
     navegador
         │
    ┌────▼─────────────────────────┐
    │  veloxsearch (binario único) │   Rust · Axum · kube-rs
    │  SPA React servida en /      │   un Deployment, un Service
    └────┬─────────────────────────┘
         │  API de Kubernetes (RBAC acotado, propiedad comprobada)
    ┌────▼──────────────┬──────────────────┬──────────────────┐
    │ operador de       │ cert-manager     │ Longhorn         │
    │ OpenSearch        │ (certs webhook)  │ (PVCs)           │
    └────┬──────────────┴──────────────────┴──────────────────┘
         │  CRs OpenSearchCluster
    ┌────▼───────────────────────────────────────────────────┐
    │ por despliegue: nodos OpenSearch + Dashboards          │
    │ + agentes de recolección en el namespace del inquilino │
    └────────────────────────────────────────────────────────┘
```

El plano de control es un único binario con la SPA embebida. Habla con la API de
Kubernetes y con las APIs HTTP de OpenSearch y Dashboards de cada despliegue. El
estado del despliegue vive en el CR `OpenSearchCluster`, no en una base de datos,
de modo que el clúster sigue siendo la fuente de verdad. Los comportamientos
autogestionados y los permisos que cada uno exige están en
[`docs/PREMISES.md`](docs/PREMISES.md); los detalles internos, en
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

---

## Documentación

| | |
|---|---|
| [`docs/INSTALL.md`](docs/INSTALL.md) | De cero a la interfaz en marcha: instalación por plataforma, mirrors privados, side-load, primer arranque, dominio y TLS propios |
| [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) | El contrato de plataforma: R1–R8, sondas, mensajes de rechazo, plataformas probadas |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Cómo está montado el plano de control, y las dos convenciones que lo sostienen |
| [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) | El ciclo local, y cómo ejecutar las pruebas que necesitan Postgres o un checkout del registry |
| [`docs/DEPLOY.md`](docs/DEPLOY.md) | Construir y publicar un release; side-load air-gapped |
| [`docs/INSTALLER.md`](docs/INSTALLER.md) | La CLI `velox`, para instalaciones desde un mirror privado |
| [`docs/SECRETS.md`](docs/SECRETS.md) | Cada secreto que el plano de control lee o crea, y cómo rotarlo |
| [`docs/PREMISES.md`](docs/PREMISES.md) | Los comportamientos autogestionados y los permisos que cada uno exige |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Qué está planificado, qué está abierto y qué deliberadamente no se hará |
| [`docs/adr/README.md`](docs/adr/README.md) | Qué decidió cada número de ADR citado en el código |
| [`docs/integrations/`](docs/integrations/) | Formato de los paquetes de integración: esquema del manifiesto, interpolación, firma |
| [`CHANGELOG.md`](CHANGELOG.md) | Qué cambió en cada release |

Estructura: `src/` plano de control y la CLI `velox` · `frontend/` SPA React ·
`deploy/` manifiesto de instalación, Dockerfile, bundles de bootstrap,
plantillas de inquilino · `migrations/` esquema · `tests/*_check.py`
comprobaciones de aceptación ejecutables.

---

## Madurez

En producción para su propio autor, y deliberadamente estrecho en lugar de
ampliamente compatible: el perímetro de requisitos se mantiene pequeño para que
todo lo que hay dentro funcione, en vez de degradarse de formas interesantes
fuera de él.

La última ejecución registrada de la flota de conformidad fue contra la v0.8.0,
el 2026-08-25 (evidencia fila a fila en [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md)):
instalación → conformidad → rechazo verificados en vivo en k3s, k0s y un clúster
real de 3 nodos con Longhorn. Los dos fallos que encontró — despliegues de un
solo nodo atascados en el número de réplicas de Longhorn
([#26](https://github.com/tornis-tecnologia/veloxsearch-oss/issues/26)) y el
rolling restart posterior al verde bloqueado en la recuperación de shards
([#27](https://github.com/tornis-tecnologia/veloxsearch-oss/issues/27)) — se
corrigieron en la 0.8.1. Un lane de CI en trunk arranca la última imagen
publicada en minikube en cada push. La multi-tenencia está lo bastante completa
para funcionar, pero viene desactivada por defecto, y la pila de observabilidad
OpenTelemetry está publicada pero ha tenido poca ejercitación en el mundo real.

---

## Cómo contribuir

Las contribuciones son bienvenidas. Empieza por [`CONTRIBUTING.md`](CONTRIBUTING.md)
— cubre la configuración local, el sign-off DCO que necesita cada commit, y las
dos convenciones que este código sigue y que no son obvias desde fuera: la
propiedad la impone el sistema de tipos y no una comprobación, y los módulos que
deciden cosas deliberadamente no hacen ninguna llamada al clúster.

Tres puntos de partida que no requieren Rust:

- Issues con la etiqueta [`good first issue`](https://github.com/tornis-tecnologia/veloxsearch-oss/labels/good%20first%20issue)
- **Nuevas integraciones de registros** — una integración es un paquete de
  *datos* firmado, no código. Viven en
  [`veloxsearch-registry`](https://github.com/tornis-tecnologia/veloxsearch-registry)
- **Traducciones** — toda cadena de la interfaz está en `frontend/i18n.jsx`,
  en español, portugués e inglés

Lee el [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) antes de participar, y el
[`SECURITY.md`](SECURITY.md) antes de reportar cualquier cosa relacionada con la
seguridad — las vulnerabilidades van por advisory privado, nunca por issue
pública.

---

## Licencia

**GNU Affero General Public License v3.0 only** (`AGPL-3.0-only`). Texto
completo en [LICENSE](LICENSE); cada archivo fuente lleva la cabecera SPDX, y el
`Cargo.toml` declara lo mismo.

Qué significa en la práctica:

- **Ejecutarlo es libre** — interna o comercialmente, sin coste.
- **Modificarlo está permitido.**
- **La Sección 13 es la que hay que leer.** Si pones VeloxSearch a disposición
  de otros usuarios *a través de la red* — incluida una versión modificada —
  debes ofrecer a esos usuarios el código fuente completo correspondiente de la
  versión con la que están interactuando, bajo esta misma licencia. Para una
  herramienta cuyo propósito entero es ser una interfaz web que otras personas
  usan, esa cláusula es el punto, no una nota al pie.

Las dependencias son compatibles con la AGPL: MIT, Apache-2.0, BSD, ISC, Zlib,
Unicode-3.0 y CDLA-Permissive-2.0 del lado Rust; MIT, Apache-2.0, BSD-3-Clause,
0BSD, ISC y MPL-2.0 en el frontend, con MPL solo en herramientas de build.
Ningún código GPL-2.0-only, SSPL, BUSL o no comercial está enlazado.
Compruébalo tú mismo:

```bash
cargo install cargo-deny && cargo deny check licenses
```

Las contribuciones recibidas entran bajo la misma licencia, certificadas por un
sign-off [DCO](https://developercertificate.org/) en cada commit, en lugar de un
CLA. Ver [`CONTRIBUTING.md`](CONTRIBUTING.md).
