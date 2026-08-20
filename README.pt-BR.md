# VeloxSearch

[![CI](https://github.com/tornis-tecnologia/veloxsearch-oss/actions/workflows/ci.yml/badge.svg)](https://github.com/tornis-tecnologia/veloxsearch-oss/actions/workflows/ci.yml)
[![Licença: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)

**Transforme um cluster Kubernetes cru em uma plataforma OpenSearch gerenciada.**

*[Read in English](README.md)*

VeloxSearch é um control plane em Rust com uma interface React que provisiona,
atualiza e opera deployments OpenSearch em qualquer cluster conformante — k3s,
k0s, minikube, kubeadm puro. Ele cuida de planejamento de capacidade, bootstrap
de primeira execução, operações do dia 2 e receitas de integração de logs em um
comando (nginx, PostgreSQL, Kafka, eventos do Kubernetes, …).

Não é um Helm chart com uma página web por cima. O control plane é dono do ciclo
de vida: verifica se o cluster realmente consegue hospedar OpenSearch antes de
prometer qualquer coisa, instala os próprios pré-requisitos (cert-manager,
operator do OpenSearch, Longhorn quando não há StorageClass padrão utilizável),
dimensiona os node pools a partir da capacidade real do cluster e mantém os
deployments resultantes atualizáveis.

## Começando

Você precisa de um cluster Kubernetes (≥ 1.30) onde consiga rodar `kubectl
apply` e de cerca de 8 GiB de folga alocável. Requisitos completos:
[docs/REQUIREMENTS.md](docs/REQUIREMENTS.md).

```sh
kubectl apply -f https://raw.githubusercontent.com/tornis-tecnologia/veloxsearch-oss/main/deploy/install.yaml
kubectl -n veloxsearch-system port-forward svc/veloxsearch 3000:80
```

Abra <http://localhost:3000>, crie a conta de administrador, e o gate de
primeira execução verifica o cluster e instala sozinho o que estiver faltando.
Sem credencial de registry, sem `helm repo add`, sem values file.

O manifesto de instalação é um arquivo só e puxa uma imagem pública
(`docker.io/tornistecnologia/veloxsearch-oss`). Para instalações air-gapped,
passo a passo por distribuição e o caminho com mirror autenticado, veja
[docs/INSTALL.md](docs/INSTALL.md).

## O que você ganha

- **Um gate de conformidade antes de qualquer promessa.** A primeira execução
  confere o cluster contra um contrato escrito (R1–R8) e diz qual requisito
  falhou, em vez de deixar Pods Pending.
- **Dimensionamento consciente de capacidade.** Os node pools são propostos a
  partir do que o cluster tem de fato alocável, não de um tamanho fixo.
- **Operações do dia 2.** Snapshots, upgrades de versão, rotação de senha do
  admin, configuração de provedor LDAP/OIDC com sonda de alcançabilidade antes
  de salvar, e uma visão de atividade que responde "já estabilizou?".
- **Integrações de log como pacotes de dados assinados.** Uma integração é um
  manifesto mais assets — pipeline de ingestão, index template, dashboards,
  configuração do Fluent Bit. Nunca código. Todo pacote é assinado com ed25519 e
  verificado contra um keyring compilado no binário; não-assinado, chave
  desconhecida e adulterado são três rejeições duras e distintas.
- **Uma stack de coleta OpenTelemetry** como alternativa aditiva aos agentes
  baseados em receita.

## Estrutura do repositório

| Caminho | O que tem dentro |
| --- | --- |
| `src/` | O control plane e a CLI de operador `velox` (Rust, axum) |
| `frontend/` | A SPA React 18 + Vite |
| `deploy/` | `install.yaml`, o Dockerfile, manifestos de bootstrap vendorizados, templates de tenant |
| `docs/` | Instalação, requisitos, arquitetura, desenvolvimento, deploy, formato dos pacotes de integração |
| `migrations/` | SQL puro, aplicado pelo runner artesanal em `src/db.rs` |
| `tests/` | Verificações ponta a ponta (Python standalone, não um alvo de teste Rust) |
| `keys/` | Metades públicas das chaves que assinam pacotes de integração |

## Compilando do código-fonte

```sh
cargo build --release            # target/release/veloxsearch
cargo build --bin velox          # a CLI de operador
cargo test                       # os testes são módulos #[cfg(test)] inline

cd frontend && npm ci && npm run build
```

Loop de desenvolvimento local, build do container e como rodar os testes que
dependem de serviços externos: [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Documentação

| Documento | Para |
| --- | --- |
| [docs/INSTALL.md](docs/INSTALL.md) | Instalar em minikube, k3s, k0s, kubeadm |
| [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) | O contrato de plataformas suportadas (R1–R8) |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Como o control plane é montado |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | O loop de desenvolvimento local |
| [docs/DEPLOY.md](docs/DEPLOY.md) | Como construir e publicar um release |
| [docs/SECRETS.md](docs/SECRETS.md) | Todo segredo que o control plane lê, e de onde |
| [docs/ROADMAP.md](docs/ROADMAP.md) | O que está planejado, e o que está em aberto |
| [docs/integrations/](docs/integrations/) | Formato dos pacotes de integração, assinatura, interpolação |
| [docs/adr/](docs/adr/) | Registros de decisão de arquitetura |

## Como contribuir

Contribuições são bem-vindas — veja [CONTRIBUTING.pt-BR.md](CONTRIBUTING.pt-BR.md)
para o setup de desenvolvimento, as convenções que este código segue e o
sign-off DCO exigido. Bons pontos de partida:

- Issues com a etiqueta [`good first issue`](https://github.com/tornis-tecnologia/veloxsearch-oss/labels/good%20first%20issue)
- Novas integrações de log — elas vivem em
  [`veloxsearch-registry`](https://github.com/tornis-tecnologia/veloxsearch-registry)
  e são dados, não código, então não exigem Rust
- Traduções: todas as strings da interface estão em `frontend/i18n.jsx`

Leia o [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) antes de participar, e o
[SECURITY.md](SECURITY.md) antes de reportar qualquer coisa relacionada a
segurança — vulnerabilidades vão por advisory privado, não por issue pública.

## Licença

VeloxSearch é licenciado sob a
[GNU Affero General Public License v3.0 only](LICENSE). Se você rodar uma versão
modificada como serviço de rede, a AGPL exige que você ofereça o código-fonte
dela aos seus usuários. Veja o [NOTICE](NOTICE) para as licenças dos manifestos
upstream embutidos.
