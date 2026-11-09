# Velox Mesh e Velox Agent — arquitetura

**Status:** proposta, 2026-11-09. Decisões aqui são para discutir e derrubar
antes da fase 1 do [plano](PLAN.md). O que já está decidido de fato é a
licença (AGPL-3.0-only, a mesma do core) e a separação em workspace próprio
(§1).

Este documento é o *o quê*. O levantamento das referências (Fleet, Alloy,
Vector, OpAMP), a crítica ao Fluent Bit e o cronograma estão em
[PLAN.md](PLAN.md).

## 0. Em uma tela

```
 HOSTS (VM, bare metal, edge)              NÓS KUBERNETES
 ┌────────────────────────────────┐        ┌─────────────────────────────────────────┐
 │ velox-agent  supervisiona:     │        │ DaemonSet velox, um pod por nó:          │
 │   fluent-bit: logs, métricas   │        │   fluent-bit + velox-agent (sidecar)     │
 │   do host, relé OTLP das apps  │        │   mesmas integrações, mesma política     │
 └───────────────┬────────────────┘        └────────────────────┬────────────────────┘
                 │                                              │
   controle ─────┴──── OpAMP /v1/opamp ──► velox-mesh ◄─────────┘
                                           Postgres: agentes, políticas, tokens, licença
                                           ◄── console /api/mesh/* (via core)
   dados:
     logs, nível 0   ── direto ─────────────────────────────────────────► OpenSearch
     logs, nível 1   ── HTTP+TLS+ack ──► gateway ──► fila ──► workers ────► OpenSearch
                                          │
                                          ├─ Kafka:  Data Prepper ─► Strimzi/cliente ─► Data Prepper
                                          └─ Valkey: velox-queue relay ─► Valkey ─► velox-queue drain
                        compat (opcional): Fluent Bit in_forward/in_syslog ─► mesmo gateway
     métricas        ── Prometheus remote write ───────────────────────► Cortex
     traces          ── OTLP ──► otelcol gateway ──► Data Prepper ─────► OpenSearch
```

Três planos, separados de propósito:

| Plano | Quem | Volume | Estado |
| --- | --- | --- | --- |
| **Controle** | Velox Mesh ↔ Velox Agent (OpAMP) | minúsculo: 1 mensagem/agente/30 s | Postgres |
| **Dados** | Fluent Bit no host e no nó → OpenSearch (logs) e Cortex (métricas), direto ou via gateway + fila; traces via OTel no cluster | tudo | buffer em disco no agente; fila no cluster |
| **Gestão** | console → `/api/mesh/*` | humano | Postgres |

O plano de controle **nunca** vê um registro de log. Isso é o que permite um
Mesh pequeno gerenciar dezenas de milhares de agentes (Fleet Server: 100 mil
agentes em 16 vCPU) e o que permite o plano de dados escalar sozinho.

## 1. Licença e fronteira de compilação

Tudo em `veloxagent/` é `AGPL-3.0-only`, como o core ([LICENSE](../../LICENSE)
na raiz, ADR-068). Não há fronteira de licença. Há uma fronteira de
**compilação**, mantida por razões de engenharia:

1. **Workspace e binários separados.** `velox-mesh` e `velox-agent` formam um
   workspace Cargo próprio (`veloxagent/Cargo.toml`), compilam em binários e
   imagens próprias e falam com o core por HTTP. Motivos: o agente é um
   binário estático (musl) para hosts fora do cluster e não pode arrastar
   `kube`, axum do console nem a SPA; o Mesh escala e é atualizado no seu
   próprio ritmo, e um upgrade do core não reinicia o controlador da frota; o
   ciclo de release do agente (pacotes `.deb`/`.rpm`, rollout por percentual)
   não é o do console.
2. **O que os dois lados compartilham vira crate comum.** O renderer de
   integrações (pacote + variáveis → trecho de config do coletor) hoje vive no
   core, em `src/agents.rs` e `src/integrations.rs`. Ele sai do core como
   crate `velox-integrations` (fase 0 do plano), também AGPL, e os dois
   workspaces dependem dela. Como a licença é a mesma, a extração não exige
   auditoria de autoria nem relicenciamento: é refatoração.
