<div align="center">

<img src=".github/assets/logo.svg" width="92" alt="Logo do VeloxSearch" />

# VeloxSearch

**Transforma um cluster Kubernetes cru em uma plataforma OpenSearch gerenciada.**

Um control plane em Rust e uma interface React que instalam o OpenSearch, mantêm
ele rodando, e te dão um assistente no lugar de uma pasta de YAML.

[![CI](https://github.com/tornis-tecnologia/veloxsearch-oss/actions/workflows/ci.yml/badge.svg)](https://github.com/tornis-tecnologia/veloxsearch-oss/actions/workflows/ci.yml)
[![Licença: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)
[![Downloads no Docker](https://img.shields.io/docker/pulls/tornistecnologia/veloxsearch-oss?logo=docker&label=pulls)](https://hub.docker.com/r/tornistecnologia/veloxsearch-oss)
[![rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-dea584?logo=rust)](Cargo.toml)
[![kubernetes ≥ 1.30](https://img.shields.io/badge/kubernetes%20%E2%89%A5%201.30-326ce5?logo=kubernetes&logoColor=white)](docs/REQUIREMENTS.md)
[![DCO](https://img.shields.io/badge/DCO-required-8e44ad)](CONTRIBUTING.md)

*Read in English: [README.md](README.md) · Leer en español: [README.es.md](README.es.md)*

</div>

<img src=".github/assets/demo.gif" width="880" alt="Demo do VeloxSearch: criação da conta admin no primeiro acesso, a tela de conformidade do cluster, a visão geral e o catálogo de integrações de um deployment verde, a capacidade do cluster, e o assistente de criação parado na revisão" />

*Do primeiro acesso ao primeiro cluster: setup → conformidade → deployments →
integrações → o assistente de criação (parado na revisão — nada é provisionado
nesta gravação).*

---

Você aponta para um cluster, abre o navegador, e ele faz o resto: confere se o
cluster é capaz, instala o que falta (Longhorn, cert-manager, o operator do
OpenSearch), provisiona um deployment dimensionado por presets, liga a coleta de
logs, e depois cuida do trabalho do dia 2 — upgrades de versão, snapshots,
rotação de credenciais, isolamento por tenant.

---

## É para você?

**Provavelmente serve se…**

- você quer OpenSearch no seu próprio Kubernetes, não um serviço de busca hospedado
- você prefere clicar num assistente a manter CRs do operator, políticas ISM,
  index templates e configs do Fluent Bit na mão
- você roda k3s / k0s / kubeadm / minikube em hardware que você controla
- você quer coleta de logs para serviços comuns (nginx, postgres, kafka, eventos
  do Kubernetes, …) sem escrever as pipelines
- multi-tenancy importa: cada deployment ganha namespace, quota, NetworkPolicy e
  verificação de posse próprios

**Provavelmente não serve se…**

- você precisa de um serviço gerenciado na nuvem — isto instala no *seu* cluster
- seu cluster é **brownfield**: um operator OpenSearch já existente, ou um
  cert-manager anterior à 1.16, está fora do escopo da v1 e o instalador recusa
  em vez de brigar
- você está em **arm64**, Kubernetes **< 1.30**, OpenShift, ou nós Windows
- você precisa escolher a própria StorageClass — deployments são fixados no
  Longhorn de propósito (veja abaixo)
- você precisa de instalação air-gapped — o bootstrap puxa imagens de docker.io,
  quay.io e cr.fluentbit.io

Leia [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) antes de qualquer coisa, e
[`docs/adr/README.md`](docs/adr/README.md) se quiser saber se a estreiteza é
deliberada ou acidental. O primeiro é o contrato honesto: oito requisitos
numerados, o que cada sonda verifica, e exatamente o que o app diz quando o seu
cluster falha em um deles. Um cluster fora do envelope recebe uma recusa clara na
tela de conformidade — nunca uma instalação pela metade.

---

## O que você ganha de fato

| | |
|---|---|
| **Provisionamento guiado** | Assistente de 4 passos: propósito → tamanho → backup → revisão. Os presets de dimensionamento vêm do backend, não de uma caixa de texto |
| **Auto-bootstrap** | Instala cert-manager, o operator do OpenSearch e o Longhorn sozinho, e depois **revoga o próprio binding de cluster-admin** quando termina |
| **Operações do dia 2** | Upgrades de versão (um nó por vez, esperando o verde entre eles; recusa downgrade porque o operator não sabe voltar), repositórios e agendamentos de snapshot em S3, rotação da senha de admin |
| **Integrações de log** | Receitas de um clique para nginx, postgres, redis, mysql, traefik, mongo, rabbitmq, kafka, mais logs de cluster/pod e auditoria do Kubernetes. Pipeline de ingestão, index template, política ISM de retenção e o agente de coleta, juntos |
| **Stack de observabilidade** | Stack OpenTelemetry opcional por deployment — collector, Data Prepper, Cortex, Alertmanager — alimentando as telas de Observability |
| **Multi-tenancy** | Namespace, ResourceQuota, LimitRange e NetworkPolicy por tenant; toda rota de API é checada quanto à posse, e um nome que não é seu se lê como "não existe" |
| **Status honesto** | As telas de atividade explicam uma operação travada com fatos do cluster — qual shard, qual nó, há quanto tempo — em vez de um spinner |

---

## Requisitos, em uma frase

Kubernetes **≥ 1.30**, **amd64**, **≥ 8 GiB** de RAM alocável e **2 vCPU**
livres (12 GiB / 4 vCPU / 60 GB recomendados para um nó único confortável), saída
para registries, cluster-admin **apenas na hora da instalação**, e nenhum
operator OpenSearch já rodando.

O armazenamento é deliberadamente estreito: **Longhorn é o único armazenamento
suportado para deployments.** Se estiver ausente, o VeloxSearch o instala.
Provisionadores locais de nó (`local-path`, hostpath) são recusados porque um pod
OpenSearch reagendado perde os dados neles — um CSI padrão de terceiro também não
é aceito em silêncio. Se um nó não tiver `open-iscsi`, cliente NFS ou `dmsetup`, a
interface nomeia o nó e te dá o comando de instalação para a distribuição dele.

Tabela completa, com sondas e mensagens de falha: [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md).

---

## Experimente

```bash
kubectl apply -f https://github.com/tornis-tecnologia/veloxsearch-oss/releases/latest/download/install.yaml
kubectl -n veloxsearch-system port-forward svc/veloxsearch 3000:80
# abra http://localhost:3000 — crie a conta de admin, e o app assume dali
```

Essa URL é um **artefato de release**, não um branch: a imagem dentro dela está
fixada por digest, então o que você aplica hoje é o que você recebe se aplicar de
novo mês que vem. `releases/latest/` acompanha o release mais novo; para fixar
uma versão, use `releases/download/v0.7.1/install.yaml`. Aplicar o
`deploy/install.yaml` do `main` te dá o que estiver no HEAD naquele instante —
serve para desenvolvimento, não para um cluster que importa.

Um arquivo, sem credencial de registry — a imagem é
[`tornistecnologia/veloxsearch-oss`](https://hub.docker.com/r/tornistecnologia/veloxsearch-oss),
pública e puxada anonimamente — sem passo prévio de `velox init`. Num cluster com
IngressClass padrão — um k3s recém-instalado, por exemplo — um Ingress catch-all
também é criado, então ele responde em `http://<ip-do-nó>/` sem port-forward
nenhum.

O que acontece em seguida é automático: a tela de conformidade confere os oito
requisitos, depois instala o cert-manager e o operator sem perguntar. O Longhorn
chega quando você cria o primeiro deployment. A única coisa que pode te parar é um
nó sem os pacotes do Longhorn, e a interface te diz qual comando rodar.

Passo a passo por plataforma — minikube, k0s, k3s, kubeadm — mais o side-load
air-gapped e o caminho de desinstalação: [`docs/INSTALL.md`](docs/INSTALL.md).

---

## Como funciona

```
     navegador
         │
    ┌────▼─────────────────────────┐
    │  veloxsearch (binário único) │   Rust · Axum · kube-rs
    │  SPA React servida em /      │   um Deployment, um Service
    └────┬─────────────────────────┘
         │  API do Kubernetes (RBAC escopado, posse verificada)
    ┌────▼──────────────┬──────────────────┬──────────────────┐
    │ operator do       │ cert-manager     │ Longhorn         │
    │ OpenSearch        │ (certs webhook)  │ (PVCs)           │
    └────┬──────────────┴──────────────────┴──────────────────┘
         │  CRs OpenSearchCluster
    ┌────▼───────────────────────────────────────────────────┐
    │ por deployment: nós OpenSearch + Dashboards            │
    │ + agentes de coleta no namespace do tenant             │
    └────────────────────────────────────────────────────────┘
```

O control plane é um binário só, com a SPA embutida — não há frontend separado
para publicar. Ele fala com a API do Kubernetes e com as APIs HTTP do OpenSearch e
do Dashboards de cada deployment. O estado do deployment vive no CR
`OpenSearchCluster`, não num banco, então o cluster continua sendo a fonte da
verdade.

Os três comportamentos auto-gerenciados — quando o Longhorn é instalado, como o
bootstrap é controlado, e o modelo de namespaces — estão especificados em
[`docs/PREMISES.md`](docs/PREMISES.md), com cada afirmação citada em `arquivo:linha`.

---

## Documentação

| | |
|---|---|
| [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) | O contrato de plataforma: R1–R8, sondas, mensagens de recusa, plataformas testadas. **Comece aqui.** |
| [`docs/INSTALL.md`](docs/INSTALL.md) | Instalação por plataforma, modos de acesso, desinstalação |
| [`docs/ARCHITECTURE.pt-BR.md`](docs/ARCHITECTURE.pt-BR.md) | Como o control plane é montado, e as duas convenções que sustentam o projeto |
| [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) | O loop local, e como rodar os testes que precisam de Postgres ou de um checkout do registry |
| [`docs/DEPLOY.md`](docs/DEPLOY.md) | Construir e publicar um release; side-load air-gapped |
| [`docs/SECRETS.md`](docs/SECRETS.md) | Todo segredo que o control plane lê ou cria, e como rotacioná-lo |
| [`docs/PREMISES.md`](docs/PREMISES.md) | Os comportamentos auto-gerenciados e as permissões que cada um exige |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | O que está planejado, o que está em aberto, e o que deliberadamente não será feito |
| [`docs/adr/README.md`](docs/adr/README.md) | O que cada número de ADR citado no código decidiu |
| [`docs/integrations/`](docs/integrations/) | Formato dos pacotes de integração: schema do manifesto, interpolação, assinatura |
| `tests/*_check.py` | Verificações de aceitação executáveis — smoke, primeira execução, dia 2, jornada completa, e um gate de navegador com Playwright |

Estrutura: `src/` control plane e a CLI `velox` · `frontend/` SPA React ·
`deploy/` manifesto de instalação, Dockerfile, bundles de bootstrap, templates de
tenant · `migrations/` schema.

---

## Maturidade

Rodando em produção para o próprio autor, e deliberadamente estreito em vez de
amplamente compatível: o envelope de requisitos é mantido pequeno para que tudo
dentro dele funcione, em vez de degradar de formas interessantes fora dele.

Saiba de duas coisas antes de depender disso. A frota de conformidade — k3s
greenfield, k0s cru, e um cluster subdimensionado que precisa ser *recusado* — tem
execuções pendentes de reverificação contra o caminho de armazenamento atual; o
`docs/REQUIREMENTS.md` marca cada linha com quando ela foi de fato verificada, não
com quando se esperava que funcionasse. E a stack de observabilidade
OpenTelemetry está publicada mas teve exercício limitado no mundo real.

---

## Como contribuir

Contribuições são bem-vindas. Comece pelo [`CONTRIBUTING.pt-BR.md`](CONTRIBUTING.pt-BR.md)
— ele cobre o setup local, o sign-off DCO que todo commit precisa, e as duas
convenções que este código segue e que não são óbvias de fora: posse é imposta
pelo sistema de tipos e não por verificações, e os módulos que decidem coisas
deliberadamente não fazem nenhuma chamada ao cluster.

Três pontos de partida que não exigem Rust:

- Issues com a etiqueta [`good first issue`](https://github.com/tornis-tecnologia/veloxsearch-oss/labels/good%20first%20issue)
- **Novas integrações de log** — uma integração é um pacote de *dados* assinado,
  não código. Elas vivem em
  [`veloxsearch-registry`](https://github.com/tornis-tecnologia/veloxsearch-registry)
- **Traduções** — toda string da interface está em `frontend/i18n.jsx`

Leia o [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) antes de participar, e o
[`SECURITY.md`](SECURITY.md) antes de reportar qualquer coisa relacionada a
segurança — vulnerabilidades vão por advisory privado, nunca por issue pública.

---

## Licença

**GNU Affero General Public License v3.0 only** (`AGPL-3.0-only`). Texto completo
em [LICENSE](LICENSE); todo arquivo-fonte carrega o header SPDX, e o
`Cargo.toml` declara o mesmo.

O que isso significa na prática:

- **Rodar é livre** — internamente ou comercialmente, sem custo.
- **Modificar é permitido.**
- **A Seção 13 é a que importa ler.** Se você disponibilizar o VeloxSearch a
  outros usuários *pela rede* — inclusive uma versão modificada — você precisa
  oferecer a esses usuários o código-fonte completo correspondente da versão com
  a qual eles estão interagindo, sob esta mesma licença. Para uma ferramenta cujo
  propósito inteiro é ser uma interface web que outras pessoas usam, essa
  cláusula é o ponto, não uma nota de rodapé.

As dependências são compatíveis com a AGPL: MIT, Apache-2.0, BSD, ISC, Zlib,
Unicode-3.0 e CDLA-Permissive-2.0 do lado Rust; MIT, Apache-2.0, BSD-3-Clause,
0BSD, ISC e MPL-2.0 no frontend, com MPL apenas em ferramenta de build. Nenhum
código GPL-2.0-only, SSPL, BUSL ou não-comercial é linkado. Confira você mesmo:

```bash
cargo install cargo-deny && cargo deny check licenses
```

Contribuições recebidas entram sob a mesma licença, certificadas por um sign-off
[DCO](https://developercertificate.org/) em cada commit, em vez de um CLA. Veja o
[`CONTRIBUTING.pt-BR.md`](CONTRIBUTING.pt-BR.md).
