# Tier 5 — Rust full-stack smoke (Cargo workspace + Axum + sqlx-Postgres + CLI) — 2026-08-11

**Status (2026-08-11):** Plan draft. Spec written, smoke runner script in `scripts/tier5-smoke.sh`, smoke not yet fired.

**Significance:** Tier 5 is the first **Rust multi-crate workspace** smoke. Tier 4 proved ALPS on a Python monorepo (`backend/` + `frontend/`); Tier 5 proves it on a Rust monorepo (Cargo workspace with 3+ crates sharing one `Cargo.lock`). The structured-DoD monorepo recursion (`d92ad99`) was verified for Python (depth-1 `pyproject.toml` walk); Tier 5 stresses the same detector logic on a different monorepo shape (`Cargo.toml` + `Cargo.lock` + nested `crates/*/Cargo.toml`). It also surfaces the Tier 1 single-crate DoD at the `cargo test --workspace --quiet` level — not just `cargo test`.

## Why Tier 5 was the right next move (over Turborepo / Go-gRPC / SaaS multi-tenant)

- **Rust DoD path is the least-smoke-covered.** One real verification (smoke #6, 2026-07-27, single-crate `cargo test`). The full multi-crate workspace path has never been exercised end-to-end.
- **Different monorepo shape.** Tier 4's `backend/` + `frontend/` is two independent project roots. A Cargo workspace is *one* `Cargo.toml` at the root with member crates — `detect_project_type` must return `(ProjectType::Rust, <workspace root>)` rather than crashing on multiple `Cargo.toml`s, and `cargo test --workspace` must run from the workspace root, not from any member.
- **sqlx + Postgres is genuinely new.** Tier 4 used SQLAlchemy; Tier 5 uses sqlx with compile-time-checked queries (different failure modes — missing `DATABASE_URL` at compile time vs runtime, migrations vs auto-create).
- **A CLI in the same workspace** adds cross-crate dependency boundaries the orchestrator has to plan: the API crate reads from the `core` crate's models, the CLI calls into `core` directly. Tests in one crate must not silently use mocks that override the real `core` types.
- **No new Tier-2/3 DoD shape needed.** This is still Rust at the root, still `cargo test --workspace`. Tier 5 is *not* an excuse to add the workspace-recursion as a §12 P-item — it should Just Work on the post-`d92ad99` detector. If it doesn't, *that's the finding*.

## What Tier 5 verifies

After this smoke passes end-to-end, we can claim:

1. ALPS can decompose a **Rust Cargo workspace** from a single prompt — multi-crate, not single-crate.
2. The structured Rust DoD path fires on `cargo test --workspace --quiet` (not just per-crate).
3. The structured-DoD monorepo detector handles a Rust workspace shape (one root `Cargo.toml` + nested member `Cargo.toml`s) without mis-classifying.
4. sqlx + Postgres lifecycle: DB reachability check, compile-time query check, runtime query check, migrations.
5. Axum (Tier-4 FastAPI equivalent in Rust) — auth, JWT, per-user CRUD.
6. Cross-crate Rust patterns — `core` crate owns models, `api` + `cli` consume them. Workspace-level tests work.
7. Real CLI binary (`cargo run --bin cli`) — tests that the orchestrator plans executable artifacts, not just a library.

## Scope decision — IN vs OUT

**IN:**
- Backend: Axum 0.7 + sqlx 0.8 (Postgres) + tokio + serde + jsonwebtoken + argon2 + thiserror + anyhow
- CLI: clap 4 (derive API) + rusqlite-or-sqlx for local-mode notes (so the CLI works without the API server — proves cross-crate `core` reuse)
- `core` crate: domain types (`User`, `Note`), validation, error types — pure logic, no IO
- DB: Postgres 16 (existing local instance, same `alps_tier5` database we create at smoke time)
- Auth: JWT (HS256), argon2 password hashing, 24h TTL — same shape as Tier 4 but in Rust
- Per-user notes CRUD: 4 endpoints + per-user isolation test
- Tests: per-crate `cargo test` AND workspace-level `cargo test --workspace` — must all pass
- Verification: build, test, server startup, curl flow, DB schema dump, CLI invocation

**OUT:**
- WebSockets / real-time
- File uploads
- Multi-tenancy beyond per-user ownership
- OAuth / SSO / password reset / email verification
- Docker Compose / production deploy
- TLS / rate limiting / background jobs
- Frontend (no Vite/React this time — keep the smoke tight; frontend was the bulk of Tier 4's wall clock)

## App shape

A **CLI + HTTP API for a per-user notes service** in a Cargo workspace. Same core domain as Tier 4, expressed in Rust.

```
notes-tier5/                          # Cargo workspace root
├── Cargo.toml                        # [workspace] members = ["crates/core", "crates/api", "crates/cli"]
├── Cargo.lock                        # workspace-pinned
├── README.md
├── crates/
│   ├── core/                         # pure logic, no IO
│   │   ├── Cargo.toml                # no tokio, no sqlx
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── models.rs             # User, Note (sqlx::FromRow impls)
│   │       ├── auth.rs               # password hashing, JWT encode/decode (pure)
│   │       └── error.rs              # thiserror enum
│   ├── api/                          # Axum + sqlx, depends on core
│   │   ├── Cargo.toml                # tokio, axum, sqlx, jsonwebtoken
│   │   └── src/
│   │       ├── main.rs               # axum::serve, router
│   │       ├── db.rs                 # sqlx PgPool, migrations
│   │       ├── auth.rs               # JWT middleware extractor
│   │       ├── handlers/
│   │       │   ├── mod.rs
│   │       │   ├── auth.rs           # POST /api/auth/register, /api/auth/login
│   │       │   ├── notes.rs          # GET/POST/PUT/DELETE /api/notes
│   │       │   └── healthz.rs
│   │       └── tests/                # integration tests against real Postgres
│   │           ├── common.rs         # spawn_app() helper, per-test tx rollback
│   │           ├── auth.rs
│   │           └── notes.rs
│   └── cli/                          # clap binary, depends on core
│       ├── Cargo.toml                # clap, rusqlite (or just core for offline mode)
│       └── src/
│           └── main.rs               # `notes <add|list|delete> <body>` etc.
└── migrations/
    └── 001_initial.sql               # users + notes tables
```

## Acceptance criteria

1. `cargo build --workspace` exits 0 from the workspace root.
2. `cargo test --workspace` exits 0 with all unit + integration tests passing (expect 8+ across the 3 crates).
3. `cargo run --bin notes -- list` (with a local SQLite DB at `./.notes.db`) returns an empty list (proves CLI builds + runs against core models).
4. `cargo run --bin api` starts on `127.0.0.1:8080`; `curl http://127.0.0.1:8080/api/healthz` returns 200 `{"status":"ok"}`.
5. Live curl flow:
   - `POST /api/auth/register {email, password}` → 201
   - `POST /api/auth/login {email, password}` → 200 + JWT
   - `GET /api/notes -H "Authorization: Bearer <token>"` → 200 + `[]`
   - `POST /api/notes -H "Authorization: Bearer <token>"` → 201
   - `GET /api/notes` (no token) → **401**
   - User A's note → `PUT /api/notes/<id>` with user B's token → **404** (per-user isolation, no existence leak)
6. CLI works: `cargo run --bin notes -- add "hello from cli"` writes to local `.notes.db`; `cargo run --bin notes -- list` shows the entry.
7. `cargo clippy --workspace -- -D warnings` exits 0 (or has only known-acceptable warnings).
8. Containment ACs: `artifacts/filesystem_inventory.txt` + `artifacts/tmp_listing.txt` exist.

## Tier 5 prompt (final, to be pasted into `/tmp/alps-tier5-notes-prompt.txt`)

```text
Build a Rust full-stack notes service at /tmp/alps-tier5-notes as a Cargo
workspace with three crates: `core` (pure logic), `api` (Axum + sqlx +
Postgres), and `cli` (clap binary using `core` types). It must have real
JWT auth, real Postgres persistence, and a CLI that works offline against
a local SQLite database by reusing the same `core` models. No frontend,
no Docker Compose, no deployment.

The deliverable directory is /tmp/alps-tier5-notes. Create it with
mkdir -p. Organize the project as a Cargo workspace:

  /tmp/alps-tier5-notes/
    Cargo.toml                       # [workspace] members, resolver = "2"
    Cargo.lock                       # committed
    README.md
    .gitignore                       # target/, .notes.db, *.swp
    migrations/
      001_initial.sql                # CREATE TABLE users + notes
    crates/
      core/
        Cargo.toml                   # no tokio, no sqlx, no axum — pure logic only
        src/
          lib.rs                     # pub mod models; pub mod auth; pub mod error;
          models.rs                  # User, Note (with serde + sqlx::FromRow derives)
          auth.rs                    # hash_password, verify_password (argon2), encode_jwt, decode_jwt
          error.rs                   # thiserror enum with variants for the domain
      api/
        Cargo.toml                   # axum, tokio, sqlx (postgres + runtime-tokio-rustls),
                                     # jsonwebtoken, argon2, serde, thiserror, anyhow
                                     # depends on core via path
        src/
          main.rs                    # #[tokio::main], axum::serve, bind 127.0.0.1:8080
          db.rs                      # PgPool init + sqlx::migrate!
          auth.rs                    # axum extractors: AuthUser(claims) + require_auth middleware
          handlers/
            mod.rs
            auth.rs                  # POST /api/auth/register, /api/auth/login → 201/200 + JSON
            notes.rs                 # GET/POST/PUT/DELETE /api/notes (auth-gated)
            healthz.rs               # GET /api/healthz → {"status":"ok"}
          tests/                     # integration tests (use the api crate's main helpers)
            common.rs                # spawn_app() → TestApp { address, pool }
            auth.rs                  # 4+ tests: register/login/401/duplicate
            notes.rs                 # 4+ tests: CRUD happy + 401 + per-user isolation
      cli/
        Cargo.toml                   # clap (derive), rusqlite (bundled), serde, core (path)
        src/
          main.rs                    # `notes` binary with subcommands: add, list, delete

Backend requirements (api crate):
- Rust edition 2021, MSRV 1.75.
- Use real Postgres 16 at DATABASE_URL=postgres://gbrain:gbrain_local_pass@localhost:5432/alps_tier5.
  Create the DB at smoke time if missing:
    PGPASSWORD=gbrain_local_pass psql -h localhost -U gbrain -d gbrain -c "CREATE DATABASE alps_tier5;"
- JWT_SECRET=dev-secret-change-me (hardcoded for dev).
- Token TTL: 24h, HS256.
- All SQL via sqlx::query! / sqlx::query_as! with compile-time check (set
  DATABASE_URL at compile time via `.env` or sqlx prepare offline).
- Passwords: argon2 with default params.
- sqlx::migrate!() runs migrations on startup; migrations dir is
  `<workspace>/migrations/`.
- Per-test fixture: each integration test runs inside a transaction that's
  rolled back at teardown — real Postgres, no mocking.

CLI requirements (cli crate):
- clap derive for subcommands: `add <body>`, `list`, `delete <id>`.
- Persists to local SQLite at `.notes.db` (relative to CWD).
- Reuses core::models::Note (with serde_json serialization to TEXT column).
- No network calls — proves core types compile and serialize in a
  different consumer.

Workspace requirements:
- All 3 crates in `[workspace.members]`.
- Shared deps via `[workspace.dependencies]` (axum, tokio, serde, etc.).
- One Cargo.lock at workspace root (resolver = "2").
- `cargo test --workspace --quiet` must pass with all tests across crates.
- `cargo build --workspace` must produce 3 binaries: `api`, `cli`, plus
  the `core` library.

Captured runtime artifacts in /tmp/alps-tier5-notes/artifacts/:
- cargo_build_output.txt — `cargo build --workspace 2>&1`
- cargo_test_output.txt — `cargo test --workspace --quiet 2>&1`
- cargo_clippy_output.txt — `cargo clippy --workspace -- -D warnings 2>&1`
- api_uvicorn_startup.log — wait, no, this is Rust: api_startup.log
  showing the Axum server bind + curl healthz
- cli_invocations.txt — output of `cargo run --bin notes -- add/list/delete`
- curl_flow.txt — full register → login → CRUD transcript
- db_schema_dump.sql — pg_dump --schema-only alps_tier5
- users_in_db.txt — psql SELECT id, email FROM users
- notes_in_db.txt — psql SELECT id, owner_id, body FROM notes
- filesystem_inventory.txt — find under /tmp/alps-tier5-notes
- tmp_listing.txt — ls -la /tmp (containment AC)

Constraints:
- Write everything inside /tmp/alps-tier5-notes. Do NOT create files
  outside the deliverable directory.
- Use the existing Postgres instance — do NOT spin up a new container.
- Use only crates.io deps (no git deps, no path deps outside the workspace).
- Lock the Cargo.lock — do NOT use `cargo update` mid-smoke.
- If sqlx compile-time query check fails due to missing DATABASE_URL,
  fall back to `sqlx::query` (runtime-checked) — log the fallback in
  artifacts/cargo_test_output.txt.
```

## Verification recipe (operator runs after `# ALPS — Done`)

```bash
# 1. Build the workspace
cd /tmp/alps-tier5-notes
cargo build --workspace 2>&1 | tee artifacts/cargo_build_output.txt
# expect: 3 binaries built (api, cli, plus core lib), 0 errors

# 2. Test the workspace
cargo test --workspace --quiet 2>&1 | tee artifacts/cargo_test_output.txt
# expect: all tests passed (8+ across 3 crates)

# 3. Clippy
cargo clippy --workspace -- -D warnings 2>&1 | tee artifacts/cargo_clippy_output.txt
# expect: 0 errors (warnings allowed if explicitly justified)

# 4. CLI smoke (offline)
cargo run --quiet --bin notes -- add "hello from cli"
cargo run --quiet --bin notes -- add "second note"
cargo run --quiet --bin notes -- list
cargo run --quiet --bin notes -- delete 1
cargo run --quiet --bin notes -- list
# expect: cli writes to .notes.db, list reflects add/delete

# 5. API smoke
DATABASE_URL=postgres://gbrain:gbrain_local_pass@localhost:5432/alps_tier5 \
JWT_SECRET=dev-secret-change-me \
    cargo run --quiet --bin api &
API_PID=$!
sleep 3
curl -s http://127.0.0.1:8080/api/healthz
# expect: {"status":"ok"}

# 6. Live curl flow
TOKEN=$(curl -s -X POST http://127.0.0.1:8080/api/auth/register \
    -H "Content-Type: application/json" \
    -d '{"email":"u1@example.com","password":"hunter22"}' \
    | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('token',''))")
# if /register doesn't return a token, call /login separately
TOKEN=$(curl -s -X POST http://127.0.0.1:8080/api/auth/login \
    -H "Content-Type: application/json" \
    -d '{"email":"u1@example.com","password":"hunter22"}' \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['token'])")

curl -s -i http://127.0.0.1:8080/api/notes -H "Authorization: Bearer $TOKEN"
# expect: 200 + []
curl -s -i -X POST http://127.0.0.1:8080/api/notes \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"body":"first api note"}'
# expect: 201
curl -s -i http://127.0.0.1:8080/api/notes
# expect: 401 (no token)

# 7. Per-user isolation (register user 2, verify cannot see/edit user 1's notes)
# (similar to tier4 flow)

# 8. DB state
PGPASSWORD=gbrain_local_pass psql -h localhost -U gbrain -d alps_tier5 \
    -c "SELECT id, email FROM users ORDER BY id;"
PGPASSWORD=gbrain_local_pass psql -h localhost -U gbrain -d alps_tier5 \
    -c "SELECT id, owner_id, body FROM notes ORDER BY id;"

# 9. Containment ACs
test -f artifacts/filesystem_inventory.txt && echo ok
test -f artifacts/tmp_listing.txt && echo ok

# 10. Cleanup
kill $API_PID
```

## Smoke run ritual (Tier 5 — operator runs after this PR merges)

```bash
# ── Pre-flight ──
cd /home/kyle/Development/alps
git status  # clean
cargo build --workspace  # confirm alps binary builds
test -f /tmp/alps-tier5-notes-prompt.txt  # confirm prompt written

# Postgres pre-check
PGPASSWORD=gbrain_local_pass psql -h localhost -U gbrain -d alps_tier5 -c "SELECT 1" 2>&1
# If "database does not exist":
PGPASSWORD=gbrain_local_pass psql -h localhost -U gbrain -d gbrain -c "CREATE DATABASE alps_tier5;"

# Clean state
rm -rf /tmp/alps-tier5-notes /tmp/alps-tier5-notes-workdir
mkdir -p /tmp/alps-tier5-notes-workdir

# ── Fire the smoke via the in-repo runner ──
./scripts/tier5-smoke.sh --smoke-number 1 \
    --workdir /tmp/alps-tier5-notes-workdir \
    --deliverable-path /tmp/alps-tier5-notes \
    --prompt-template /tmp/alps-tier5-notes-prompt.txt \
    --log-prefix /tmp/alps-tier5-1-stderr
```

The runner handles: herdr workspace creation, pane setup, prompt
substitution, wrapper invocation (with the canonical tier4 wrapper), and
**filesystem/telemetry monitoring with a 4h hard ceiling** (no `herdr
wait` — that's the smoke-25 failure shape that the runner fixes by
design).

## Expected outcome + risks

**Expected:** 1-3 outer iterations, 30-60 min wall clock, 8-14 Plan stories, all passing, Judge ACCEPTED.

**Risks (ranked):**

1. **sqlx compile-time query check fails.** If `DATABASE_URL` isn't set
   at compile time, `sqlx::query!` macros refuse to compile. Mitigation:
   prompt explicitly allows runtime-query fallback (`sqlx::query` instead
   of `sqlx::query!`). If fallback fires, smoke proves cross-crate Rust
   structure but loses compile-time SQL verification depth.
2. **Cargo workspace monorepo misdetection.** The post-`d92ad99`
   `detect_project_type` walks depth-1 looking for `Cargo.toml`. A
   workspace root + member crate `Cargo.toml`s may trigger multiple
   matches. Mitigation: documented as a Tier-5 smoke-failure
   candidate — if the Judge REJECTS on a `cargo test --workspace` run
   from the wrong cwd, that's the §12 P8 finding.
3. **Cross-crate Rust trait bounds.** `core::Note` needs both `serde::Serialize` (for JSON API) AND `sqlx::FromRow` (for Postgres), and the same type needs `rusqlite`-compatible serialization for the CLI. Trait conflict resolution is non-trivial — codex may need to split into `Note` (domain) + `NoteRow` (sqlx) + `NoteDto` (api).
4. **Argon2 build time.** First `cargo build` of the api crate downloads + compiles `argon2` + its deps (~30s). Smoke budget accounts for this.
5. **Postgres connection string in `core`.** `core` is supposed to be pure (no IO) — must not depend on `sqlx` directly. The api crate does the sqlx-specific work. Mitigation: prompt is explicit that core has no `sqlx` or `tokio` deps.
6. **herdr SIGPIPE (Pattern B from smoke-runner-truncation-diagnostic).** Mitigation: dropped `tee` + `stdbuf -oL` in the wrapper per the truncation-diagnostic recipe.

## What comes AFTER Tier 5 (for the roadmap conversation, not this arc)

- **Tier 6** — Multi-service (3+ services, message queue, async jobs)
- **Tier 7** — alps-on-alps (use ALPS to build a feature in ALPS itself)
- **Tier 5b** — Turborepo (Node monorepo with real cross-package dep graph) — if Tier 5 reveals the Rust workspace shape Just Works, a Node monorepo is the next monorepo-shape variation
- **§12 P8** — if Tier 5 surfaces a real monorepo-recursion bug for Rust workspaces, that's the next P-item

## Why P7 is being deferred (note for SPEC §12)

§12 P7 (Tier-4 smoke wrapper herdr-wait timeout) is **not blocking Tier 5**
and is being deferred. The smoke-25 failure shape was the operator-side
`herdr wait output --timeout 3600000` killing a still-running smoke.
The Tier-5 runner `scripts/tier5-smoke.sh` is the first consumer of the
**filesystem/telemetry monitor** pattern (no `herdr wait`); if that
pattern holds up, P7's failure mode has been made structurally
impossible for future smokes, and P7 becomes "closed by architectural
change" rather than "one-line wrapper fix."

If Tier 5 reveals that the smoke-26 pattern is brittle (the wrapper
itself dies on certain shell signals, etc.), P7 re-opens with a more
substantive fix.

— Evo, 2026-08-11