3. **Contribuições.** Mesmo regime do resto do repositório (DCO). Se um dia a
   Tornis quiser vender uma licença comercial do mesmo código a quem não
   aceita AGPL, aí precisa de CLA ou cessão; isso vale para o repositório
   inteiro, não só para esta pasta.

Consequência para o modelo de negócio: o limite de 10 agentes (§6) não é
trava. Qualquer um pode remover a checagem e rodar o resultado, e a AGPL
garante esse direito. O limite marca o que a build oficial e a assinatura de
suporte cobrem. A receita vem de suporte, build oficial assinada, serviço
gerenciado e licença comercial para quem não quer AGPL, o modelo do Wazuh e
do Grafana (PLAN.md §6).

## 2. Vocabulário

| Termo | O que é |
| --- | --- |
| **Velox Mesh** | o controlador central (binário `velox-mesh`). Elastic chama de Fleet Server; Grafana, de Fleet Management. |
| **Velox Agent** | o supervisor instalado no host (binário `velox-agent`). Não coleta nada: roda e configura os *engines*. |
| **Engine** | processo filho que coleta. No host: **Fluent Bit** (logs, métricas do host, relé OTLP). **OpenTelemetry Collector** só por exceção no host (§5.5); o normal é ele rodar no cluster. |
| **Integração** | pacote de dados assinado (ADR-039), open source, idêntico ao que o DaemonSet do core usa hoje. |
| **Ativação** | uma integração ligada numa política, com suas variáveis. |
| **Política** | conjunto nomeado e versionado de ativações + saídas + ajustes do agente. Revisões imutáveis. |
| **Saída** | para onde os dados vão: um deployment OpenSearch direto, ou um Gateway. |
| **Token de matrícula** | segredo reutilizável e revogável que liga um agente novo a uma política (Fleet: *enrollment token*). |
| **Assento** | um agente ativo contando contra a assinatura (§6). |
| **Gateway** | camada opcional de agregação que recebe dos agentes e grava na fila (§5). |

## 3. Desacoplamento integrações ↔ Velox Mesh

O que o cliente precisa saber: *uma integração escrita para o VeloxSearch open
source funciona sem alteração sob o Velox Mesh*. O que o time precisa manter:

```
 veloxsearch-registry (dados, assinados)          core                   veloxagent/
 ┌────────────────────────────────┐   ┌──────────────────────────┐  ┌──────────────────────┐
 │ integrations/nginx/            │   │ catalog.rs  verifica     │  │ velox-policy         │
 │   manifest.yaml                │──►│ integrations.rs  Package │  │   compile(policy,    │
 │   agent.conf.tmpl  (Fluent Bit)│   │                          │  │     &dyn Renderer)   │
 │   pipeline.json, template.json │   │ velox-integrations (crate comum, extraída)         │
 │   dashboard.ndjson             │   │   render(package, vars, outputs) -> Vec<Fragment>  │
 └────────────────────────────────┘   └──────────────────────────┴──────────────────────────┘
```

* O **renderer** é um só, usado pelos dois lados. O core o usa para montar o
  ConfigMap do DaemonSet; o Mesh o usa para montar a config por política.
  Divergência entre "o que a integração faz no DaemonSet" e "no Mesh" fica
  impossível por construção, o mesmo argumento do `manifests()` único em `otel_stack.rs`.
* O **conjunto fechado de oito tokens** de interpolação (`CLOSED_TOKENS`) é do
  renderer e continua fechado. O Mesh não ganha tokens novos; se uma política
  precisar de uma variável nova, é mudança de especificação do registry.
* A **verificação de assinatura** continua sendo a fronteira de segurança
  inteira. O Mesh só compila pacotes que `catalog::verify_package` aceitou,
  pelo mesmo keyring compilado.
* O esquema **1.1** do manifesto traz as duas extensões que as políticas
  pedem: `variables` (nome, tipo, default, obrigatório) e o asset opcional
  `otel_config`. Variáveis não são tokens de interpolação: o template as
  referencia como `${name}` e o engine substitui só os nomes declarados
  (`docs/integrations/interpolation.md`). Pacotes 1.0 continuam válidos (sem
  variáveis, só Fluent Bit), e o caminho DaemonSet do core preenche os
  defaults — um `required` sem default torna o pacote não instalável ali, de
  propósito.

