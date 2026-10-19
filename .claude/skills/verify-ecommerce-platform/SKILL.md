---
name: verify-ecommerce-platform
description: Drive the EU Commerce Platform (MatCer/ecommerce-platform) the way a shopper, merchant or API client does and capture proof. Covers bringing up your own instance from a linked worktree (native API on the worktree database, or a held full compose stack on its own ports), the storefront, checkout, edge and API surfaces, receipts, and the host traps that broke earlier verification lanes. Use for live verification of a change, a proof for a PR, or when a lane needs a running shop.
---

# Verify the ecommerce platform

Read `AGENTS.md` and README "Worktrees" first. Hard rules from them: never start, stop or seed the
main dev stack (compose project `ecommerce`, `*.localhost:8080`) from a worktree, never call
`docker compose` against it, never deploy. The dev stack belongs to the user and may run an older
build than your branch, so it is not proof of your change.

`$WT` below is your linked worktree; run `pnpm worktree:setup` there once before anything else.

## Launch: pick the cheapest instance that reaches the change

| Change touches | Instance | Cost |
|---|---|---|
| Rust API or SQL only, checked through HTTP | A. native API | 1-3 min (warm sccache) |
| Storefront, checkout, edge, admin SPA, theme, auth, worker, anything a browser sees | B. held stack | 10+ min cold, seconds to resync |
| The whole PR before merge, recorded on GitHub | C. `pnpm verify <PR#>` | about 20 min |

### A. Native API on the worktree database

```bash
cd "$WT"
mkdir -p "$EVID"   # evidence dir, see Evidence
wtdev -- make api-native >"$EVID/api-native.log" 2>&1 &   # stays in the foreground: background it
. ./.env.worktree.local                                  # APP_DB, API_PORT (wtdev's port)
until grep -q '"api listening"' "$EVID/api-native.log"; do sleep 5; done
curl -sS "http://127.0.0.1:$API_PORT/readyz"             # ready: database, meilisearch, storage ok
```

What it is: this branch's API on `127.0.0.1:$API_PORT` against `app_wt_<name>_<hash>` (migrated by
setup), sharing the dev stack's Postgres cluster, Meilisearch, MinIO, Mailpit and auth service.
Limits: the database is migrated but empty (no demo tenant; `make seed` refuses in a worktree);
`/storefront/*` needs the edge's token, so storefront calls need B; staff invitations answer 503.
After a code change: `wtdev stop`, then start it again. Teardown: `wtdev stop`.

### B. Held full stack (own compose project and ports)

`stack.sh` (next to this file) copies `$WT` into `~/.cache/ecommerce-stack/ecommerce-s<slot>`
without `.git`, writes a `.env` with project `ecommerce-s<slot>` and ports `<slot>xxxx`, then runs
`make up`, `make seed`, the review fixture seed and the e2e fixture setup there. The Makefile allows
compose in that copy because it is not a linked worktree of the dev stack; this is what
`pnpm verify` does too. Slots are 3 and 4 (1 is `pnpm verify`, 2 collides with worktree API
ports 28000-28199); `up` refuses a slot another worktree holds or a port in use.

```bash
cd "$WT"
S=$WT/.claude/skills/verify-ecommerce-platform/stack.sh
$S up 3          # prints "stack: up, shop http://demo.localhost:38080 https://demo.localhost:38443"
$S doctor 3
$S sync 3 api worker     # after Rust edits; also: edge | admin | auth | mocks
$S sync 3                # after theme or checkout edits: rebuild and publish artifacts
eval "$($S env 3)"       # COMPOSE_PROJECT_NAME, HTTP(S)_PORT, PG_PORT, MAILPIT_UI_PORT, E2E_RATE_SECRET
$S down 3
```

Slot 3 URLs: shop `http://demo.localhost:38080` (CZ), `http://demo-sk.localhost:38080` (SK),
`https://demo.localhost:38443` (TLS + h2), checkout `http://checkout.demo.localhost:38080`, API
`http://api.localhost:38080`, admin `http://admin.localhost:38080`, Mailpit
`http://127.0.0.1:38025`. Slot 4 swaps the leading 3 for 4. The compose log is
`~/.cache/ecommerce-stack/ecommerce-s<slot>.log`; services: `docker logs ecommerce-s3-api-1`.

