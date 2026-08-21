# Arquitetura

*[Read in English](ARCHITECTURE.md)*

VeloxSearch é um control plane em Rust (axum) com uma SPA React. Roda como um
único Pod em `veloxsearch-system`, fala com a API do Kubernetes usando uma
service account, e gerencia deployments OpenSearch através do operator do
OpenSearch para Kubernetes.

Este documento é o mapa. A justificativa de projeto de cada módulo está no
doc-comment no topo daquele módulo — esses comentários são densos de propósito e
são a fonte autoritativa.

## O formato de uma requisição

```
navegador ──► /api/*  ──► auth_guard ──► handler (api.rs) ──► Scope ──► Deployment
                          (cookie de      │                              │
                           sessão)        │                              ▼
                                          │                     k8s.rs (todas as escritas)
                                          │                              │
                                          └──► módulo puro ──────────────┘
                                               (snapshot, upgrade,
                                                otel_stack, …)

navegador ──► /*      ──► ServeDir (o build da SPA) ──► fallback index.html
```

O `main.rs` liga tudo nessa ordem: o router `/api`, depois o fallback `ServeDir`
da SPA, depois uma camada de middleware global `auth::auth_guard` sobre tudo.
Duas tarefas de fundo rodam ao lado: `metrics::run_sampler` e
`version_feed::run_poller`. A subida do Postgres acontece **antes** de servir e
encerra o processo em caso de falha — falha fechada, para que a aplicação nunca
sirva com um store meio migrado.

## A feature `ssr`

Tudo exceto `api` (os DTOs puros) fica atrás da feature `ssr`, que controla a
camada Kubernetes/OpenSearch inteira. `#[cfg(feature = "ssr")]` num módulo novo é
a norma, não a exceção. Os DTOs continuam compiláveis sozinhos para que se possa
raciocinar sobre o formato do protocolo sem arrastar o kube-rs junto.

`default = ["ssr"]`, então um `cargo build` simples dá o servidor.

## Mapa de módulos

| Módulo | Papel |
| --- | --- |
| `api.rs` | Todo handler JSON, a tabela de rotas (`routes()`) e os DTOs compartilhados |
| `k8s.rs` | A camada Kubernetes/OpenSearch — o maior módulo, e dono de **todas** as escritas no cluster |
| `scope.rs` | Imposição de posse: `Scope` → token de capacidade `Deployment` |
| `auth.rs` | Cookie de sessão, formato do token, `auth_guard` |
| `tenants.rs`, `db.rs`, `mail.rs`, `email_denylist.rs` | Contas do control plane (sobre Postgres, atrás de flag) |
| `auth_provider.rs`, `auth_probe.rs` | Geração de provedor LDAP/OIDC por deployment e a sonda de alcançabilidade antes de salvar |
| `bootstrap.rs` | Gate de conformidade da primeira execução; auto-instalação de cert-manager / operator |
| `recipes.rs`, `agents.rs`, `integrations.rs`, `catalog.rs` | Integrações de log: receitas embutidas, os agentes Fluent Bit, o motor de aplicação só-dados, o cliente do registry assinado |
| `otel_stack.rs` | A stack de coleta OpenTelemetry (segunda opção aditiva ao lado das receitas) |
| `capacity.rs`, `profiles.rs`, `metrics.rs`, `telemetry.rs`, `discovery.rs` | Dimensionamento, capacidade do cluster, amostragem de saúde, descoberta de telemetria existente |
| `snapshot.rs`, `upgrade.rs`, `activity.rs`, `provisioning.rs`, `version_feed.rs` | Dia 2: snapshots, upgrades de versão, "já estabilizou?", trabalho de provisionamento adiado |
| `access.rs` | Como usuários alcançam dashboards (port-forward vs. ingress), lido de um ConfigMap |
| `bin/velox.rs` | A CLI de operador `velox` (`velox init`) |

## As duas convenções que sustentam o projeto