`velox-policy::FragmentRenderer` é a assinatura dessa fronteira; o fake nos
testes dela mostra o contrato mínimo.

## 4. Velox Mesh

### 4.1 Processo e estado

* Rust, axum, mesmas convenções do core (env `VELOX_MESH_*`, erros `{"error"}`,
  módulos puros + um módulo que escreve). Sem estado em memória que não possa
  ser perdido: réplicas são intercambiáveis atrás de um Service, como o Fleet
  Server.
* **Postgres** é o único estado (o core já traz Postgres opcional; o Mesh o
  torna obrigatório para si). Migrações SQL planas em `veloxagent/migrations/`, mesmo
  runner feito à mão do core. Tabelas: `mesh_agents`, `mesh_policies`,
  `mesh_policy_revisions`, `mesh_enrollment_tokens`, `mesh_actions`,
  `mesh_license`, todas com `tenant_id`.
* **Multi-tenant por tipo**, como `scope.rs`: um handler que nomeia um agente
  transforma a `String` em `AgentRef` só via `Scope`. Mesma propriedade
  anti-enumeração: "não existe" e "é de outro tenant" são a mesma resposta.
* Check-in barato de propósito: `last_seen` é coalescido em memória e gravado
  em lote a cada 10 s; a política compilada fica em cache por `(revisão)`; o
  token de matrícula é guardado como hash e o agente recebe uma credencial
  própria depois da matrícula (Fleet separa *communication key* de *output
  key*; aqui, credencial de controle ≠ credencial de dados).

### 4.2 Modelo

* **Token de matrícula** → aponta para uma política, é reutilizável, revogável,
  com expiração opcional. Expirar bloqueia matrículas novas e não afeta agentes
  já matriculados (Fleet faz igual e está certo).
* **Agente**: `instance_uid` estável (gerado e persistido no host), atributos
  (`velox.os`, `velox.arch`, `velox.agent.version` reservados; o resto é do
  operador: `env=prod`, `team=payments`), política atribuída, revisão aplicada,
  hash ecoado, saúde por componente, `last_seen`.
* **Atribuição de política**: direta (pelo token) **ou por rótulos** (regra
  "agentes com `env=prod` e `role=web` recebem a política X"). Alloy atribui
  por atributos no servidor; Fleet só por token. Ter os dois evita re-matricular
  para trocar de política.
* **Revisões imutáveis**: editar cria N+1; a UI mostra "12 de 14 na revisão 7"
  a partir do hash ecoado, não de um contador.
* **Ações** (`mesh_actions`): fila no Postgres com *lease*; tipos `upgrade`,
  `unenroll`, `diagnostics`, `restart`. Entregues no check-in, confirmadas pelo
  agente (ack), com limite de taxa por Mesh (Fleet tem `action_limit`).
* **Status** (derivado, nunca gravado): `healthy`, `degraded` (um engine
  doente), `applying`, `failed` (config recusada; a mensagem do engine fica
  visível), `offline` (sem check-in por 5 min, fixo, igual ao Fleet), `parked`
  (sem assento, §6), `unenrolled`.

### 4.3 Protocolo: OpAMP sobre HTTP

Agente inicia sempre; nada entra no host. `POST /v1/opamp` a cada 30 s ou
quando há algo a dizer; resposta carrega config nova quando o hash muda.
Mensagens em `velox-opamp`. WebSocket fica para depois, se latência de
entrega importar (upgrade imediato em frota grande). Credencial do agente
no header `Authorization: Bearer`, rotacionável pelo próprio canal
(OpAMP *connection settings*).

### 4.4 Compilação de política

`velox-policy::compile` → `Compiled { bodies: {fluent-bit, otelcol}, hash }`.
Regras que o compilador impõe sempre, por cima das integrações:

* seção `service` do Fluent Bit com `hot_reload: on`, `http_server: on` em
  loopback, `health_check: on`, `storage.type: filesystem`, `storage.path`,
  `storage.total_limit_size` da política;
