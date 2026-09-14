# Validação externa da instalação

*[Read in English](EXTERNAL-VALIDATION.md)*

Toda instalação e atualização do VeloxSearch até hoje foi feita por quem o
construiu. Essas pessoas sabem onde estão as arestas e desviam delas sem
perceber. Esta rodada pede a quem nunca usou o VeloxSearch que o instale partindo
só do README e conte, com olhar crítico, o que aconteceu.

A pergunta não é se um especialista consegue colocá-lo no ar. É se alguém que
chega agora consegue instalar, acessar a interface e ter um deployment verde
sozinho.

A rodada é acompanhada na
[#60](https://github.com/tornis-tecnologia/veloxsearch-oss/issues/60). Há dois
papéis; você pode assumir um ou os dois.

- **Testadores de instalação** — o [roteiro de instalação](#o-roteiro-de-instalação) abaixo.
- **Revisores de segurança** — [uma seção separada](#revisão-de-segurança) no final.

---

## Para quem é

Você é um bom testador de instalação se tem familiaridade com o `kubectl`,
consegue subir um cluster k3s ou minikube de um nó seguindo a documentação dele e
sabe ler os logs de um pod. **Você não precisa saber nada de VeloxSearch nem de
OpenSearch** — a falta de contexto é justamente o que estamos testando.

## Do que você precisa

- **Um cluster seu, descartável.** Não um cluster compartilhado, nem produção.
  Instalar o VeloxSearch usa cluster-admin uma vez e instala componentes no
  cluster inteiro (cert-manager, o operator do OpenSearch, Longhorn).
- **Uma máquina ou VM amd64**, idealmente com 4 vCPU, 12 GiB de RAM e 60 GB de
  disco.
- **Acesso de saída à internet** a partir do cluster, para baixar imagens.
- Um arquivo de texto para anotações, e um relógio.

Os requisitos exatos são parte do que o README deveria te dizer, por isso a lista
acima é curta de propósito.

---

## As regras

1. **Comece só pelo [`README.md`](../README.md).** Siga os links que ele oferece
   quando você decidir que precisa, e anote qual documento abriu e por quê.
2. **Anote cada ponto em que você travou, teve que adivinhar ou abriu o código-fonte**
   — ou pesquisou nas issues ou na web. Registre o horário de cada um. Essas notas
   são a parte mais valiosa do seu relato.
3. **Não pergunte ao time durante o teste.** Se travar, anote e então encontre
   sozinho um caminho ou pare. Um teste que parou é um resultado, e onde ele parou
   é exatamente o que precisamos saber.
4. **Diga quando usou conhecimento de fora.** Se você passou de algum ponto porque
   já conhece bem Longhorn, cert-manager ou a sua distribuição, diga — alguém que
   chega agora não conheceria.
5. **Seja crítico.** Atrito descrito com precisão ajuda mais que um resumo gentil.

---

## O roteiro de instalação

Execute os cenários em ordem, no mesmo cluster; cada um começa onde o anterior
terminou. Anote o horário de início e de fim de cada um.

### 1. Instalação do zero

Crie um cluster **k3s** ou **minikube** novo, de um nó, seguindo as instruções da
própria distribuição, e anote a versão. Depois instale o VeloxSearch partindo do
README.

**Pronto quando** você criou a conta de administrador, a tela de conformidade
passou e as abas principais aparecem.

Anote qual caminho de instalação usou, o que a tela de conformidade mostrou e
qualquer coisa que você mesmo teve que instalar no nó.

### 2. Criar um deployment de observabilidade

Pela interface, crie um deployment com o propósito **Observability** e o menor
tamanho.

**Pronto quando** o deployment fica verde e você consegue abrir o Dashboards dele.
O tempo entre o primeiro comando de instalação e este ponto é o seu *tempo até o
primeiro deployment verde*.

Se travar, registre o que a tela de atividade diz antes de investigar mais.

### 3. Instalar uma integração

Pela aba **Integrations** do deployment, instale uma integração.

**Pronto quando** os dados dessa integração aparecem no Dashboards.

### 4. Desinstalar

Remova o que instalou, na ordem inversa: a integração, depois o deployment,
depois o próprio VeloxSearch.

**Pronto quando** você acredita que o cluster voltou ao estado de antes do
cenário 1.

Se a documentação não diz como fazer algum passo, isso é um achado: registre o
que você tentou e o que ainda ficou no cluster depois (namespaces, CRDs, volumes).

### 5. Atualização — *aguardando a [#54](https://github.com/tornis-tecnologia/veloxsearch-oss/issues/54)*

**Não** execute este cenário ainda. O contrato de atualização está sendo definido
na #54; quando ela for concluída, esta seção vai dizer qual release instalar
primeiro e onde estão as instruções de atualização.

---

## O que registrar

**Ambiente**

- Distribuição e versão, e a saída de `kubectl version`
- Número de nós; CPU, RAM e disco por nó; sistema operacional e versão do kernel
- A release do VeloxSearch que você instalou — se usou `releases/latest`, a tag
  para a qual ela apontava

**Linha do tempo** — horários de: primeiro comando de instalação, interface
acessível, conta de administrador criada, conformidade aprovada, abas principais
visíveis, deployment verde, dados da integração visíveis.

**Registro de atrito** — uma entrada por ponto de travamento, adivinhação ou
leitura de código:

```
00:31 · <documento e seção> · esperava <…> · aconteceu <…> · adivinhei / travei / abri o código · como passei
```

**Evidências** de tudo que falhou — o texto do erro ou uma captura de tela, e:

```bash
kubectl -n veloxsearch-system logs deploy/veloxsearch --tail=200
kubectl get storageclass
kubectl get nodes -o wide
```

Remova credenciais, tokens e hostnames internos antes de compartilhar qualquer
coisa.

---

## Como relatar

Abra uma issue **[Install report](https://github.com/tornis-tecnologia/veloxsearch-oss/issues/new?template=install_report.yml)**
e cole suas anotações nela. Um relato por execução. O formulário é em inglês, mas
pode responder em português.

- Você não precisa abrir uma issue para cada problema. Os mantenedores
  transformam cada bloqueio e ponto de confusão do seu relato em uma issue
  própria, ligada à #60.
- O relato de um teste que parou no cenário 1 é tão útil quanto o de um que foi
  até o fim.
- Se encontrou algo que parece um problema de segurança, **deixe fora do relato**
  e siga [a seção de segurança](#revisão-de-segurança).

---

## Revisão de segurança

Esta parte é para revisores com foco em segurança que querem tentar quebrar o que
o VeloxSearch publica. É separada do roteiro de instalação: você pode fazer os
dois, mas relate cada um pelo seu canal.

### Escopo

As superfícies publicadas:

- **Autenticação da interface** — a criação do administrador no primeiro acesso,
  o login e o cookie de sessão, incluindo o acesso à interface pelo Ingress
  catch-all criado em clusters com uma IngressClass padrão. O modelo de contas
  está descrito em [`auth/accounts.md`](auth/accounts.md).
- **O Ingress do Dashboards** — o Dashboards de cada deployment publicado em
  `https://<deployment>.<base-domain>` no modo ingress (a seção de domínio e TLS
  do [`INSTALL.md`](INSTALL.md)).
- **Ingestão OTLP** — as rotas OTLP públicas que um deployment publica depois que
  o Observability Stack é instalado pela aba Integrations, junto com a credencial
  delas e a lista de IPs permitidos do deployment.

Leia antes o modelo de ameaças e a lista do que está fora de escopo no
[`SECURITY.md`](../SECURITY.md): eles dizem quais propriedades o projeto promete,
para você distinguir um bug de um limite deliberado.

### Regras

- **Teste só infraestrutura sua**, ou que você tem permissão por escrito para
  testar. Instale o VeloxSearch no seu próprio cluster e ataque essa instalação.
  Nunca teste contra a instalação de outra pessoa, o registry de integrações, o
  registry de imagens, o host de download do projeto ou a hospedagem do código.
- **Nada de negação de serviço** nem teste volumétrico contra o que não é seu.
- **Relate em privado, pelo [`SECURITY.md`](../SECURITY.md)** — um security
  advisory privado. Nunca em um install report, issue pública, discussion ou
  pull request.
- **A divulgação é coordenada** com você depois que a correção sai, no prazo
  descrito no `SECURITY.md`.