### 1. Posse é um tipo, não uma verificação

O `scope.rs` é a razão pela qual uma verificação de autorização esquecida é um
erro de compilação, e não um incidente de segurança.

Um handler recebe uma `String` controlada pelo atacante. Para fazer qualquer
coisa com ela, precisa transformá-la em um `Deployment` — e a única forma de
cunhar um é através de um `Scope` derivado do cookie de sessão assinado.
`Deployment` tem campos privados, nenhum construtor público e nenhum
`From<&str>`. A camada Kubernetes recebe `&Deployment`:

```rust
k8s::delete_cluster(&req.name)                        // não compila
k8s::delete_cluster(&scope.require(&req.name).await?) // o único caminho
```

Seguem duas regras, ambas absolutas: **nunca** adicione um ponto de entrada que
aceite `&str` na camada Kubernetes, e **nunca** amplie os construtores de
`Deployment`. Qualquer uma das duas converte a garantia de volta em convenção,
em silêncio.

O `Scope::resolve` retorna `None` de forma idêntica para "não existe" e "é de
outra pessoa" — mesmo namespace, mesmo seletor de labels, mesmo caminho de
código. A propriedade anti-enumeração é por construção, não por cuidado, e
buscas devem ser alteradas de um jeito que a mantenha assim.

### 2. Módulo puro, e quem escreve é o `k8s.rs`

`snapshot`, `upgrade`, `auth_provider`, `provisioning`, `activity` e
`otel_stack` não fazem nenhuma chamada ao cluster. Eles renderizam artefatos
(manifestos, configurações, planos) e decidem regras; o `k8s.rs` aplica o
resultado. Essa separação é o que permite testar a maior parte da lógica
interessante sem cluster, sem mocks e sem fixtures além de dados puros.

Lógica nova pertence ao lado puro.

## Segurança fora do cluster

Rodar o binário numa máquina de desenvolvimento com um kubeconfig apontando para
um cluster real nunca pode dirigir aquele cluster. Dois mecanismos garantem
isso:

- O `k8s.rs` cai para o namespace `veloxsearch-dev`, escolhido deliberadamente
  porque não existe em lugar nenhum.
- O `ensure_namespace_exists` transforma toda escrita contra um namespace
  inexistente numa recusa barulhenta, em vez de um erro engolido em alguma
  camada acima.

Não "conserte" isso apontando para um namespace que existe.

## Convenções da API

- Leituras sem argumento são `GET`; todo o resto é `POST` com um corpo JSON cujos
  nomes de campo batem com o DTO.
- Sucesso devolve o DTO como JSON (200), ou um 200 vazio para resultados
  unitários.
- Erros devolvem `{"error": "<mensagem>"}` com 400 (validação), 401
  (credenciais) ou 500 (camada Kubernetes / OpenSearch).
- `login`, `logout` e `setup_admin` definem o cookie de sessão na resposta.
- A lista de deployments é transmitida por SSE em `GET /api/events`, a cada 3
  segundos.

Adicionar um endpoint significa quatro coisas, e o método HTTP precisa bater dos
dois lados: um DTO em `api.rs`, um handler em `api.rs`, uma linha em `routes()`,
e um wrapper em `frontend/api.jsx`.

## Frontend

React 18 + Vite puro. **Sem router e sem biblioteca de estado.** Arquivos `.jsx`
planos no nível de cima, não uma árvore `src/`.

O `app.jsx` é a raiz. Ele inicializa a partir de `GET /api/auth_state` e troca de
tela conforme o estado:

```
first_run                 → setup
!authenticated            → login
authenticated && !ready   → bootstrap
caso contrário            → a aplicação principal
```

Os deployments chegam pelo stream SSE. O `localStorage` guarda **apenas**
preferências de interface (tema, idioma) — nunca estado do servidor, nunca dados
falsos.