* credenciais **nunca** no corpo: `${VELOX_OUTPUT_PASSWORD}` e afins, mesma
  técnica do `PASSWORD_REF` do core; o valor chega por outro canal e vive em
  arquivo `0600` do agente;
* um engine ausente em `bodies` é um engine que o agente deve parar.

### 4.5 API de gestão e UI

`/api/mesh/*` no Mesh, autenticado por token de serviço que o core emite para
a sessão (o core continua dono do login). A UI é um **bundle separado**
(`veloxagent/frontend`, Vite), carregado pela SPA por `import()` dinâmico
quando `GET /api/features` diz `mesh: true`. Quem não instala o Mesh não
baixa o bundle; o shell da ADR-067 ganha rotas `/mesh/agents`, `/mesh/policies`,
`/mesh/enrollment` e `/settings/license`. Mesmas primitivas (`ui.jsx`), mesmo
`i18n` por chave, mesmos `data-testid`.

## 5. Plano de dados

### 5.1 Nível 0 — direto (padrão)

Fluent Bit → `out_opensearch` do deployment, com buffer `filesystem` no host.
Simples, sem peças novas, é o que o DaemonSet do core faz hoje. Serve até
dezenas de milhares de eventos por segundo por deployment. Limite real: o
`_bulk` do OpenSearch, não o agente. Garantia: *at-least-once por salto*;
o `out_opensearch` não tem ack de ponta a ponta (confirmado na doc).

### 5.2 Nível 1 — Gateway + fila

Para "altíssima volumetria", o desenho é *stream-based*: uma fila durável
entre quem coleta e quem indexa. A fila tem **dois backends à escolha do
cliente**, com o mesmo contrato de entrada e os mesmos agentes:

```
agentes ──HTTP+TLS+ack──► gateway (stateless, N pods) ──► fila ──► workers ──► OpenSearch
  out_http, JSON gzip         │
                              ├─ Kafka:  Data Prepper (fonte http → KafkaBuffer) ─► Data Prepper (fonte kafka)
                              └─ Valkey: velox-queue relay (XADD)              ─► velox-queue drain (XREADGROUP)

compatibilidade (opcional): Fluent Bit  in_forward (TLS, ack) / in_syslog ──out_http──► mesmo gateway
```

* **Agente → Gateway** por HTTP sobre TLS: `out_http` com JSON comprimido
  (gzip), um token por agente emitido pelo Mesh e revogável. O gateway
  responde 2xx **só depois** de a fila confirmar a escrita, e o Fluent Bit só
  descarta o chunk ao receber 2xx; o *at-least-once* por salto se mantém sem
  o protocolo *forward*. O agente não precisa de credencial do OpenSearch: só
  do gateway. Isso remove o maior problema operacional do nível 0 (senha de
  cluster em milhares de hosts). A política diz `Output::Gateway`; **qual
  fila está atrás é configuração do gateway, não da política**. Trocar de
  backend não muda nenhum agente.
* **O gateway é a peça de entrada do próprio backend**, não um Fluent Bit no
  meio (decidido em 2026-11-09). Com Kafka é o Data Prepper; com Valkey é o
  `velox-queue relay`. Um salto e um buffer a menos, e o ack vem de quem
  escreveu na fila. Verificado no código do Data Prepper 2.17: a fonte `http`
  só devolve 200 depois da escrita no buffer, e o `KafkaBuffer` escreve de
  forma síncrona (`send(...).get()`), então o 200 é o ack do broker.
* **Entrada de compatibilidade**: um Fluent Bit opcional, sem estado, com
  `in_forward` (TLS, `require_ack_response`) e `in_syslog`, saindo por
  `out_http` para o mesmo gateway. É por aqui que entram frotas Fluentd, o
  Fluent Operator e o Logging Operator (§5.4) e syslog-ng/AxoSyslog. Não está
  no caminho dos agentes Velox; quem não tem essas frotas não o instala.
* **Backpressure** de ponta a ponta, nos dois backends: OpenSearch 429 →
  worker reduz → fila cresce → acima da marca d'água a fila recusa → gateway
  recusa ack → agente segura no buffer de disco → `storage.total_limit_size`
  descarta o mais antigo **e isso aparece na métrica do agente**. Em nenhum
  ponto a perda é silenciosa.
