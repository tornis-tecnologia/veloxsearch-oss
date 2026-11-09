# Velox Mesh e Velox Agent — levantamento, crítica e plano

**Status:** proposta, 2026-11-09. Companheiro de [ARCHITECTURE.md](ARCHITECTURE.md).

1. [Levantamento das referências](#1-levantamento-das-referências)
2. [O que levar de cada uma](#2-o-que-levar-de-cada-uma)
3. [Fluent Bit é o coletor certo?](#3-fluent-bit-é-o-coletor-certo)
4. [Plano de desenvolvimento](#4-plano-de-desenvolvimento)
5. [Crítica à solução proposta](#5-crítica-à-solução-proposta)
6. [Caminhos e decisões pendentes](#6-caminhos-e-decisões-pendentes)

## 1. Levantamento das referências

Fontes lidas em 2026-11-09: documentação oficial de cada projeto. Onde a
documentação não afirma algo, está dito "não documentado" em vez de inferido.

### 1.1 Elastic Fleet / Elastic Agent

* **Matrícula.** O *enrollment token* é uma API key do Elasticsearch ligada a
  **uma** política; reutilizável, revogável, com expiração opcional (expirar
  bloqueia matrículas novas, não agentes já matriculados). Na matrícula o
  Fleet Server emite duas chaves por agente: uma só para falar com o Fleet,
  outra de *output* com permissão mínima de ingestão.
* **Check-in.** Sempre iniciado pelo agente, HTTP *long polling*
  (`checkin_long_poll` 5 min; atividade registrada a cada 30 s). Políticas,
  ações e status vivem em índices `fleet-*` do Elasticsearch; os Fleet Servers
  observam esses índices e empurram para os agentes. Entrega de ações e acks
  têm limite de taxa configurável.
* **Status.** `Healthy`, `Unhealthy`, `Updating`, `Enrolling`, subestados de
  upgrade; **offline após 5 min sem check-in, fixo e não configurável**.
  Desmatrícula por inatividade por política (varredura a cada 10 min, 1 000
  agentes por rodada).
* **Política.** "Uma coleção de *inputs* e configurações"; cada agente tem
  exatamente uma; uma *integration policy* pode ser compartilhada entre várias.
  Vai ao agente como YAML; segredos saem do YAML como `${SECRET_0}` (desde
  8.10). Variáveis e condições avaliadas no agente.
* **Escala.** Fleet Server é *stateless*, balanceável por *round-robin*;
  estado no ES. Tabela da Elastic Cloud: 2 000 agentes = 2 GB; 10 000 = 8 GB;
  100 000 = 16 GB / 16 vCPU. Máximo de 1 000 políticas por instância. Acima de
  10 mil agentes, aumentar o cache de API keys do ES (cada check-in autentica
  uma chave).
* **Upgrade.** Disparado do Fleet; por percentual e janela de manutenção;
  retries em 1, 5, 10, 15, 30 min, 1 h; *rollback* manual (9.3+) mantendo o
  anterior em disco por 7 dias; **DEB/RPM/Docker/Kubernetes não são
  atualizáveis pelo Fleet**; agente não pode ser mais novo que o Fleet Server.
* **Supervisão.** O agente é um supervisor de subprocessos (`agentbeat`, depois
  `elastic-otel-collector`, `endpoint-security`); o próprio Fleet Server é um
  subprocesso de um agente no sabor "servers".

### 1.2 Grafana Alloy

* **Modelo.** Grafo de componentes (mais de 120); cada um tem argumentos e
  *exports*, outros referenciam `local.file.api_key.content`; mudar um export
  reavalia dependentes **em processo, sem restart**. Pipelines ligados por
  `forward_to`/`receiver`.
* **Config remota.** Bloco `remotecfg`: `poll_frequency` padrão 1 min (mínimo
  10 s); `id` aleatório persistido em `alloy_seed.json`; `attributes`
  autorreportados com prefixo reservado `collector.`; **o servidor decide a
  config pelo `id` e pelos atributos**. API em protobuf/connect-go
  (`grafana/alloy-remote-config`). Como o estado aplicado volta ao servidor:
  **não documentado** nas páginas lidas.
* **Clustering.** Gossip + hashing consistente (512 tokens) para repartir
  alvos de *scrape*; só componentes com `clustering { enabled = true }`;
  **a doc desaconselha clusterizar coletores de log em DaemonSet**.
* **Entrega.** `prometheus.remote_write` tem WAL por endpoint (perda se a
  indisponibilidade passar de `max_keepalive_time`, 8 h). `otelcol.exporter.otlp`
  tem fila **em memória** por padrão (1 000; "cerca de 10 s de tolerância a
  100 req/s"), persistência só com `otelcol.storage`; `retry_on_failure` desiste
  em 5 min. Nenhuma garantia de ponta a ponta documentada.
* **UI.** `:12345` com saúde por componente, argumentos avaliados, exports,
  *live debugging* opt-in para alguns componentes.

### 1.3 Vector

* **Modelo.** DAG de *sources* → *transforms* → *sinks*; eventos são logs,
  métricas ou traces (traces em alfa). VRL para transformação (não lido).
* **Acks de ponta a ponta.** O *source* cria um *batch notifier*; o *sink*
  reporta o resultado; o *source* confirma para cima (status HTTP, acks
  nativos em `kafka` e `aws_sqs`). Em *fan-out*, **o pior status vence**:
  falha em 1 de 3 sinks torna o evento reenviável para os 3. Vector avisa na
  partida se um sink com ack tem source sem ack: "*silent data loss may
  occur*". Comportamento resultante: *at-least-once* com duplicatas possíveis.
* **Buffers.** Memória (por contagem de eventos, não bytes) ou disco (WAL com
  checksum, `fsync` a cada 500 ms, mínimo 256 MiB, arquivos de 128 MiB,
  **sai do processo em erro de I/O**). `when_full`: `block` (padrão,
  backpressure), `drop_newest` (sem backpressure), `overflow` (não para
  produção). Backpressure propaga sink → transform → source → cliente (HTTP
  429; *pull sources* desaceleram).
* **ARC.** Concorrência adaptativa de requisições "inspirada em controle de
  congestionamento TCP", substitui limites estáticos.
* **Topologias.** *Distributed*, *Centralized* (agregadores amortecem; "agent
  nodes are designed to get data off of the machine as quickly as possible",
  perda possível) e *Stream-based* (Kafka/Pulsar: "*most durable and
  reliable*", replay, Vector vira camada de roteamento; custa mais operar).
* **Ferramentas.** `vector tap` (amostra eventos entrando/saindo de um
  componente via gRPC) e `vector top`.
* Nenhum benchmark de throughput nas páginas lidas.

### 1.4 OpAMP (Open Agent Management Protocol)

* Especificação **Beta**; itens individuais variam (pacotes, telemetria
  própria, *connection settings*, saúde e restart em Beta; heartbeats,
  capacidades customizadas, CSR iniciado pelo agente em *Development*).
* Transportes: HTTP simples (POST, *polling* padrão 30 s, caminho `/v1/opamp`,
  porta 4320) e WebSocket.
* Cobre: config remota com status de aplicação, saúde por componente,
  telemetria própria via OTLP, heartbeat, pacotes (upgrade e downgrade),
  *connection settings* com rotação e revogação de certificado, restart,
  mensagens customizadas.
* Implementações: `opamp-go` (cliente e servidor), clientes .NET, Java,
  Python; **nenhuma em Rust**. Plataformas: Bindplane, Grafana Fleet
  Management. Agentes: extensão OpAMP e *OpAMP Supervisor* no
  collector-contrib (prontidão para produção do supervisor: não declarada).

### 1.5 Fluent Bit (fatos que o desenho usa)

* Última estável listada: **5.2.0**; CNCF *graduated*. (Que os mantenedores
  centrais são empregados da Chronosphere, via aquisição da Calyptia em 2024,
  é conhecimento público não confirmado no site.)
* **Hot reload**: `hot_reload: on` + `POST /api/v2/reload` (precisa de
  `http_server: on`) ou `SIGHUP` (não no Windows); 5.1.2+ preserva o diretório
  de config entre reloads. Flag de validação (`--dry-run`): existe na CLI, mas
  **não apareceu nas páginas lidas**; confirmar na versão que pinarmos.
* YAML com *processors* a partir da 3.2.
* Buffer: `storage.type` `memory|filesystem|memrb`; `storage.max_chunks_up`;
  `storage.total_limit_size` por saída, **descarta o chunk mais antigo** ao
  encher.
* Monitoração: `/api/v2/health` (200/500, exige `health_check: on`),
  `/api/v2/metrics/prometheus`.
* Saída `forward` com `require_ack_response` → *at-least-once* por chunk;
  **a saída `opensearch` não tem ack**.

## 2. O que levar de cada uma

| De | Levar | Deixar |
| --- | --- | --- |
| **Fleet** | token por política, reutilizável e revogável; duas credenciais por agente (controle ≠ dados); check-in iniciado pelo agente; offline em 5 min fixo; revisões de política ecoadas; rollout de upgrade por percentual com janela e retries; ações com limite de taxa; pacote de diagnóstico | estado no mesmo banco que os dados (nosso Mesh usa Postgres, não o OpenSearch do cliente); "Fleet Server é um agente" (bootstrap confuso); pacotes que não atualizam por DEB/RPM (nosso agente atualiza o engine junto, em qualquer formato) |
| **Alloy** | atribuição de config **por atributos** no servidor; `id` persistido em arquivo *seed*; prefixo reservado de atributos; UI local de depuração com config efetiva e saúde por componente | *clustering* no agente de logs (a própria doc desaconselha); reavaliação em processo (impossível com engine externo; o nosso faz reload + restart) |
| **Vector** | semântica explícita de buffer (`when_full`, tamanho, o que acontece ao encher) **declarada na política**; topologia *stream-based* para alta volumetria; a ideia de `tap` para depurar um agente; o aviso honesto de perda silenciosa na partida | Vector como agente de borda (sem o corpus de integrações; VRL não é necessário no edge); como peça do plano de dados, fora por governança (§3) |
| **OpAMP** | o protocolo inteiro, em vez de inventar um; a semântica de hash ecoado; *connection settings* para rotação de credencial | WebSocket na fase 1 |

### 2.1 Comparativo resumido

| | Fleet | Alloy + Fleet Mgmt | Vector | **Velox Mesh (proposta)** |
| --- | --- | --- | --- | --- |
| Protocolo de gestão | próprio (HTTP long-poll) | próprio (connect-go) | nenhum (config em arquivo) | **OpAMP** (HTTP; WS depois) |
| Engine de logs | Beats → OTel | próprio (Go) | próprio (Rust) | **Fluent Bit** (subprocesso) |
| Métricas/traces | OTel | nativo | sim | **OTel Collector** (subprocesso) |
| Atribuição | por token | por atributos | — | **token ou rótulos** |
| Integrações | pacotes Elastic | componentes | — | **pacotes assinados, mesmo renderer do core** |
| Ack ponta a ponta | não documentado | não | sim | por salto (edge→gateway com ack; fila com `acks=all`) |
| Estado | Elasticsearch | SaaS Grafana | — | **Postgres do cliente** |
| Licença | Elastic License / SSPL | Apache-2.0 + SaaS | MPL-2.0 | **AGPL-3.0-only inteiro**; 10 agentes na build oficial sem assinatura |

## 3. Fluent Bit é o coletor certo?

Resposta curta: **sim para logs na borda, nas fases 1 e 2, com uma condição
de desenho**; **também para métricas do host**, com traces repassados ao
OTel do cluster; **não na camada de agregação**. Trocar o engine do host
pelo OTel Collector continua possível, não planejado (§3.4).

Restrição de produto (2026-11-09): só componentes de governança neutra no
plano de dados. **Vector fica fora** (Datadog, MPL-2.0, fora de fundação); ele
continua no levantamento (§1.3) como fonte de ideias, não como peça.

### 3.1 A favor

* **Investimento existente.** Doze receitas e o formato de pacote do registry
  são Fluent Bit (`agent.conf.tmpl`). O renderer, os testes *golden* e a
  experiência de produção já existem. Trocar o engine agora zera isso.
* **Pegada e maturidade na borda.** C, poucos MB de RSS, `tail` com
  *multiline*, parsers CRI/Docker, filtro Kubernetes, `systemd`, Windows
  Event Log, `kubernetes_events`, `node_exporter_metrics`, OTLP in/out. É o
  coletor que já está em quase todo cluster.
* **As peças de gestão que precisamos existem**: HTTP server com saúde e
  Prometheus, *hot reload* por HTTP, buffer em disco com limites explícitos,
  YAML, saída `forward` com ack. O que falta (protocolo remoto) é exatamente o
  que o Velox Agent adiciona por fora.
* CNCF *graduated*, Apache-2.0, sem risco de licença.

### 3.2 Contra

1. **Não tem gestão remota nativa.** A única que teve foi o plugin
   proprietário da Calyptia (hoje Chronosphere). Supervisionar por fora é o
   modelo do Elastic Agent com os Beats, funciona, mas significa: dois
   processos por engine, reload fora do processo, e nenhuma "reavaliação em
   lugar" à la Alloy.
2. **Garantia de entrega fraca na saída que mais usamos.** `out_opensearch`
   não confirma de ponta a ponta; o buffer `filesystem` dá *at-least-once por
   salto* com retries por chunk e duplicatas possíveis; ao encher, descarta o
   mais antigo em silêncio (só a métrica conta). Para "sem perda" de verdade,
   o Fluent Bit sozinho não entrega; a fila do nível 1 é o que entrega.
3. **Processamento limitado.** Lua por registro e *processors*; nada perto de
   VRL ou OTTL. Enriquecimento e normalização pesados vão para gateway/workers,
   o que empurra a complexidade para a camada de agregação.
4. **Métricas e traces são secundários.** Existem, mas o ecossistema, os
   *receivers* e a semântica são do OTel Collector. Por isso o agente roda
   os dois engines desde o primeiro dia.
5. **Hot reload tem arestas.** Plugins que não recarregam limpo, `SIGHUP`
   ausente no Windows. O agente nunca pode depender só dele (daí o fallback
   para restart e o rollback no `supervisor.rs`).
6. **Isolamento dentro do processo.** Um `tail` com glob errado ou um parser
   catastrófico afeta o pipeline inteiro; `threaded: on` ajuda, não resolve.
   Mitigação: a política pode pedir **duas instâncias** de Fluent Bit (uma por
   grupo de ativações críticas); o supervisor suporta N filhos.
7. **Governança concentrada.** Mantenedores centrais numa única empresa: a
   Calyptia foi para a Chronosphere, e a Chronosphere foi comprada pela Palo
   Alto Networks (anunciado em nov/2025, concluído em 2026). CNCF *graduated*
   e Apache-2.0 reduzem o risco de abandono e garantem o fork, não o de
   prioridades.
8. **Rotação de arquivo.** O benchmark de 2026 da VictoriaMetrics (fornecedor
   concorrente, configurações padrão dos charts) mediu 34 registros quebrados
   em 1 h a 10 mil logs/s durante a rotação, e OOM no pico com limite de
   1 GiB. Precisa ser reproduzido na nossa bancada com buffer `filesystem`.

### 3.3 Veredito e a condição de desenho

Fluent Bit **fica como engine de logs na borda**. A condição: **nenhuma lógica
Velox entra nele** (sem plugin C, sem fork, sem patch); ele é um binário
upstream pinado que o agente gerencia por arquivo + HTTP. Isso mantém o custo
de troca baixo: o `FragmentRenderer` ganha um alvo novo e o resto não muda.

Decisões derivadas:

* **Métricas no Fluent Bit**, no host e no DaemonSet, com saída Prometheus
  remote write para o Cortex do core. **Traces** passam pelo Fluent Bit como
  relé OTLP até o otelcol gateway do cluster. O otelcol no host fica só por
  exceção (ARCHITECTURE.md §5.5).
* **Gateway e workers do backend Kafka: OpenSearch Data Prepper**, nos dois
  papéis (decidido em 2026-11-09, ARCHITECTURE.md §5.2.1). É o ingestor do
  próprio projeto OpenSearch (Apache-2.0, OpenSearch Software Foundation /
  Linux Foundation), o core já o usa para traces, tem *sink* OpenSearch GA com
  DLQ, e a fonte `http` com `KafkaBuffer` responde 200 só depois do ack do
  broker (verificado no código 2.17: `produceRawData` chama `send(...).get()`).
  O OTel Collector em modo gateway fica fora porque o `opensearchexporter` é
  *alpha* para logs; volta ao marco de 12 meses se chegar a *beta*.
* **Gateway do backend Valkey: `velox-queue relay`**, código nosso. Fluent Bit
  não lê nem escreve Redis Streams (sem plugin oficial; os da comunidade só
  escrevem em listas), e um plugin nosso dentro dele violaria a condição
  acima. O Bento (MIT, WarpStream/Confluent) faria o papel só com
  configuração, mas é governado por fornecedor e não dá quatro garantias que
  exigimos (`noeviction`, `XAUTOCLAIM`, marca d'água, credencial por agente).
  Fica como bancada da fase 3 (ARCHITECTURE.md §5.2.2).
* **O gateway Fluent Bit vira entrada de compatibilidade opcional**
  (`in_forward`, `in_syslog` → `out_http`), fora do caminho dos agentes Velox.

### 3.4 O OTel Collector pode substituir o Fluent Bit?

**Pode, mas ainda não como troca direta.** Estado verificado em 2026-11-09
(`metadata.yaml` de cada componente no `opentelemetry-collector-contrib`):

| Peça | Fluent Bit | OTel Collector |
| --- | --- | --- |
| Arquivo (`tail` / `filelog`) | maduro | *beta* |
| journald | maduro (`systemd`) | *alpha*, só contrib/k8s |
| Windows Event Log | maduro | *beta*, só contrib |
| Saída OpenSearch | `out_opensearch` maduro | `opensearchexporter` *alpha* para logs, só contrib, modo ECS instável, procura mantenedores |
| Transformação | Lua, *processors* | OTTL, bem mais expressivo |
| Métricas e traces | secundários | nativos |
| Gestão remota | nenhuma | OpAMP: extensão + supervisor *alpha*; sem *packages* nem update do binário; rollback desligado por padrão |
| Governança | CNCF *graduated*, uma empresa (Palo Alto) | CNCF, dezenas de empresas |
| Integrações Velox | todas (`agent.conf.tmpl`) | nenhuma |

Benchmark VictoriaMetrics 2026, *charts* padrão, 1 núcleo e 1 GiB, a ~10 mil
logs/s: Fluent Bit 0,26 núcleo e 78 MiB; OTel 0,49 núcleo e 107 MiB. Pico com
100 pods: Fluent Bit 31,3 mil logs/s, OTel 20,5 mil. OTel sem registro
quebrado na rotação; Fluent Bit com 34 e OOM no pico. Número de fornecedor
concorrente: direção, não verdade; a bancada da fase 0 reproduz.

O que a troca resolveria: um engine em vez de dois (dois processos por host,
não três; crítica 1 do §5), uma linguagem de config, governança neutra,
OTTL, ecossistema OpAMP. O que ela custa hoje: reescrever cada integração em
operadores `filelog` + OTTL com novos testes *golden*, aceitar uma saída
OpenSearch *alpha* (ou sempre passar por Data Prepper), ~2× CPU por evento na
borda, e um binário contrib enorme (resolve-se com distribuição própria pelo
OpenTelemetry Collector Builder, só com os componentes que usamos). O
supervisor OpAMP oficial não muda nada para nós: o Velox Agent já é o
supervisor e cobre o que ele não faz (pacotes, update, rollback).

**Decisão (revista em 2026-11-09): Fluent Bit é o engine do host e do nó;
OTel fica no cluster; a troca do engine do host continua possível, não
planejada.**

1. Host e DaemonSet: Fluent Bit para logs, métricas do host e relé OTLP. Dois
   processos por host (agente e Fluent Bit).
2. Cluster: o otelcol gateway que o core já instala cuida de traces,
   descoberta de serviços, kube-state-metrics e conversão delta → cumulativo.
3. O host só fala protocolos abertos (HTTP/JSON, OTLP, remote write). Trocar o
   engine do host depois mexe nos templates das integrações, não no backend.
   Por isso o manifesto continua aceitando um asset `otel.yaml.tmpl`, e o
   `FragmentRenderer` gera os dois alvos quando o pacote os traz.
4. Reavaliar se a dependência da Palo Alto Networks virar problema, ou nos
   critérios do marco de 12 meses (§4, marcos).

### 3.5 Fluentd, Fluent Operator e Logging Operator

Avaliados em 2026-11-09 a partir dos repositórios e da documentação oficial.

| | Fluent Operator | Logging Operator |
| --- | --- | --- |
| Desde | 2019 | 2018 |
| Governança | subprojeto da comunidade Fluent (origem KubeSphere) | CNCF Sandbox (origem Banzai Cloud, Cisco) |
| Última versão | v3.11.0, 2026-10-28 | 6.9.0, 2026-09-17 |
| Agregador | opcional: tem modo só Fluent Bit | Fluentd ou syslog-ng; só o agregador processa |
| Aplicação de config | Secret + watcher que reinicia o processo | config-check + reloader |

* **Logging Operator** é mais maduro como produto: mais adoção, modelo
  multi-tenant melhor. Mas "*Only the aggregators can manipulate the logs*",
  e os mantenedores já testam o Telemetry Controller (OTel) no lugar do
  Fluent Bit.
* **Fluent Operator** combina mais com o nosso desenho: modo só Fluent Bit, e
  os CRDs `ClusterInput`/`ClusterFilter`/`ClusterOutput` espelham os nossos
  fragmentos. Exige imagem própria do Fluent Bit.
* **Não adotamos nenhum como dependência**: dois reconciliadores sobre a mesma
  config, nenhum valida antes nem reverte, e o Kubernetes teria um caminho
  diferente do host. Usamos os dois **como origem**: o Velox cria um
  `ClusterOutput`/`Output` apontando para o gateway (ARCHITECTURE.md §5.4).
* **Fluentd fica fora.** No benchmark VictoriaMetrics chegou a 5,1 mil logs/s
  e perdeu logs antes de 10 mil; a doc do Logging Operator manda usar
  syslog-ng em volume alto; o papel que ele cumpre nos operators
  (processar, rotear, segurar) já é do agente, dos workers e da fila; o
  buffer em PVC de um pod só não substitui uma fila replicada. Frotas Fluentd
  existentes entram pela entrada de compatibilidade (protocolo forward), sem
  migração.

## 4. Plano de desenvolvimento

Premissas: 2 engenheiros Rust, 1 frontend, ½ infra/QA. Semanas são de
esforço calendário com esse time; cada fase termina com algo instalável e
testado em k3s/minikube. As estimativas são ordens de grandeza para
priorizar, não compromissos.

### Fase 0 — Fundações (3 semanas)

Decisões que, erradas, custam a refazer tudo:

1. **Crate comum.** Extrair `velox-integrations` (renderer +
   `CLOSED_TOKENS` + parsing de manifesto) para `crates/velox-integrations/`,
   AGPL como o resto, consumida pelo core e por `veloxagent/`. Refatoração
   pura: mesma licença, sem auditoria de autoria. ADR-068.
   **Feito (2026-11-09)**: o core re-exporta o crate em `integrations.rs` e
   ficou só com as escritas no cluster; `velox-policy::PackageRenderer` é o
   `FragmentRenderer` de produção sobre o mesmo `render`. O script
   `check-agent-boundary.sh` prova no grafo resolvido que `velox-agent` não
   liga `veloxsearch` nem `kube`.
2. **Esquema de manifesto**: `variables` e asset opcional `otel.yaml.tmpl` em
   `docs/integrations/manifest.schema.json` + validador. Mudança de especificação.
   **Feito (2026-11-09)** como `schema_version: "1.1"`: `variables` (nome,
   tipo, default, obrigatório) e `assets.otel_config`, opcionais; pacote que
   usa qualquer um deve declarar 1.1. Variáveis **não** são tokens: entram
   como `${name}` e só os nomes declarados são substituídos
   (`velox_integrations::resolve_variables` / `apply_variables`,
   interpolation.md). O validador JSON-Schema continua no registry.
3. **OpAMP**: fechar o subconjunto de mensagens (`velox-opamp`), gerar tipos a
   partir do `.proto` oficial com `prost` ao lado do JSON, testes de
   compatibilidade contra `opamp-go` (servidor de exemplo) para não divergir
   do padrão. **Parcial (2026-11-09)**: o `.proto` oficial está vendorizado
   em `velox-opamp/proto/` (Apache-2.0, commit registrado no NOTICE) e um
   teste lê o arquivo e prova que cada campo e cada variante do JSON existe
   na mensagem de mesmo nome. `prost` e o teste contra `opamp-go` ficam para
   o marco "fim da fase 1", que decide se o JSON basta.
4. **Chaves**: gerar o par de licença (`veloxagent/keys/velox-license-2026-11.pub`
   compilado; privada fora do repo, mesmo procedimento do registry).
   **Feito (2026-11-09)**: `.pub` em `veloxagent/keys/` com README de
   custódia e rotação; `velox_license::keyring()` compila o arquivo. A
   privada está fora do repositório e precisa ir para o cofre dos
   mantenedores (segredo do GitHub Actions, ADR-054) antes de emitir a
   primeira licença.
5. **Bancada de carga**: gerador de logs (`flog`-like) + k3s de 3 nós +
   OpenSearch de referência; define a linha de base do nível 0 antes de
   existir nível 1.
6. **CI**: job `veloxagent` (fmt, clippy `-D warnings`, test; o
   `check-headers.sh` já cobre a pasta com o cabeçalho AGPL); `cargo deny`
   com allowlist própria. **Feito (2026-11-09)**: job `veloxagent` em
   `ci.yml` com a checagem de fronteira, e `veloxagent/deny.toml` no job
   `supply-chain`.

### Fase 1 — Mesh + Agent mínimos (8 semanas)

Meta: *um host fora do cluster matricula, recebe uma política com nginx e
kubernetes-logs, grava no deployment, aparece saudável na UI.*

* **Mesh**: migrações (`mesh_agents`, `mesh_policies`,
  `mesh_policy_revisions`, `mesh_enrollment_tokens`, `mesh_license`);
  `POST /v1/opamp` (matrícula por token, check-in, entrega de config por hash,
  ack de status); compilação por `velox-policy` com o renderer comum; cache por
  revisão; `last_seen` em lote; status derivado; `Scope` por tenant;
  `/api/mesh/{agents,policies,tokens,license}`; tier gratuito e **assentos**
  (`parked` funcional, com mensagem no agente e na UI).
* **Agent**: Linux x86_64/arm64 estático; `enroll`/`run`/`version`;
  supervisor do Fluent Bit (plano de aplicação do `supervisor.rs` executado:
  staging, dry-run, promoção, reload, confirmação, restart, rollback);
  credencial de dados em `0600`; `instance_uid` persistido; relatório de saúde
  e métricas do Fluent Bit; unidade systemd; instalador em uma linha.
* **Kubernetes**: DaemonSet Velox um por nó (Fluent Bit + agente sidecar)
  substitui as workloads por receita do core; buffer `filesystem` em
  hostPath por padrão; ativação por annotation `veloxsearch.ai/integration`.
* **UI** (`veloxagent/frontend`, carregado sob `features.mesh`): Agentes (lista,
  filtros por status e rótulo, detalhe com config efetiva e erro do engine),
  Políticas (editor de ativações com variáveis do manifesto, revisões, "N de M
  aplicados"), Tokens, Licença (tier, assentos usados, carregar arquivo).
* **Core**: `GET /api/features`; emissão de token de serviço para o
  Mesh; instalação do Mesh como componente opcional (padrão do OTel stack);
  regra de NetworkPolicy `AGENT_NS` → Mesh.
* **Testes**: unitários em cada crate; e2e Python novo `tests/mesh_check.py`
  (matrícula, política, F5 em `/mesh/agents/:id`, assento 11 estacionado).

### Fase 2 — Operação do dia 2 (6 semanas)

* **Métricas no Fluent Bit**: `node_exporter_metrics`,
  `windows_exporter_metrics`, `prometheus_scrape` local, relé
  `in_opentelemetry`; saída remote write para o Cortex do core. Traces
  repassados ao otelcol gateway. otelcol no host como exceção ligada por
  política.
* **Adaptadores** para clusters com Fluent Operator ou Logging Operator:
  detecção no discovery e `ClusterOutput`/`Output` para o gateway.
* Atribuição de política **por rótulos**; ações (`unenroll`, `restart`,
  `diagnostics`) com fila `mesh_actions` e ack.
* **Upgrade** do agente por OpAMP *packages*: artefato assinado (cosign),
  rollout por percentual e janela, retries, rollback para o anterior.
* **Ativação online** (`license activate <código>`) + serviço de licenças da
  Tornis (fora do repo); carência e somente-leitura na UI; banner.
* Windows (reload só por HTTP; serviço do Windows).
* Auditoria (`audit`): quem/quando/diff de revisão.
* Credencial de dados **por agente** no nível 0 (usuário OpenSearch mínimo
  criado pelo Mesh via security plugin), revogável individualmente.

### Fase 3 — Alta volumetria: Gateway + fila (11 semanas)

* **Contrato de entrada comum**: HTTP sobre TLS, JSON gzip, token por agente
  emitido pelo Mesh; 2xx só depois da escrita confirmada na fila; 503 acima
  da marca d'água. O `FragmentRenderer` gera o `out_http` do agente a partir
  de `Output::Gateway`. A fila é configuração do gateway, não da política.
* **Backend Kafka** (6 semanas): instalação opcional de Apache Kafka via
  **Strimzi** (KRaft) pelo bootstrap do core, ou Kafka do cliente; tópico por
  deployment, `acks=all`, ISR 2, cotas. Dois Deployments de **Data Prepper**:
  gateway (fonte `http` → `KafkaBuffer`, HPA por CPU) e workers (fonte
  `kafka` → *sink* OpenSearch com DLQ, KEDA por *lag*). Pipelines gerados
  pelo core, no padrão de `src/otel_stack.rs`.
* **Backend Valkey/Redis Streams** (5 semanas): instalação opcional de
  **Valkey** (primário + réplica, AOF) ou Redis do cliente; crate e binário
  `velox-queue` (`relay`: HTTP → `XADD` com marca d'água; `drain`:
  `XREADGROUP` → `_bulk` → `XACK`, DLQ, `XAUTOCLAIM`); recusa de partida sem
  `noeviction`; streams particionados por deployment; KEDA por pendentes.
  Bancada: Bento com `http_server` → `redis_streams` → `opensearch` mede o
  backend antes do `velox-queue` existir, depois roda o mesmo *golden*.
* **Entrada de compatibilidade** (opcional, 1 semana dentro das anteriores):
  Fluent Bit com `in_forward` (TLS, ack) e `in_syslog`, `out_http` para o
  gateway; Deployment + HPA. Para frotas Fluentd e os dois operators.
* Saída `Gateway` nas políticas; o agente passa a não ter credencial de
  OpenSearch nesse modo.
* Painel de pipeline no Mesh (entrada, saída, retries, descartes, lag) por
  agente e por deployment; alertas no console quando um agente descarta.
* **Teste *golden* comum**: o mesmo conjunto de eventos atravessa os dois
  backends e o resultado indexado tem de ser idêntico.
* **Testes de carga da fase**, hardware de referência fixado em
  `veloxagent/docs/BENCHMARK.md`, resultado publicado com método:
  * Kafka: 500 mil eventos/s por 1 h;
  * Valkey: 50 mil eventos/s por 1 h, com uma queda do OpenSearch de 10 min
    absorvida sem perda.
  Meta de perda: zero fora do descarte declarado por limite de buffer e,
  no Valkey, fora da janela de ~1 s documentada para falha do primário.

### Fase 4 — Escala e empresa (contínuo)

Mesh em alta disponibilidade com Postgres replicado; WebSocket para entrega
imediata; rollouts canário de **política** (não só de upgrade; nenhuma das
três referências faz isso bem); grupos de agentes; busca na frota; RBAC fino
sobre objetos do Mesh; SSO para o Mesh (`sso`); multi-região (um Mesh por
região, federação de leitura); `velox-agent tap` (amostra do que sai de um
engine, inspirado no `vector tap`).

### Marcos de decisão

| Quando | Decidir | Com base em |
| --- | --- | --- |
| fim da fase 0 | `velox-integrations` extraída e consumida pelos dois workspaces | testes *golden* do registry verdes nos dois lados |
| fim da fase 1 | OpAMP JSON basta ou prost desde já | tamanho de mensagem × 10 mil agentes na bancada |
| meio da fase 3 | `velox-queue` confirmado, ou reabrir Bento | bancada Valkey com os dois, mesmo *golden*, método publicado |
| fim da fase 3 | backend de fila padrão do bootstrap | carga medida dos dois backends e perfil dos primeiros clientes |
| 12 meses | trocar o engine do host para OTel? | as doze integrações portadas com *golden* verde; `filelog` ≤ 1,5× a CPU do Fluent Bit na nossa bancada; caminho OpenSearch estável (exporter *beta* ou Data Prepper); journald *beta* |

## 5. Crítica à solução proposta

Dito antes que alguém de fora diga:

1. **Dois processos por host.** Agente + Fluent Bit (otelcol só por
   exceção). Mais RSS que o Alloy (um processo) e mais superfície de falha.
   Mitigado pelo supervisor com rollback. Custo pequeno, mas real em edge
   pequeno.
2. **A linguagem do engine vaza pela "política".** No host é só Fluent Bit
   YAML; o OTel YAML fica no gateway do cluster, gerido pelo core. Mesmo
   assim, a mensagem de erro que o operador vê é do engine. Mostrá-la
   *verbatim* é deliberado (melhor que uma tradução errada), mas exige que a
   UI ensine a lê-la.
3. **OpAMP em Beta, e sem implementação Rust.** Escrevemos a nossa. Risco de
   divergência do padrão; mitigado por testes contra `opamp-go`. Benefício só
   se aparece quando um agente de terceiros se conectar; até lá é um protocolo
   próprio com nome bonito.
4. **A fronteira de compilação é frágil operacionalmente.** Basta um `use`
   errado para o agente arrastar `kube`, o servidor do console e megabytes de
   dependências para dentro de um binário que deveria ser estático e pequeno.
   Precisa de checagem mecânica no CI (`cargo tree` de `velox-agent` não pode
   conter o crate `veloxsearch` nem `kube`), não de atenção na revisão.
5. **AGPL torna o limite de 10 agentes opcional para quem quiser.** Remover a
   checagem é direito garantido pela licença, não pirataria. Aceito por
   desenho (§1 e §6 da arquitetura): o que se vende é suporte, build oficial
   assinada, atualizações, serviço gerenciado e licença comercial para quem
   não aceita AGPL. É o modelo do Wazuh (GPLv2 inteiro, receita de cloud e
   suporte). Consequência: o limite deve ser generoso e servir de lembrete,
   não de muro (crítica 6).
6. **10 agentes gratuitos é pouco frente ao mercado.** Elastic Agent não limita
   agentes no tier Basic; Alloy é Apache-2.0 sem limite; o Fleet Management da
   Grafana tem tier gratuito. Um limite baixo de assentos empurra o avaliador
   para a alternativa antes de ver valor. Alternativa a considerar: assentos
   generosos (50–100) no gratuito e cobrar por **recursos** (gateway, auditoria,
   SSO, rollout canário). A arquitetura não muda em nada entre as duas; só o
   número em `FREE_SEATS` e o conteúdo de `features`.
7. **Postgres obrigatório para o Mesh.** O core trata Postgres como opcional;
   o Mesh o exige. Instalação fica maior e o backup do Postgres vira parte do
   produto. Alternativa (estado em CRs, como o provisioning do core) foi
   rejeitada: dezenas de milhares de agentes com `last_seen` não cabem no
   etcd com dignidade.
8. **Duas filas dobram o que testar.** Kafka e Valkey são escolha do
   cliente, então cada mudança no gateway e nos workers roda nos dois. E o
   backend Valkey tem código nosso no caminho dos dados (`velox-queue`), porque
   nada do ecossistema neutro fala Redis Streams (o Bento fala, mas é de um
   fornecedor; §3.3). Mitigações: a política não muda
   por backend, processamento só no agente, teste *golden* comum. As
   garantias são diferentes e a UI tem de dizer isso: Valkey é RAM, minutos
   de buffer e ~1 s de janela de perda na queda do primário; Kafka é disco,
   horas e nenhuma perda com uma falha. Valkey com política de despejo perde
   dados em silêncio, por isso o relay recusa subir sem `noeviction`.
9. **Duplicatas.** *At-least-once* em todos os saltos significa duplicatas
   possíveis no OpenSearch. Mitigação barata: `_id` determinístico por
   registro (hash de origem + offset) nas integrações que têm offset; é
   mudança no renderer comum e tem custo de indexação. Não está no plano;
   deveria estar na lista de decisões.
10. **Dependência do Fluent Bit numa única empresa** (§3.2 item 7), hoje a
    Palo Alto Networks. O desenho minimiza o custo de troca, e o §3.4 planeja
    a saída.

## 6. Caminhos e decisões pendentes

Em ordem do que destrava mais:

1. **Jurídico**: decidir DCO × CLA daqui em diante. Só importa se a Tornis
   quiser vender licença comercial do código a quem não aceita AGPL; com DCO,
   cada contribuição externa fica só sob AGPL.
2. **Comercial**: `FREE_SEATS` em 10 ou tier gratuito por recursos (crítica 6).
3. **Produto**: confirmar se o primeiro cliente-alvo tem hosts **fora** do
   Kubernetes. Se todos os hosts são nós de cluster, a fase 1 pode começar só
   pelo DaemonSet Velox gerido pelo Mesh, bem menor, e o agente de host vem
   depois. O DaemonSet já corrige dois problemas do core hoje: uma workload
   por receita e buffer só em memória.
4. **Engenharia**: extrair `velox-integrations`; aceitar o ADR-068; executar a
   fase 0.
5. **Dados**: definir desde já a política de `_id` determinístico (crítica 9)
   porque muda o renderer comum e os testes *golden* do registry.
6. **Fila padrão**: qual backend o bootstrap oferece primeiro. Sugestão:
   Valkey para começar (mais simples, cobre volume moderado), Kafka quando o
   cliente declarar volume alto ou precisar de replay.

O esqueleto em `veloxagent/crates/` compila e testa: regras de licença e assentos
(`velox-license`), compilador de política com a fronteira `FragmentRenderer`
(`velox-policy`), mensagens OpAMP (`velox-opamp`), plano de aplicação segura de
config (`velox-agent::supervisor`) e um `velox-mesh` que sobe, declara o tier
e responde saúde. É o ponto de partida da fase 1, não a fase 1.
