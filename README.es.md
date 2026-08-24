<div align="center">

<img src=".github/assets/logo.svg" width="92" alt="Logo de VeloxSearch" />

# VeloxSearch

**Convierte un clúster Kubernetes vacío en una plataforma OpenSearch
gestionada.**

Un plano de control en Rust y una interfaz React que instalan OpenSearch, lo
mantienen en marcha y te dan un asistente en lugar de una carpeta llena de YAML.

[![CI](https://github.com/tornis-tecnologia/veloxsearch-oss/actions/workflows/ci.yml/badge.svg)](https://github.com/tornis-tecnologia/veloxsearch-oss/actions/workflows/ci.yml)
[![Licencia: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)
[![Descargas en Docker](https://img.shields.io/docker/pulls/tornistecnologia/veloxsearch-oss?logo=docker&label=pulls)](https://hub.docker.com/r/tornistecnologia/veloxsearch-oss)
[![rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-dea584?logo=rust)](Cargo.toml)
[![kubernetes ≥ 1.30](https://img.shields.io/badge/kubernetes%20%E2%89%A5%201.30-326ce5?logo=kubernetes&logoColor=white)](docs/REQUIREMENTS.md)
[![DCO](https://img.shields.io/badge/DCO-required-8e44ad)](CONTRIBUTING.md)

*Read in English: [README.md](README.md) · Leia em português: [README.pt-BR.md](README.pt-BR.md)*

</div>

<img src=".github/assets/demo.gif" width="880" alt="Demo de VeloxSearch: creación de la cuenta de administrador en el primer acceso, la pantalla de conformidad del clúster, la visión general y el catálogo de integraciones de un despliegue verde, la capacidad del clúster y el asistente de creación detenido en la revisión" />

*Del primer acceso al primer clúster: setup → conformidad → despliegues →
integraciones → el asistente de creación (detenido en la revisión — nada se
aprovisiona en esta grabación).*

---

Lo apuntas a un clúster, abres el navegador y él hace el resto: comprueba que el
clúster es capaz, instala lo que falta (Longhorn, cert-manager, el operador de
OpenSearch), aprovisiona un despliegue dimensionado a partir de presets, conecta
la recolección de registros y después se ocupa del trabajo del día 2 —
actualizaciones de versión, snapshots, rotación de credenciales, aislamiento por
inquilino.

---

## ¿Es para ti?

**Probablemente encaja si…**

- quieres OpenSearch en tu propio Kubernetes, no un servicio de búsqueda alojado
- prefieres avanzar por un asistente antes que mantener a mano CRs del operador,
  políticas ISM, plantillas de índice y configuraciones de Fluent Bit
- ejecutas k3s / k0s / kubeadm / minikube sobre hardware que tú controlas
- quieres recolección de registros para servicios comunes (nginx, postgres,
  kafka, eventos de Kubernetes, …) sin escribir las canalizaciones
- la multi-tenencia importa: cada despliegue recibe su propio namespace, cuota,
  NetworkPolicy y comprobaciones de propiedad

**Probablemente no encaja si…**

- necesitas un servicio gestionado en la nube — esto se instala en *tu* clúster
- tu clúster es **brownfield**: un operador de OpenSearch ya existente, o un
  cert-manager anterior a 1.16, queda fuera del alcance de la v1 y el instalador
  rehúsa en lugar de pelearse con él
- estás en **arm64**, Kubernetes **< 1.30**, OpenShift o nodos Windows
- necesitas elegir tu propia StorageClass — los despliegues están fijados a
  Longhorn a propósito (ver abajo)
- necesitas instalaciones air-gapped — el bootstrap descarga imágenes de
  docker.io, quay.io y cr.fluentbit.io

Lee [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) antes que nada, y
[`docs/adr/README.md`](docs/adr/README.md) si quieres saber si esa estrechez es
deliberada o accidental. El primero es el contrato honesto: ocho requisitos
numerados, qué comprueba cada sonda y exactamente qué dice la aplicación cuando
tu clúster falla en uno. Un clúster fuera del perímetro recibe un rechazo claro
en la pantalla de conformidad — nunca una instalación a medias.

---

## Qué obtienes en concreto

| | |
|---|---|
| **Aprovisionamiento guiado** | Asistente de 4 pasos: propósito → tamaño → copia de seguridad → revisión. Los presets de dimensionamiento vienen del backend, no de una caja de texto |
| **Auto-bootstrap** | Instala cert-manager, el operador de OpenSearch y Longhorn por su cuenta, y después **revoca su propio binding de cluster-admin** al terminar |
| **Operaciones del día 2** | Actualizaciones de versión (un nodo cada vez, esperando el verde entre ellos; rechaza los downgrades porque el operador no sabe volver atrás), repositorios y programaciones de snapshot en S3, rotación de la contraseña de administrador |
| **Integraciones de registros** | Recetas de un clic para nginx, postgres, redis, mysql, traefik, mongo, rabbitmq, kafka, además de registros de clúster/pod y de auditoría de Kubernetes. Canalización de ingesta, plantilla de índice, política ISM de retención y el agente de recolección, juntos |
| **Pila de observabilidad** | Pila OpenTelemetry opcional por despliegue — collector, Data Prepper, Cortex, Alertmanager — alimentando las pantallas de Observability |
| **Multi-tenencia** | Namespace, ResourceQuota, LimitRange y NetworkPolicy por inquilino; toda ruta de la API comprueba la propiedad, y un nombre que no es tuyo se lee como "no existe" |
| **Estado honesto** | Las pantallas de actividad explican una operación atascada con hechos del clúster — qué shard, qué nodo, cuánto tiempo — en lugar de un spinner |

---

## Requisitos, en una frase

Kubernetes **≥ 1.30**, **amd64**, **≥ 8 GiB** de RAM asignable y **2 vCPU**
libres (12 GiB / 4 vCPU / 60 GB recomendados para un nodo único cómodo), salida
hacia registries, cluster-admin **solo en el momento de la instalación**, y
ningún operador de OpenSearch ya en marcha.

El almacenamiento es deliberadamente estrecho: **Longhorn es el único
almacenamiento soportado para los despliegues.** Si falta, VeloxSearch lo
instala. Los aprovisionadores locales al nodo (`local-path`, hostpath) se
rechazan porque un pod de OpenSearch reprogramado pierde sus datos con ellos — y
un CSI por defecto ajeno tampoco se acepta en silencio. Si a un nodo le falta
`open-iscsi`, un cliente NFS o `dmsetup`, la interfaz nombra el nodo y te da el
comando de instalación para su distribución.

Tabla completa, con sondas y mensajes de fallo: [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md).

---

## Pruébalo

```bash
kubectl apply -f https://github.com/tornis-tecnologia/veloxsearch-oss/releases/latest/download/install.yaml
kubectl -n veloxsearch-system port-forward svc/veloxsearch 3000:80
# abre http://localhost:3000 — crea la cuenta de administrador, y la app sigue sola
```

Esa URL es un **artefacto de release**, no una rama: la imagen que contiene está
fijada por digest, así que lo que aplicas hoy es lo que recibes si lo aplicas de
nuevo el mes que viene. `releases/latest/` sigue el release más reciente; para
fijar una versión usa `releases/download/v0.7.1/install.yaml`. Aplicar el
`deploy/install.yaml` de `main` te da lo que haya en HEAD en ese instante — vale
para desarrollo, no para un clúster que te importe.

Un archivo, sin credenciales de registry — la imagen es
[`tornistecnologia/veloxsearch-oss`](https://hub.docker.com/r/tornistecnologia/veloxsearch-oss),
pública y descargada de forma anónima — sin paso previo de `velox init`. En un
clúster con IngressClass por defecto — un k3s recién instalado, por ejemplo — se
crea además un Ingress catch-all, así que responde en `http://<ip-del-nodo>/`
sin ningún port-forward.

Lo que viene después es automático: la pantalla de conformidad comprueba los
ocho requisitos y luego instala cert-manager y el operador sin preguntar.
Longhorn llega cuando creas tu primer despliegue. Lo único que puede detenerte
es un nodo al que le falten los paquetes de Longhorn, y la interfaz te dice qué
comando ejecutar.

Guías paso a paso por plataforma — minikube, k0s, k3s, kubeadm — más el
side-load air-gapped y el camino de desinstalación:
[`docs/INSTALL.md`](docs/INSTALL.md).

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

El plano de control es un único binario con la SPA embebida — no hay frontend
aparte que desplegar. Habla con la API de Kubernetes y con las APIs HTTP de
OpenSearch y Dashboards de cada despliegue. El estado del despliegue vive en el
CR `OpenSearchCluster`, no en una base de datos, de modo que el clúster sigue
siendo la fuente de verdad.

Los tres comportamientos autogestionados — cuándo se instala Longhorn, cómo se
controla el bootstrap y el modelo de namespaces — están especificados en
[`docs/PREMISES.md`](docs/PREMISES.md), con cada afirmación citada como
`archivo:línea`.

---

## Documentación

| | |
|---|---|
| [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) | El contrato de plataforma: R1–R8, sondas, mensajes de rechazo, plataformas probadas. **Empieza aquí.** |
| [`docs/INSTALL.md`](docs/INSTALL.md) | Instalación por plataforma, modos de acceso, desinstalación |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Cómo está montado el plano de control, y las dos convenciones que lo sostienen |
| [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) | El ciclo local, y cómo ejecutar las pruebas que necesitan Postgres o un checkout del registry |
| [`docs/DEPLOY.md`](docs/DEPLOY.md) | Construir y publicar un release; side-load air-gapped |
| [`docs/SECRETS.md`](docs/SECRETS.md) | Cada secreto que el plano de control lee o crea, y cómo rotarlo |
| [`docs/PREMISES.md`](docs/PREMISES.md) | Los comportamientos autogestionados y los permisos que cada uno exige |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Qué está planificado, qué está abierto y qué deliberadamente no se hará |
| [`docs/adr/README.md`](docs/adr/README.md) | Qué decidió cada número de ADR citado en el código |
| [`docs/integrations/`](docs/integrations/) | Formato de los paquetes de integración: esquema del manifiesto, interpolación, firma |
| `tests/*_check.py` | Comprobaciones de aceptación ejecutables — smoke, primer arranque, día 2, recorrido completo y una verificación de navegador con Playwright |

Estructura: `src/` plano de control y la CLI `velox` · `frontend/` SPA React ·
`deploy/` manifiesto de instalación, Dockerfile, bundles de bootstrap,
plantillas de inquilino · `migrations/` esquema.

---

## Madurez

En producción para su propio autor, y deliberadamente estrecho en lugar de
ampliamente compatible: el perímetro de requisitos se mantiene pequeño para que
todo lo que hay dentro funcione, en vez de degradarse de formas interesantes
fuera de él.

Ten en cuenta dos cosas antes de depender de esto. La flota de conformidad — k3s
greenfield, k0s desnudo y un clúster infradimensionado que debe ser *rechazado* —
tiene ejecuciones pendientes de reverificación contra el camino de
almacenamiento actual; `docs/REQUIREMENTS.md` marca cada fila con cuándo se
verificó realmente, no con cuándo se esperaba que funcionara. Y la pila de
observabilidad OpenTelemetry está publicada pero ha tenido poca ejercitación en
el mundo real.

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
- **Traducciones** — toda cadena de la interfaz está en `frontend/i18n.jsx`.
  Vale decirlo con claridad: **la interfaz todavía no habla español.** Hoy son
  portugués e inglés, aunque el catálogo de integraciones ya trae títulos y
  descripciones en español. Añadir el juego de claves `_es` en ese archivo es
  una contribución autocontenida y no toca ninguna pantalla

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