* **Observabilidade do pipeline**: o agente raspa `/api/v2/metrics/prometheus`
  do Fluent Bit e envia por OpAMP (*own telemetry*); gateway, fila e workers
  expõem Prometheus; o Mesh mostra entrada/saída/retries/descartes/lag por
  agente e por deployment.

#### 5.2.1 Backend Kafka (alto volume, retenção, replay)

* **Qualquer coisa que fale a API Kafka.** Instalado pelo core: **Apache Kafka
  via Strimzi** (CNCF), modo KRaft, no padrão de bootstrap de
  cert-manager/Longhorn. Ou o Kafka do cliente: MSK, Confluent, Aiven,
  Redpanda. Redpanda funciona por compatibilidade de API, mas não é o que o
  produto instala: núcleo BSL e governança de uma empresa só.
* **Dois Deployments de OpenSearch Data Prepper**, o ingestor do próprio
  projeto OpenSearch (Apache-2.0, OpenSearch Software Foundation), já usado
  pelo core para traces (`src/otel_stack.rs`).
  * **Gateway**: fonte `http` → `KafkaBuffer` com `acks=all`; tópico por
    deployment (`velox.<dep>.logs`); `min.insync.replicas=2`. Escala por CPU
    (HPA). Um tenant barulhento não atrasa o outro; cotas do broker por
    cliente. Retenção de horas: amortecedor e *replay*, não armazenamento.
  * **Workers**: fonte `kafka` → *sink* OpenSearch com DLQ; um grupo de
    consumo por tópico; escala por *lag* (KEDA).
* Por que não OTel Collector nos workers: o `opensearchexporter` é *alpha*
  para logs e procura mantenedor; o Data Prepper tem o *sink* OpenSearch GA e
  o DLQ pronto. Reavaliar quando o exporter chegar a *beta* (PLAN.md, marcos).
* Garantia: sem perda com uma falha de broker (`acks=all` + ISR 2).

#### 5.2.2 Backend Valkey/Redis Streams (volume moderado, operação simples)

Para o cliente que já opera Redis ou não quer operar Kafka. Instalado pelo
core: **Valkey** (Linux Foundation, BSD-3). Ou o do cliente: Redis ≥ 6.2,
ElastiCache, Memorystore; falamos só o protocolo RESP e comandos de Streams.

**Nada do plano de dados fala Redis Streams.** Verificado em 2026-11-09:
Fluent Bit não tem entrada nem saída Redis; Data Prepper não tem fonte Redis;
o `redisreceiver` do OTel só coleta métricas do servidor. Por isso este
backend tem uma peça nossa, em Rust, no workspace `veloxagent/`:
`velox-queue`, um binário com dois modos.

* **`velox-queue relay`** é o gateway deste backend: recebe dos agentes por
  HTTP (JSON, gzip, token por agente) e faz `XADD`. Responde 2xx **só
  depois** do `XADD`, então o ack continua de ponta a ponta por salto. Acima da marca d'água (memória usada ou *lag* do grupo)
  responde 503 e o backpressure sobe até o agente.
* **`velox-queue drain`** faz `XREADGROUP` → `_bulk` → `XACK` só do que o
  OpenSearch aceitou. Documento rejeitado por mapeamento vai para um stream
  DLQ (`velox:{<dep>}:dlq`); 429 é backoff sem ack; `XAUTOCLAIM` recupera
  pendentes de consumidor morto. Escala pelo *scaler* Redis Streams do KEDA
  (entradas pendentes).
* **Streams por deployment, particionados**: `velox:{<dep>.<n>}:logs`, N
  partições por hash da origem. Um stream é uma chave e mora num shard só;
  sem partição, um deployment não escala além de um núcleo do Valkey.
* **Regras obrigatórias**, verificadas pelo relay na partida, que se recusa a
  subir sem elas:
  * `maxmemory-policy noeviction`. Com qualquer política de despejo o Valkey
    apaga streams inteiros para liberar memória: perda silenciosa.
  * Nunca `MAXLEN` no `XADD`. Aparar só o que todos os grupos já
    confirmaram (`XTRIM MINID` calculado do grupo).
  * AOF ligado (`appendfsync everysec`) e ao menos uma réplica.
