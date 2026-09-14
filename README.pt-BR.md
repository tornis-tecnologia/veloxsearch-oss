<div align="center">

<img src=".github/assets/logo.svg" width="92" alt="Logo do VeloxSearch" />

# VeloxSearch

**Transforma um cluster Kubernetes cru em uma plataforma OpenSearch gerenciada.**

[![CI](https://github.com/tornis-tecnologia/veloxsearch-oss/actions/workflows/ci.yml/badge.svg)](https://github.com/tornis-tecnologia/veloxsearch-oss/actions/workflows/ci.yml)
[![Licença: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)
[![Downloads no Docker](https://img.shields.io/docker/pulls/tornistecnologia/veloxsearch-oss?logo=docker&label=pulls)](https://hub.docker.com/r/tornistecnologia/veloxsearch-oss)
[![rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-dea584?logo=rust)](Cargo.toml)
[![kubernetes ≥ 1.30](https://img.shields.io/badge/kubernetes%20%E2%89%A5%201.30-326ce5?logo=kubernetes&logoColor=white)](docs/REQUIREMENTS.md)
[![DCO](https://img.shields.io/badge/DCO-required-8e44ad)](CONTRIBUTING.md)

*Read in English: [README.md](README.md) · Leer en español: [README.es.md](README.es.md)*

</div>

O VeloxSearch é um control plane com interface web que roda dentro do seu próprio
cluster Kubernetes. Você aponta para o cluster e abre o navegador: ele confere se
o cluster dá conta, instala o que falta (cert-manager, o operator do OpenSearch,
Longhorn), cria deployments OpenSearch por um assistente de quatro passos, liga a
coleta de logs e depois cuida do trabalho do dia 2 — upgrades de versão,
snapshots, rotação de credenciais.

**Código aberto sob a [GNU AGPL-3.0-only](LICENSE).** Hospedar por conta própria
— para o seu time ou a sua empresa, inclusive com fins comerciais — é gratuito e
não depende de nada da nossa parte. A única obrigação: se você modificar o
VeloxSearch e deixar outras pessoas usarem a sua versão pela rede, precisa
oferecer a elas o código-fonte dessa versão. [Detalhes abaixo](#licença).

## Instalação

```bash
kubectl apply -f https://github.com/tornis-tecnologia/veloxsearch-oss/releases/latest/download/install.yaml
```

Depois, `kubectl -n veloxsearch-system port-forward svc/veloxsearch 3000:80`,
abra <http://localhost:3000> e crie a conta de admin — o app assume dali. Em um
cluster com IngressClass padrão (um k3s recém-instalado, por exemplo) ele também
responde em `http://<ip-do-nó>/`, sem port-forward.

> **Começando do zero, sem um cluster Kubernetes?** Siga o
> [`docs/INSTALL.md`](docs/INSTALL.md#0-no-kubernetes-cluster-yet) — de uma
> máquina Linux ou de um notebook até a interface no ar — ou o
> [guia do usuário no site](https://get.veloxsearch.ai/docs/pt).

## Telas

<table>
  <tr>
    <td width="50%"><a href=".github/assets/screens/conformity.png"><img src=".github/assets/screens/conformity.png" alt="Tela de conformidade: requisitos R1 a R8 todos aprovados em um cluster k3s de três nós, com cert-manager e o operator do OpenSearch na fila do bootstrap" /></a></td>
    <td width="50%"><a href=".github/assets/screens/deployment-overview.png"><img src=".github/assets/screens/deployment-overview.png" alt="Visão geral de um deployment verde chamado prod-logs: OpenSearch 3.8.0, três de três nós prontos, e os endereços do Dashboards e da API" /></a></td>
  </tr>
  <tr>
    <td align="center"><sub>O cluster é verificado antes de qualquer instalação</sub></td>
    <td align="center"><sub>Um deployment verde e seus endereços</sub></td>
  </tr>
  <tr>
    <td width="50%"><a href=".github/assets/screens/create-purpose.png"><img src=".github/assets/screens/create-purpose.png" alt="Assistente de criação, passo 1 de 4: nome do deployment e escolha do propósito — Observability, Security ou Search — com o que cada um retém, coleta e configura" /></a></td>
    <td width="50%"><a href=".github/assets/screens/create-review.png"><img src=".github/assets/screens/create-review.png" alt="Assistente de criação, passo de revisão: nome, propósito, tamanho (medium, três nós, 10 GiB) e backup, com o botão Create cluster" /></a></td>
  </tr>
  <tr>
    <td align="center"><sub>Criar, passo 1: o propósito define retenção e padrões</sub></td>
    <td align="center"><sub>Criar, passo 4: revisão antes de provisionar qualquer coisa</sub></td>
  </tr>
</table>

<sub>As capturas estão com a interface em inglês; ela também fala português e espanhol.</sub>

## Por que o VeloxSearch

- **Um assistente no lugar de uma pasta de YAML.** Propósito → tamanho →
  snapshot → revisão. Os presets de dimensionamento vêm do backend; o propósito
  escolhido já define retenção, detectores e padrões de índice para você.
- **Ele confere antes de mexer.** Oito requisitos numerados são testados logo de
  início. Um cluster fora do envelope recebe uma recusa clara, dizendo o que
  falhou — nunca uma instalação pela metade.
- **Instala os próprios pré-requisitos e depois devolve a chave.** cert-manager,
  o operator do OpenSearch e o Longhorn chegam sozinhos, e o app **revoga o
  próprio binding de cluster-admin** quando termina.
- **Logs chegando sem escrever pipeline.** Integrações de um clique para nginx,
  postgres, redis, mysql, traefik, mongo, rabbitmq, kafka e Kubernetes entregam
  juntos o pipeline de ingestão, o index template, a política de retenção e o
  agente de coleta.
- **O dia 2 já vem pronto.** Upgrades de versão um nó por vez (esperando o verde
  entre eles e recusando downgrades que o operator não sabe desfazer),
  agendamentos de snapshot em S3, rotação da senha de admin e uma stack
  OpenTelemetry opcional.
- **Status que se explica.** Uma operação travada é explicada com fatos do
  cluster — qual shard, qual nó, há quanto tempo — em vez de um spinner.
- **Seu cluster, seus dados.** Nada roda fora da sua infraestrutura, e o estado
  dos deployments fica em objetos Kubernetes que você inspeciona com `kubectl`.

**Para onde está indo:** o [`docs/ROADMAP.md`](docs/ROADMAP.md) mostra o que está
em andamento, o que vem depois e o que deliberadamente não está nos planos.

**Quer ver funcionando no seu próprio cluster?** [Solicite uma demonstração](https://get.veloxsearch.ai/pt#demo).

---

## É para você?

**Provavelmente serve se…**

- você quer OpenSearch no seu próprio Kubernetes, não um serviço de busca hospedado
- você roda k3s / k0s / kubeadm / minikube em hardware que você controla
- você prefere clicar num assistente a manter na mão CRs do operator, políticas
  ISM, index templates e configs de Fluent Bit

**Provavelmente não serve se…**

- você precisa de um serviço gerenciado em nuvem — isto instala no *seu* cluster
- seu cluster é **brownfield**: um operator de OpenSearch já existente, ou um
  cert-manager anterior ao 1.16, está fora do escopo da v1 e o instalador recusa
  em vez de brigar com ele
- você está em **arm64**, Kubernetes **< 1.30**, OpenShift ou nós Windows
- você precisa escolher sua própria StorageClass — os deployments são fixados no
  Longhorn de propósito
- você precisa de instalação air-gapped — o bootstrap baixa imagens do docker.io,
  quay.io e cr.fluentbit.io

**Requisitos, em uma frase:** Kubernetes **≥ 1.30**, **amd64**, **≥ 8 GiB** de
RAM alocável e **2 vCPU** livres (12 GiB / 4 vCPU / 60 GB recomendados para um nó
único confortável), saída para registries, cluster-admin **só na hora da
instalação**, e nenhum operator de OpenSearch já rodando. O Longhorn é o único
armazenamento suportado para deployments; se faltar algum pacote dele em um nó, a
interface diz qual nó e qual comando rodar. O contrato completo — cada requisito,
sua verificação e sua mensagem de recusa — está no
[`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md).

---

## Como funciona

```
      navegador
         │
    ┌────▼─────────────────────────┐
    │  veloxsearch (binário único) │   Rust · Axum · kube-rs
    │  SPA React servida em /      │   um Deployment, um Service
    └────┬─────────────────────────┘
         │  API do Kubernetes (RBAC restrito, posse checada)
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

O control plane é um binário único com a SPA embutida. Ele conversa com a API do
Kubernetes e com as APIs HTTP do OpenSearch e do Dashboards de cada deployment. O
estado do deployment vive no CR `OpenSearchCluster`, não num banco de dados, então
o cluster continua sendo a fonte da verdade. Os comportamentos autogerenciados e
as permissões de cada um estão no [`docs/PREMISES.md`](docs/PREMISES.md); os
detalhes internos, no [`docs/ARCHITECTURE.pt-BR.md`](docs/ARCHITECTURE.pt-BR.md).

---

## Documentação

| | |
|---|---|
| [`docs/INSTALL.md`](docs/INSTALL.md) | Do zero à interface no ar: instalação por plataforma, mirrors privados, side-load, primeiro acesso, domínio e TLS próprios |
| [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) | O contrato de plataforma: R1–R8, verificações, mensagens de recusa, plataformas testadas |
| [`docs/ARCHITECTURE.pt-BR.md`](docs/ARCHITECTURE.pt-BR.md) | Como o control plane é montado, e as duas convenções que sustentam tudo |
| [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) | O ciclo local, e como rodar os testes que precisam de Postgres ou de um checkout do registry |
| [`docs/DEPLOY.md`](docs/DEPLOY.md) | Construir e publicar um release; side-load air-gapped |
| [`docs/INSTALLER.md`](docs/INSTALLER.md) | A CLI `velox`, para instalações a partir de um mirror privado |
| [`docs/SECRETS.md`](docs/SECRETS.md) | Todo secret que o control plane lê ou cria, e como rotacionar |
| [`docs/PREMISES.md`](docs/PREMISES.md) | Os comportamentos autogerenciados e as permissões que cada um exige |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | O que está planejado, o que está em aberto, e o que deliberadamente não será feito |
| [`docs/adr/README.md`](docs/adr/README.md) | O que cada número de ADR citado no código decidiu |
| [`docs/integrations/`](docs/integrations/) | Formato dos pacotes de integração: schema do manifesto, interpolação, assinatura |
| [`CHANGELOG.md`](CHANGELOG.md) | O que mudou em cada release |

Estrutura: `src/` control plane e a CLI `velox` · `frontend/` SPA React ·
`deploy/` manifesto de instalação, Dockerfile, bundles de bootstrap, templates de
tenant · `migrations/` schema · `tests/*_check.py` verificações de aceitação
executáveis.

---

## Maturidade

Rodando em produção para o próprio autor, e deliberadamente estreito em vez de
amplamente compatível: o envelope de requisitos é mantido pequeno para que tudo
dentro dele funcione, em vez de degradar de formas interessantes fora dele.

A última rodada registrada da frota de conformidade foi contra a v0.8.0, em
2026-08-25 (evidência linha a linha no [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md)):
instalação → conformidade → recusa verificadas ao vivo em k3s, k0s e um cluster
real de 3 nós com Longhorn. As duas falhas que ela encontrou — deployments de nó
único travando na contagem de réplicas do Longhorn
([#26](https://github.com/tornis-tecnologia/veloxsearch-oss/issues/26)) e o
rolling restart pós-verde preso na recuperação de shards
([#27](https://github.com/tornis-tecnologia/veloxsearch-oss/issues/27)) — foram
corrigidas na 0.8.1. Uma lane de CI no trunk sobe a última imagem publicada no
minikube a cada push. O multi-tenancy está completo o bastante para rodar, mas
vem desligado por padrão, e a stack de observabilidade OpenTelemetry está
publicada mas teve pouco uso no mundo real.

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