| Arquivo | Conteúdo |
| --- | --- |
| `api.jsx` | Todo wrapper REST mais os adaptadores de DTO (`adaptDeployment`, …) |
| `i18n.jsx` | Todas as strings da interface, pt + en. Texto de interface vai aqui, nunca inline numa tela |
| `ui.jsx` | Primitivas compartilhadas (Logo, Icon, Toast) |
| `views_*.jsx` | Um arquivo por tela; `views_deployment.jsx` é o grande |
| `styles.css` | O design system (custom properties CSS, IBM Plex, acento verde) |
| `tweaks-panel.jsx` | O painel de ajuste de design ao vivo |

Como os testes ponta a ponta dirigem widgets renderizados em vez de URLs,
renomear um campo de formulário ou remodelar uma estrutura `nav.tabs` pode
quebrar os `tests/*_check.py`. Confira-os quando remodelar uma tela.

## Pacotes de integração são dados

Uma integração é um manifesto mais assets — pipeline de ingestão, index template,
saved objects, configuração do Fluent Bit. Nunca é código, e duas propriedades
mantêm isso assim:

- **A interpolação é um conjunto fechado** de exatamente oito tokens
  (`integrations::CLOSED_TOKENS`, [integrations/interpolation.md](integrations/interpolation.md)).
- **A verificação de assinatura é a fronteira de segurança inteira**
  (`catalog::verify_package`, [integrations/signing.md](integrations/signing.md)).
  Não-assinado, chave desconhecida e adulterado são três rejeições duras e
  distintas, e o keyring confiável é compilado no binário, de modo que a
  verificação não precisa de rede.

Um registry degradado é um estado, não uma queda: um registry inalcançável ou não
autorizado devolve o último catálogo em cache marcado como obsoleto, ou o
catálogo de bootstrap embutido, sempre com um 200 e uma string de erro que a
interface pode mostrar.

## Persistência

Os `migrations/*.sql` são SQL puro aplicado pelo runner artesanal em `db.rs` —
deliberadamente o driver `tokio-postgres` cru, sem ORM. As migrações precisam
chegar ao topo antes de a aplicação servir. A camada de query (sqlx vs. diesel) é
uma decisão posterior e separada, que ainda não foi tomada; veja o
[ROADMAP.md](ROADMAP.md).

## Artefatos de deploy

| Caminho | O que é |
| --- | --- |
| `deploy/install.yaml` | O manifesto genérico de instalação em arquivo único (ADR-027) |
| `deploy/Dockerfile` | Só a imagem de runtime; espera `./veloxsearch` e `./dist/` prontos |
| `deploy/bootstrap/` | Manifestos vendorizados de cert-manager, operator do OpenSearch e Longhorn que o bootstrap aplica. Arquivos grandes e gerados — não edite à mão |
| `deploy/tenant-templates/` | O conjunto Namespace / ResourceQuota / LimitRange / NetworkPolicy por tenant |

## Configuração

Todo ajuste é uma variável de ambiente prefixada `VELOX_`. Feature flags nascem
**desligadas** e preservam o comportamento anterior quando não definidas. As
notáveis:

`VELOX_SITE_ADDR`, `VELOX_STATIC_DIR`, `VELOX_CONTROL_PLANE_NS`,
`VELOX_PG_ENABLED`, `VELOX_MULTITENANT_AUTH`, `VELOX_REGISTRY_URL`,
`VELOX_REGISTRY_TOKEN`, `VELOX_SESSION_SECRET`, `VELOX_COOKIE_SECURE`,
`VELOX_SMTP_*`. As que são segredos estão inventariadas em
[SECRETS.md](SECRETS.md).

## Testes

Os testes Rust são `#[cfg(test)] mod tests` inline no fim de cada módulo — não um
alvo Rust em `tests/`, porque `tests/` guarda os scripts Python de ponta a ponta.
`src/scope/tests.rs` é a única exceção separada.

Veja o [DEVELOPMENT.md](DEVELOPMENT.md) para rodar os que precisam de um Postgres
vivo ou de um checkout do registry.