### C. `pnpm verify <PR#> [--full]`

Builds a throwaway stack on 18080 from the PR head, runs scoped (or `--full`) cargo, vitest, e2e,
image smoke and perf, and posts the `local-verify` commit status to GitHub (a visible side effect).
Needs an open, non-draft, mergeable PR that contains the current base tip. One run at a time on
the host (lock in `~/.cache/ecommerce-verify-merge`). Full runs in the SHOP-54 round took
1060-1190 s. Results of a failed run: `~/.cache/ecommerce-verify-merge/pr-<n>-test-results`.

## Doctor

Run before driving and whenever something looks off.

- A: `ss -ltnpH "sport = :$API_PORT"` shows an `api` process; `curl -sS 127.0.0.1:$API_PORT/readyz`
  is `{"status":"ok",...}`; `.env.worktree.local` names this worktree's `APP_DB`.
- B: `$S doctor 3` prints the owner (must be your worktree), the commit copied at the last
  up/sync versus your HEAD (resync if they differ), each service's state and health, and 200 for
  `api.localhost/readyz`, the shop over HTTP and over HTTPS.

## Drive

Feature recipes live in [`features/`](features/README.md). Harnesses, in order of preference:

1. **The repo's Playwright specs** against your stack. B:
   `$S e2e 3 --project chromium --no-deps checkout/orders.spec.ts -g "guest checkout"`.
   Projects: `chromium`, `firefox` (handoff only), `chromium-shared-state`, `chromium-themes`
   (exclusive suites; run them alone). `--output "$EVID/pw"` keeps traces with the evidence.
2. **Ad-hoc Playwright scripts** for a journey no spec covers. Put the script in a scratch dir
   (not the worktree) and import Playwright by absolute path:
   `const { chromium, firefox } = await import("$WT/e2e/node_modules/@playwright/test/index.mjs")`.
   The e2e helpers (`e2e/checkout/support.ts`, ...) use extensionless imports and only load inside
   the test runner, so inline what you need: the consent cookie
   (`{ name: "consent", value: "", url: <shop origin> }`, keeps the banner closed), `locale: "cs-CZ"`,
   and for HTTPS `ignoreHTTPSErrors: true`. Many requests from one context can hit rate limits; the
   specs send `x-e2e-rate-key: <id>.<hmac-sha256(E2E_RATE_SECRET, id)>` (see `e2e/rate-client.ts`).
3. **Playwright CLI one-shots**:
   `pnpm --filter @platform/e2e exec playwright screenshot -b firefox --ignore-https-errors <url> "$EVID/x.png"`.
4. **curl** for edge headers and the API (`curl -k` for HTTPS).
5. **Smoke scripts** for API flows with a staff JWT: from the held copy,
   `cd ~/.cache/ecommerce-stack/ecommerce-s3 && scripts/smoke-staff-flow.sh` (also
   `smoke-catalog.sh`, `smoke-pricing.sh`); they read that copy's `.env`.

Stable handles: ARIA roles and names in Czech on the CZ shop (`navigation "Hlavní navigace"`,
`button "Přidat do košíku"`, `heading "Pokladna"`), `data-testid` on checkout and order pages
(`checkout-total`, `order-status`, `payment-status`), product slugs from the seed (`tricko-basic`,
`tricko-henley`, `mikina-crew`). SQL reads for side effects: `docker exec ecommerce-s3-postgres-1
psql -U postgres -d app -At -c '<query>'` (B only; A uses the worktree database with
`psql "postgres://app_owner:<APP_OWNER_PASSWORD>@localhost:<PG_PORT>/$APP_DB"` from `.env`).

## Evidence

Write everything to `EVID=~/.agents/verify/ecommerce-platform/<branch-or-task>-<utc-stamp>/` (an
autopilot lane may use its own run dir instead). It is outside the worktree and outside the held
copy, so no cleanup step touches it. Per proof keep:

- `head.txt`: `git rev-parse HEAD` and `$S doctor` output (what was driven, at which commit).
- The action and the result: request plus status, headers and body (`curl -sS -D -`); for UI, a
  screenshot and `locator("body").ariaSnapshot()` before and after the action.
- The side effect read through a second view: the order page and its Mailpit email, a SQL row, the
  admin list. A success toast alone is not proof.
- Playwright output dirs and traces for spec runs.

Proof drives the real user path: the storefront through the edge, checkout through the cart's
"K pokladně" handoff, the fake gateway's Pay button. Do not prove a storefront change with native
API calls or by setting rows that the user path would write. The only mocks are the
compose-provided provider stand-ins (stripe-mock, the fake gateway, carrier, bank and mail mocks).

## Cleanup

- A: `wtdev stop` in `$WT`.
- B: `$S down <slot>`: removes the containers, volumes, images tagged `ecommerce-s<slot>-*:local`
  and the copy; keeps its log. Down only the slot you brought up (`doctor` names the owner).
- Scratch scripts: delete your scratch dir. Never kill processes by name.
- The worktree itself: `pnpm worktree:teardown` drops its database; then `git worktree remove`.
- Evidence under `~/.agents/verify/...` stays.

## Traps (all hit during the SHOP-54 run)

- **Never move `XDG_CACHE_HOME`** (or `PLAYWRIGHT_BROWSERS_PATH`). Playwright's browsers live in
  `~/.cache/ms-playwright`, and `pnpm verify` keeps its lock and cargo target in
  `~/.cache/ecommerce-verify-merge`. Isolate scratch with `TMPDIR=<run dir>` instead.
- **No global `umask 077`.** Under it the checkout's `docker/postgres/init-roles.sh` becomes mode
  700, the bind-mounted init script is unreadable in the container, and `migrate` fails at
  startup. Keep the default umask and `chmod 600` only the secret files you write. `stack.sh`
  copies with `go+rX`; `pnpm verify` has no such guard.
- **Local TLS is a Caddy internal CA that is not installed in the host trust store.** Use
  `curl -k`, `ignoreHTTPSErrors: true`, or Chromium's `--ignore-certificate-errors` (what
  `make perf` does). WebKit drops `Secure` cookies over plain HTTP, so cookie flows in WebKit need
  the HTTPS origin.
- **The T3 in-app browser cannot load pages on this host (AppArmor blocks it).** Do UI checks with
  Playwright against your own stack.
- **Firefox new tabs on COOP pages:** shop and checkout pages send
  `Cross-Origin-Opener-Policy: same-origin`, and Playwright's Firefox driver can drop the
  navigation commit of a new tab whose first navigation is such a page, so `goto` hangs. Open the
  tab on `<origin>/favicon.svg` first, then the page (`shopTab` in `e2e/checkout/support.ts`).
- **HTTP/3:** main's Caddyfile serves h1 and h2 only. A stack started from an older checkout still
  sends `alt-svc: h3=":443"`, which the host cannot reach and Firefox may follow; rebuild from a
  current tree instead of debugging it. On 2026-10-19 the dev stack still sent it.
- **`pnpm verify` takes about 20 minutes** and holds the host-wide lock; iterate on B and run
  verify once at the end.
- **Measure performance through Caddy** (`make perf`, or HTTPS on the held stack): it gzips like
  production. A custom proxy or native edge without compression added about 700 ms under
  throttling and produced a false regression.

## Status of this recipe

Exercised when the skill was added (2026-10-19, main `ec7c23d`): A end to end (launch, doctor,
`api.md` health/OpenAPI/auth gate, cleanup), plus read-only probes of the dev stack for the
browser recipe (Chromium home page, Firefox second tab via `/favicon.svg` over HTTPS with
`ignoreHTTPSErrors`) and the edge headers in `edge.md`. Not yet run: `stack.sh up/sync/e2e/down`
(B), generalized from the SHOP-121/122 lane scripts that did run, and the storefront and
checkout journeys on a held stack. The first lane that runs B fixes what fails here and deletes
this paragraph.
