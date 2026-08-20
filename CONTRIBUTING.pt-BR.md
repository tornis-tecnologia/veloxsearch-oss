# Contribuindo com o VeloxSearch

*[Read in English](CONTRIBUTING.md)*

Obrigado por considerar contribuir. Este documento cobre o que você precisa para
compilar o projeto, as convenções que o código segue e como uma mudança é
mesclada.

Ao participar você concorda com o [Código de Conduta](CODE_OF_CONDUCT.md).

## Formas de contribuir que não exigem Rust

- **Integrações de log.** Uma integração é um pacote de dados assinado —
  manifesto, pipeline de ingestão, index template, dashboards, configuração do
  Fluent Bit. Não contém código. Elas vivem em
  [`veloxsearch-registry`](https://github.com/tornis-tecnologia/veloxsearch-registry);
  o formato está especificado em [docs/integrations/](docs/integrations/).
- **Traduções.** Toda string da interface está em `frontend/i18n.jsx` (hoje pt +
  en). Adicionar um idioma é adicionar um conjunto de chaves lá, sem tocar nas
  telas.
- **Documentação.** Qualquer coisa em `docs/`. O arquivo em inglês é o canônico;
  o espelho `.pt-BR` deve acompanhar no mesmo PR quando você mudar um dos dois.
- **Reproduções.** Um relato preciso de bug contra uma distribuição e versão
  nomeadas vale muito: o envelope suportado está escrito em
  [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md), e furos nele são achados.

## Preparando o ambiente

Você precisa de Rust (a MSRV está fixada como `rust-version` no `Cargo.toml`),
Node 20+, e — para qualquer coisa que toque um cluster — minikube ou outro
Kubernetes local.

```sh
git clone https://github.com/tornis-tecnologia/veloxsearch-oss.git
cd veloxsearch-oss
cargo build            # o control plane; a feature padrão é "ssr"
cargo test             # os testes são módulos #[cfg(test)] inline
cd frontend && npm ci && npm run build
```

O loop local completo — rodar o backend e o servidor de desenvolvimento do Vite
juntos, construir a imagem, rodar os testes que precisam de Postgres ou de um
checkout do registry — está em [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

**Segurança fora do cluster.** O `src/k8s.rs` cai para um namespace
deliberadamente inexistente, `veloxsearch-dev`, e o `ensure_namespace_exists`
transforma toda escrita numa recusa barulhenta. Isso existe para que uma máquina
de desenvolvimento apontada para um kubeconfig de produção nunca dirija produção
em silêncio. Não remova essa guarda, e não a "conserte" apontando para um
namespace que exista.

## As duas convenções que sustentam o projeto

Leia estas antes de escrever um handler ou mexer na camada Kubernetes. Não são
preferências de estilo — são a razão pela qual certas classes de bug não podem
acontecer.

### 1. Posse é um tipo, não uma verificação (`src/scope.rs`)

Um handler que nomeia um deployment precisa transformar a `String` controlada
pelo atacante em um `Deployment`, e a única forma de cunhar um é através de um
`Scope` derivado do cookie de sessão assinado. `Deployment` tem campos privados,
nenhum construtor público e nenhum `From<&str>`. A camada Kubernetes recebe
`&Deployment`:

```rust
k8s::delete_cluster(&req.name)                        // não compila
k8s::delete_cluster(&scope.require(&req.name).await?) // o único caminho
```

Uma verificação de posse esquecida é, portanto, um erro de compilação — não um
incidente de segurança. **Nunca** adicione uma saída de emergência que aceite
`&str` na camada Kubernetes, e nunca amplie os construtores de `Deployment`.

O `Scope::resolve` retorna `None` de forma idêntica para "não existe" e "é de
outra pessoa" — mesmo namespace, mesmo seletor de labels, mesmo caminho de
código. Essa propriedade anti-enumeração é por construção; preserve-a ao mexer
em buscas.

### 2. Módulo puro, e quem escreve é o `k8s.rs`

`snapshot`, `upgrade`, `auth_provider`, `provisioning`, `activity` e
`otel_stack` são deliberadamente puros: renderizam artefatos e decidem regras
sem nenhuma chamada ao cluster, e o `k8s.rs` faz as escritas. É isso que os torna
testáveis sem cluster. Mantenha lógica nova do lado puro e deixe o `k8s.rs`
aplicá-la.

## Convenções de código

- **Todo arquivo-fonte começa com o cabeçalho de duas linhas** (`.rs`, `.jsx`,
  `.js`, `.css`, `.sh`):

  ```
  // Copyright (C) 2026 Tornis Desenvolvimento
  // SPDX-License-Identifier: AGPL-3.0-only
  ```

  O CI verifica isso mecanicamente, então um cabeçalho faltando quebra o build
  em vez de consumir a revisão.

- **Comentários explicam o *porquê*, e citam a decisão.** Este código é
  incomumente denso em comentários de propósito. Um comentário que repete o
  código é ruído; um comentário que registra por que uma alternativa óbvia foi
  rejeitada é o objetivo. Cite a ADR ou a issue onde a decisão vive (`ADR-039`,
  `#75`).

- **Uma dependência nova carrega um comentário que a justifica** no
  `Cargo.toml`. Para crates de TLS/cripto, explique por que ela reaproveita o
  provider rustls `aws-lc-rs` do processo em vez de trazer `ring` como um
  segundo.

- **Configuração são variáveis de ambiente, todas prefixadas `VELOX_`.** Feature
  flags nascem **desligadas** e preservam o comportamento anterior quando não
  definidas.

- **Testes são `#[cfg(test)] mod tests` inline** no fim do módulo, não um alvo
  Rust em `tests/` — `tests/` guarda os scripts Python de ponta a ponta.
  `src/scope/tests.rs` é a única exceção separada.

- **Adicionar um endpoint** significa quatro coisas, e o método HTTP precisa
  bater dos dois lados: um DTO em `src/api.rs`, um handler em `src/api.rs`, uma
  linha em `routes()` e um wrapper em `frontend/api.jsx`.

- **Frontend**: React 18 puro, sem router e sem biblioteca de estado. Arquivos
  `.jsx` planos, não uma árvore `src/`. Texto de interface vai no `i18n.jsx`,
  nunca inline numa tela. O `localStorage` guarda só preferências de interface —
  nunca estado do servidor, nunca dados falsos.

- **Pacotes do registry são dados, não código.** A interpolação é um conjunto
  fechado de exatamente oito tokens (`integrations::CLOSED_TOKENS`) e a
  verificação de assinatura é a fronteira de segurança inteira
  (`catalog::verify_package`). Não adicione um bypass, e não amplie o conjunto de
  tokens sem antes mudar a especificação.

## Fazendo uma mudança

1. **Abra uma issue primeiro** para qualquer coisa além de correção de bug ou de
   documentação. É mais barato discordar sobre a abordagem numa issue do que num
   PR pronto.
2. **Crie o branch a partir de `main`.** Nomeie pelo que ele faz:
   `fix/mensagem-pvc-pending`, `feat/receita-kafka`.
3. **Mantenha o PR em um assunto só.** Um refactor e uma mudança de
   comportamento no mesmo diff levam três vezes mais tempo para revisar.
4. **Atualize a documentação no mesmo PR.** Se você renomeou um campo de
   formulário ou remodelou uma estrutura `nav.tabs`, confira os
   `tests/*_check.py` — os testes ponta a ponta dirigem widgets renderizados, não
   URLs, então uma renomeação de interface pode quebrá-los.
5. **Assine seus commits** (veja abaixo).
6. **Abra o PR** contra `main` e preencha o template.

### Mensagens de commit

Presente do indicativo, imperativo, e um corpo que diz o *porquê* quando o *o
quê* não é auto-evidente:

```
capacity: propor pools por memória alocável, não por contagem de nós

Um cluster de 3 nós com 2 GiB livres por nó não hospeda o pool que a
contagem de nós sozinha sugeriria. Dimensionar pela memória alocável faz
a proposta falhar alto no planejamento em vez de no escalonamento (#118).

Signed-off-by: Seu Nome <voce@example.com>
```

### Developer Certificate of Origin

Este projeto usa o [DCO](https://developercertificate.org/) em vez de um CLA.
Todo commit precisa carregar uma linha `Signed-off-by`, que o `git commit -s`
adiciona para você. Ela certifica que você escreveu o patch ou tem o direito de
submetê-lo sob a licença AGPL-3.0-only. Use nome real e email real.

Para assinar commits que você já fez:

```sh
git rebase --signoff main
```

## O que o CI verifica

Todo PR roda o [`.github/workflows/ci.yml`](.github/workflows/ci.yml):

| Job | O que ele exige |
| --- | --- |
| `rust` | `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, mais os testes `#[ignore]` contra um serviço Postgres |
| `msrv` | O crate ainda compila na `rust-version` fixada no `Cargo.toml` |
| `frontend` | `npm ci && npm run build` |
| `image` | O `deploy/Dockerfile` constrói (nada é publicado) |
| `supply-chain` | `cargo deny check` — licenças, advisories, crates banidos, origens |
| `headers` | Todo arquivo-fonte carrega o cabeçalho SPDX |

Rode os rápidos localmente antes de dar push:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

## Revisão

Os mantenedores estão listados em [`.github/CODEOWNERS`](.github/CODEOWNERS).
Espere uma primeira resposta em até uma semana; cutuque o PR se ficar mais tempo
que isso em silêncio. Uma mudança que toque `src/scope.rs`, `src/auth.rs`,
`src/catalog.rs` ou o caminho de assinatura recebe uma leitura mais cuidadosa que
o resto — essas são as fronteiras de segurança.

## Segurança

**Não** abra uma issue pública para uma vulnerabilidade. Reporte-a de forma
privada via GitHub Security Advisories; veja o [SECURITY.md](SECURITY.md).
