# ADR-067 — The console as a product shell: URLs, a sidebar of places, tabs by task

**Status:** proposed
**Date:** 2026-11-07

Builds on ADR-032 (the React SPA; see the [index](README.md)). No backend
contract changes except one: the login redirect keeps the page that was asked
for (`/login?next=…`).

## Context

The console was built as an operator tool for one installation and it showed:
the only navigation was a row of tabs, "Criar" sat in it as if it were a
place, every screen lived at the same URL, and a deployment's six tabs were
grouped by how the code is organised (Edit / Integrations / Snapshot /
Security / Auth), not by what the person came to do. A usability review done
on 0.11.0 against a live k3s found, among 27 items, four that block selling
the product as a service:

- **Nothing had a URL.** F5 on a deployment's Backup tab returned to the list;
  the browser's Back button left the console. An alert, a ticket or a doc link
  cannot point at a screen.
- **Numbers on one screen contradicted each other.** On a node-local volume
  the Overview showed disk at 2930% (red) on the node rows, 2.0 GB on the
  storage tile and 439 GB on the trend chart, because three code paths read
  the same `capacity_enforced` fact three different ways.
- **Failures looked like successes.** The one toast component always drew a
  green check mark; `save_cluster` errors passed through it and vanished after
  2.2 s. The login refusal reached a Portuguese form in English.
- **The destructive action was everywhere.** "Excluir cluster" sat under all
  six tabs, one click from someone who only came to read a graph.

Smaller findings pointed the same way: the whole interface in a monospace
face, health shown as `green`/`yellow`/`red`, `cluster` and `deployment` used
for the same thing, ADR and issue numbers in user-facing text, a modal without
`role="dialog"`, Esc or a focus trap, a prototyping panel in the production
bundle.

## Decision

1. **Every screen has a URL** (`frontend/router.jsx`, History API, no library):
   `/deployments`, `/deployments/new`, `/deployments/:id/:tab`, `/capacity`,
   `/settings`. The tab names before this ADR (`edit`, `integrations`,
   `snapshot`, `security`, `auth`) are accepted as aliases. `/login` and
   `/setup` stay the boot gate's; the server's unauthenticated redirect becomes
   `/login?next=<path>` for a plain in-app path and the SPA returns there after
   the session exists. The app still has no state library.

2. **A sidebar of places, actions as buttons.** The shell is a sidebar
   (Deployments, Capacity, Settings, help, account menu) and a top bar with the
   breadcrumb. Create, upgrade and delete are buttons on the screens they act
   on. Theme and language live in the account menu; the build identity lives
   only in Settings › About.

3. **Deployment tabs by task:** Overview · Data & integrations · Backup ·
   Access · Configuration · Activity. Access holds credentials, the network
   rule and the identity provider; Configuration holds size, retention, the
   Dashboards interface and, last, delete. Delete is also in the header's ⋯
   menu and nowhere else. Type-to-confirm stays (ADR-050 invariant 4: never
   locked).

4. **One source per fact on the Overview.** The storage tile, the node rows and
   the trend chart read `capacity_enforced` once: on an enforced volume they
   show used/reserved; on a node-local one they show the data size and no
   percentage anywhere. A node is emphasised only past the meter's own
   threshold, in that cell.

5. **Feedback that tells the truth.** The toast has four kinds; an error
   stays until closed and never wears a check mark. The login refusal is
   translated from its status, not echoed. One `Dialog` primitive (role,
   label, Esc, focus kept inside and returned) under every modal.

6. **One vocabulary** (`frontend/i18n.jsx`): *deployment* for what the console
   manages, *cluster* only for the Kubernetes cluster; *backup*, never
   *snapshot*, in user-facing text; health as a word in the user's language
   through `StatusPill` (`green` never reaches the screen); sizes named; no
   ADR or issue numbers in strings; numbers formatted per language with `Intl`.

7. **Sans for the interface, mono for data.** IBM Plex Sans on labels,
   buttons, tabs and body; IBM Plex Mono only on resource names, addresses,
   numbers and code. State colours get a text variant that passes 4.5:1 on
   both themes.

8. **Stable test hooks.** The end-to-end scripts drive `data-testid`s
   (`nav-*`, `create-deployment`, `dep-open-<id>`, `dep-tab-<key>`,
   `delete-deployment`) and assert URLs, not tab positions.

## Consequences

- A link now means something: `/deployments/logs-a1b2/backup` opens that tab
  for whoever has a session, and asks for one first otherwise.
- The e2e scripts changed selectors in the same PR; reshaping a screen still
  means checking them, but by test id rather than by `nth-child`.
- The design-tweaks panel is loaded lazily and only when `import.meta.env.DEV`;
  it is absent from the production bundle.
- Out of scope, deliberately: account self-service (profile, password reset,
  team, roles), usage and billing, backup history and restore. Those need
  backend surfaces and are the next phase; this ADR only puts the shell they
  will hang off in place.