* **Garantia mais fraca que Kafka, e dita claramente**: até ~1 s de escrita
  confirmada pode se perder se o primário cair (fsync a cada segundo e
  replicação assíncrona). `WAIT 1` antes do ack reduz a janela ao custo de
  latência; é opção do gateway, desligada por padrão.
* **Tudo fica em RAM.** Dimensionamento: RAM ≈ taxa × tamanho médio × janela
  de buffer × 1,5. A 20 mil eventos/s de 1 KB, 15 min de buffer pedem ~27 GB.
  Por isso este backend é para minutos de amortecimento, não horas.
* **A opção pronta que existe, e por que não é o produto.** O
  [Bento](https://github.com/warpstreamlabs/bento) (MIT, fork do Benthos) tem
  entrada e saída `redis_streams` com grupo de consumo e `XACK`, saída
  `opensearch` e entrada `http_server`; faria relay e drain só com
  configuração. Fica fora do produto por três motivos: é governado por um
  fornecedor (WarpStream, hoje Confluent), o mesmo critério que tirou o Vector;
  não tem `XAUTOCLAIM`/`XCLAIM`, então pendentes de um consumidor morto só
  voltam se o `client_id` for estável; e não checa `noeviction`, não tem
  marca d'água nem credencial por agente, as quatro garantias que justificam
  o `velox-queue`. Serve de **bancada** na fase 3: mede o backend Valkey antes
  de o `velox-queue` existir e depois roda o mesmo teste *golden*.

#### 5.2.3 Qual escolher

| | Kafka | Valkey/Redis Streams |
| --- | --- | --- |
| Volume alvo | centenas de milhares de eventos/s | até dezenas de milhares de eventos/s |
| Janela de buffer | horas, em disco | minutos, em RAM |
| Perda numa falha | nenhuma com `acks=all` + ISR 2 | até ~1 s sem `WAIT` |
| Replay e múltiplos consumidores (SIEM, data lake) | sim | limitado pela memória |
| Gateway (entrada HTTP) | Data Prepper, fonte `http` | `velox-queue relay` |
| Workers | Data Prepper, fonte `kafka` | `velox-queue drain` |
| Peça nossa no caminho | nenhuma | `velox-queue` |
| Operação | maior: discos, partições, retenção | menor, e muitos clientes já operam Redis |

O processamento pesado fica **no agente**, nos parsers de cada integração,
nos dois backends. Os workers só fazem `_bulk`. Isso mantém os dois caminhos
com o mesmo resultado indexado, que a fase 3 verifica com o mesmo teste
*golden* rodando nos dois.

### 5.3 Ordem de grandeza (hipóteses a medir, não promessas)

| Peça | Hipótese | Base |
| --- | --- | --- |
| Fluent Bit no edge | dezenas de milhares de registros/s por núcleo | relatos públicos; medir com nosso parser |
| Gateway | limitado por TLS e serialização; 3 pods para 500 mil ev/s | medir |
| Kafka (Strimzi), 3 brokers | centenas de MB/s | medir |
| Valkey, primário + réplica | dezenas de milhares de ev/s por partição de stream | medir; limitado por RAM e por núcleo por shard |
| OpenSearch | 10 a 30 mil docs/s por nó de dados com mapeamento típico | experiência operacional; é o gargalo |
| Mesh | 100 mil agentes em 3 réplicas pequenas | Fleet Server: 100 mil em 16 vCPU; nosso check-in é mais leve |

A meta de carga da fase 3 é **500 mil eventos/s sustentados por 1 h sem
perda detectável fora do descarte declarado**, em hardware de referência que
o plano fixa. É um teste, não um número de marketing.

### 5.4 Kubernetes: DaemonSet próprio e adaptadores

* **DaemonSet próprio, um por nó**, com dois containers: Fluent Bit e Velox
  Agent como sidecar. Compartilham o volume de config; o agente aplica pela
  `localhost:2020` com o mesmo fluxo do host (staging, `--dry-run`, reload,
  rollback). A política compila **todas** as integrações ativas numa config
  só. Hoje o core sobe uma workload por deployment × receita
  (`src/agents.rs`), e cada nó roda N pods lendo o mesmo `/var/log`.
* **Buffer `filesystem` por padrão** num hostPath, com limite explícito. Hoje
  o core usa só `Mem_Buf_Limit 5MB` em memória e perde o buffer a cada
  restart do pod.
* **Ativação por label ou annotation**: o `src/discovery.rs` já reconhece
  workloads pela imagem; o filtro `kubernetes` do Fluent Bit anexa labels e
  annotations a cada registro. Um pod com `veloxsearch.ai/integration: nginx`
  liga a integração sem editar a política.
* **Não dependemos de operator.** Fluent Operator e Logging Operator foram
  avaliados (PLAN.md §3.5). Os dois reiniciam o processo em vez de validar e
  reverter, e teríamos dois reconciliadores sobre a mesma config.
* **Adaptadores para quem já usa um deles**: o discovery detecta os CRDs
  (`fluentbit.fluent.io`, `logging.banzaicloud.io`) e o Velox cria só um
  `ClusterOutput`/`Output` de tipo `forward` apontando para a entrada de
  compatibilidade (§5.2), que repassa ao gateway. O cliente não troca nada.
  Agente gerenciado pelo provedor (GKE, EKS) não se edita: o DaemonSet Velox
  roda ao lado.
* **Rotação de arquivo**: aumentar `containerLogMaxSize` do kubelet e ajustar
  `Rotate_Wait`; o benchmark VictoriaMetrics mediu registros quebrados no
  Fluent Bit nesse ponto (PLAN.md §3.2 item 8).

### 5.5 Métricas e traces

* **Métricas no Fluent Bit**, no host e no DaemonSet: `node_exporter_metrics`
  e `windows_exporter_metrics` (sem exporter separado), `prometheus_scrape`
  de exporters locais com alvo fixo, e `in_opentelemetry` como relé das
  aplicações. Saída `prometheus_remote_write` para o Cortex que o OTel stack
  do core (`src/otel_stack.rs`, ADR-053) já instala.
* **Traces**: as aplicações mandam OTLP ao Fluent Bit local, que repassa por
  `out_opentelemetry` ao **otelcol gateway do cluster**, e daí ao Data
  Prepper, como o core já faz. *Tail sampling* precisa de todos os spans de
  um trace no mesmo lugar, então pertence ao gateway de qualquer forma.
* **Fica no otelcol gateway**, não no Fluent Bit: descoberta de serviços para
  scrape de pods (`prometheus_scrape` só aceita alvo fixo), kube-state-metrics,
  conversão delta → cumulativo (remote write exige cumulativo; um SDK em delta
  passa pelo gateway) e agregação ou corte de cardinalidade.
* **otelcol no host só por exceção**: receiver que o Fluent Bit não tem,
  descoberta local ou delta sem passar pelo gateway. A política liga o
  engine; o agente o supervisiona como qualquer outro.

## 6. Licenciamento e assentos

Regras (implementadas e testadas em `velox-license`):

| Regra | Comportamento |
| --- | --- |
| Sem documento | **10 assentos**, para sempre, sem ativação, sem rede |
| Assento | agente matriculado e visto na janela (`VELOX_MESH_SEAT_WINDOW`, padrão 24 h); desmatricular libera na hora |
| Acima do limite | agente **estacionado**: matrícula aceita, visível na UI como "aguardando assento", **sem política**, heartbeat lento. Nunca recusado em silêncio |
| Licença de assinatura | JSON assinado (ed25519, `aws-lc-rs`, keyring compilado, rotação como o do registry): `licensee`, `seats`, `expires_at`, `grace_days`, `features` |
| Expiração | carência de 30 dias com tudo funcionando sob banner; depois **somente leitura**: sem matrículas e sem mudança de política, agentes mantêm a config. **Dados nunca param por licença** |
| Recusa de documento | três rejeições distintas: malformado, chave desconhecida, assinatura inválida |
| Features | `gateway`, `audit`, `sso` como flags no documento; o tier gratuito não tem nenhuma |

**Ativação.** Dois caminhos, ambos terminando no mesmo arquivo:

* *offline*: o cliente recebe o arquivo (compra, e-mail, portal) e cola em
  Settings › Licença ou `velox-mesh license load <arquivo>`. Funciona
  air-gapped.
* *online*: `velox-mesh license activate <código>` troca um código de ativação
  por esse arquivo em `https://license.veloxsearch.ai/v1/activate` (serviço da
  Tornis, fora deste repositório). O Mesh guarda o arquivo e nunca mais precisa
  da rede. Nenhuma telemetria sai do cluster sem opt-in (LGPD).

**O que a licença não é.** Não é DRM, e sob AGPL nem pode ser: o cliente tem
o direito de recompilar sem a checagem. A assinatura detecta documento
*forjado* na build oficial, não checagem *removida*. O arquivo de licença é o
comprovante da assinatura de suporte, e o contrato comercial é o que obriga.

## 7. Velox Agent

* Binário estático (musl) para Linux x86_64 e arm64 na fase 1; Windows na
  fase 2 (sem `SIGHUP`: só reload HTTP e restart). Instalação em uma linha com
  token (`curl … | sh -s -- --mesh URL --token T`), pacotes `.deb`/`.rpm`,
  imagem para DaemonSet e sidecar.
* **Supervisor**, não coletor: spawn, saúde, restart com backoff, aplicação
  segura de config (`supervisor.rs`: staging → `--dry-run` → promoção atômica
  → hot reload → confirmação → restart → rollback para o *last-known-good*).
  Falha de config é reportada com a mensagem do engine, verbatim.
* **Engines pinados** por versão de agente, distribuídos junto (o agente
  "sabe" qual Fluent Bit roda, e qual otelcol quando a exceção do §5.5 o
  liga). No caso normal o host roda dois processos: agente e Fluent Bit. Upgrade do agente = upgrade dos
  engines, por OpAMP *packages*, artefato assinado, rollout por percentual
  com janela de manutenção (Fleet faz; vale copiar), rollback ao anterior.
* **Estado no host**: `instance_uid`, credencial (`0600`), política aplicada,
  anterior, buffer `filesystem` do Fluent Bit (e fila `file_storage` do
  otelcol, quando ligado).
* **No Kubernetes**: container sidecar do DaemonSet (§5.4), mesmo binário.
* **Zero código Velox dentro dos engines.** Nenhum plugin C, nenhum fork.
  É o que mantém o Fluent Bit substituível (PLAN.md §3).
* Diagnóstico: `velox-agent diagnostics` gera um pacote (configs sem segredos,
  últimos logs dos engines, métricas) e a ação `diagnostics` do Mesh pede o
  mesmo remotamente.

## 8. Segurança

* Agente só faz conexões de saída; Mesh nunca conecta no host.
* Três credenciais, três escopos: token de matrícula (cria agentes),
  credencial de controle (fala com o Mesh), credencial de dados (gateway ou
  OpenSearch, mínima, por agente ou por política). Revogar uma não revoga as
  outras.
* Segredos fora das políticas e fora dos ConfigMaps; chegam por canal próprio
  e vivem em arquivo `0600`.
* Auditoria (`audit`): quem mudou que política, quando, diff entre
  revisões; quem matriculou, quem revogou.
* Multitenancy: tudo por `tenant_id`, acesso por `Scope`. Dentro do cluster,
  as NetworkPolicies por tenant (ADR-044) ganham uma regra: `AGENT_NS` → Mesh
  na porta de controle; agentes externos entram pelo Ingress do Mesh.
* Assinaturas: pacotes de integração (keyring do registry), licença (keyring
  de licença), artefatos de upgrade (cosign, como a imagem do core). Três
  keyrings, uma regra: nunca remover a chave antiga na release que adiciona a
  nova.

## 9. O que fica de fora, de propósito

Traces processados no host (*tail sampling*, *span metrics*): ficam no
otelcol gateway do cluster. Pipelines de transformação nos workers: o
processamento é no agente, pelos parsers da integração, para os dois
backends de fila darem o mesmo resultado. Fluentd como agregador (PLAN.md
§3.5). Dependência de Fluent Operator ou Logging Operator (§5.4). Armazenamento de longo prazo na fila. Agente como
proxy de rede ou executor remoto genérico: o Mesh entrega configuração, não
comandos arbitrários, e a lista de ações é fechada.
