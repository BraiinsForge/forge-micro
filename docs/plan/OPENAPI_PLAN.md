# OpenAPI Document Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the `jtr/UBOS-163/opan-api` branch on the open master so
that `ubos/ubos-api/openapi.json` describes every `/api/*` operation from
the Rust wire types, with every enumeration, capacity and index width
machine-readable.

**Architecture:** The wire types in `ubos-config` and `ubos-api/src/schema/`
derive `schemars::JsonSchema` behind a host-only `schema` feature. A host
test in `ubos-api` holds the route table, assembles the OpenAPI 3.1
document from the derives, and fails when the committed file differs.
Strings that stand for enumerations become wire enums owned by
`ubos-api/src/schema/`, converted from the domain enums by exhaustive
`From` impls, so the document lists their values and the domain crates
gain no serde dependency.

**Tech Stack:** Rust no_std crates, `serde`, `serde-json-core`, `schemars`
1.2 (`derive`, no default features), `serde_json` as a dev dependency,
nix flake checks.

**Spec:** the review in the session that wrote this plan, recorded here
as the five items under "What this plan changes". The old branch tip
`651c9800` in this repository is the reference implementation; commits
`ea61afa9` and `6b6f9731` in the reflog are its first two commits already
rebased onto the open master.

## What this plan changes

1. Enumerations are wire enums, not `&'static str`: chain state, mining
   state, pause reason, event severity, event argument width.
2. Schema descriptions address the API client: no intra-doc links, no
   Rust spelling, no pointers into `docs/`.
3. The route table reads each method and path from `Route`, and
   coverage is checked against `Route::ALL` instead of a hand count.
4. Heapless string capacities become `maxLength`; `usize` indexes become
   `u8` and the OTA chunk offset `u32`.
5. The events envelope is a struct, and `PATCH /api/system` is
   documented as the no-op it is.

## Global Constraints

- Every open commit carries the GPL-3.0-or-later block on each new
  `.rs` file (`ubos-main/CLAUDE.md`). Markdown and JSON carry none.
- `schemars` is optional everywhere and reachable only through the
  `schema` feature: `ubos-api/schema = ["dep:schemars", "ubos-config/schema"]`,
  `ubos-config/schema = ["dep:schemars"]`. No firmware target enables it.
- Wire strings keep their current bytes. `pause_reason`,
  `mining_state`, chain `state`, event `sev` and catalog `args` values
  must serialize exactly as `as_api_str` and the old handlers wrote them.
- The host rows in `nix/checks.nix` use `features = [ ... ]` and
  `noDefaultFeatures = true`; `args` and `artifacts` no longer exist.
- Commit messages: `<crate>: <Imperative summary>`, body bullets, no
  `Co-Authored-By` footer (`.gitlint`).
- The pair rule: every open head move is followed by amending the one
  pin commit on the avalon branch (`.claude/pair.md`).

## Review Focus

- A `POST /api/config` with a 129-byte pool host answers 400; the
  document must state `maxLength: 128` on `host` and `user`
  (Task 10 test `pool_entry_schema_states_the_capacity`).
- The dashboard reads `pause_reason` as a string union; the enum must
  serialize byte-identical to `PauseReason::as_api_str` (Task 5 test
  `pause_reason_serializes_as_before`).
- A regenerate inside the nix sandbox must not try to write: the
  `UBOS_OPENAPI_WRITE` switch is read only when set, and the sandbox
  never sets it (Task 12, `committed_document_is_current`).
- The document must be byte-stable across runs: `serde_json` without
  `preserve_order` sorts object keys, and `routes()` is the only
  ordered input (Task 12 test `document_is_deterministic`).
- A new `Route` variant without a table entry must fail a test, not
  ship undocumented (Task 11 `Route::ALL`, Task 12 test
  `every_route_variant_is_in_the_table`).

## Test commands

From the avalon root, with `$CRATE` a crate directory under `ubos-main/`:

```sh
# one host crate's tests, the feature set the check row names
nix develop -c bash -c "cd ubos-main/$CRATE && cargo test --no-default-features --features schema"
# ubos-config also needs std
nix develop -c bash -c 'cd ubos-main/ubos/ubos-config && cargo test --features std,schema'
# the gated form, one check at a time
nix build ./ubos-main#checks.x86_64-linux.test-ubos-api
nix build ./ubos-main#checks.x86_64-linux.clippy-ubos-api
# the pair gate at the end
nix run ./ubos-main#gate
```

Assumption: the default shell carries the host toolchain, as the
`avalon-*` host libraries build from it. If it does not, use the shell
`ubos-main/nix/` names for host work.

---

### Task 0: Branch both trees on the new base

**Files:**
- Modify: nothing yet; git only.

- [ ] **Step 1: Recreate the open branch on the open master**

```bash
cd ubos-main
git branch -f jtr/UBOS-163/opan-api-old 651c9800   # keep the old tip by name
git checkout -B jtr/UBOS-163/opan-api origin/master
git cherry-pick ea61afa9 6b6f9731                   # the two feature commits, already rebased
```

Expected: HEAD is two commits past `origin/master`, titled
`ubos-config: Add schemars behind an optional schema feature` and
`ubos-config: Derive JSON schemas behind the schema feature`.

- [ ] **Step 2: Cherry-pick the ubos-api feature commit**

```bash
git cherry-pick 793ef507
```

Expected: applies clean; `ubos/ubos-api/Cargo.toml` gains the `schema`
feature and the optional `schemars` dependency.

- [ ] **Step 3: Verify the three commits build**

Run: `nix develop -c bash -c 'cd ubos-main/ubos/ubos-config && cargo test --features std,schema'`
Expected: `config_update_schema_names_every_section` passes.

- [ ] **Step 4: Commit this plan**

```bash
git add docs/plan/OPENAPI_PLAN.md
git commit -m "docs: Plan the OpenAPI document rebuild"
```

---

### Task 1: Derive JSON schemas for the system responses

Re-applies branch commit `5adcb237` on the new base.

**Files:**
- Modify: `ubos/ubos-api/src/schema/{generic_response,system_info,system_network,system_scoreboard,system_statistics}.rs`
- Modify: `ubos/ubos-api/src/system.rs` (`NetworkPostRequest` derive and `pub(crate)`)
- Modify: `nix/checks.nix` rows `clippy-ubos-api` and `test-ubos-api`
- Test: `ubos/ubos-api/src/tests/system.rs`

**Interfaces:**
- Produces: every type in `schema/` and `NetworkPostRequest` implement
  `JsonSchema` under `feature = "schema"`.

- [ ] **Step 1: Cherry-pick and resolve**

```bash
git cherry-pick 5adcb237
```

Expected: conflict in `nix/checks.nix` only. Resolve to
`clippy-ubos-api = { crate = "ubos-api"; features = [ "schema" ]; tests = false; }`
and `test-ubos-api = { crate = "ubos-api"; noDefaultFeatures = true; features = [ "schema" ]; }`,
keeping the comment that explains the schema feature.

- [ ] **Step 2: Run the carried test**

Run: `nix develop -c bash -c 'cd ubos-main/ubos/ubos-api && cargo test --no-default-features --features schema system_info_schema_uses_camel_case'`
Expected: PASS.

- [ ] **Step 3: Continue the cherry-pick**

```bash
git add nix/checks.nix && git cherry-pick --continue
```

---

### Task 2: Derive JSON schemas for the /api/info body

Replaces branch commit `f38fea46`, whose struct master already has as
`schema/info.rs`.

**Files:**
- Modify: `ubos/ubos-api/src/schema/info.rs`
- Test: `ubos/ubos-api/src/tests/rest.rs`

**Interfaces:**
- Produces: `ApiInfo`, `ApiInfoChain`, `ApiInfoOta` implement
  `JsonSchema` under the feature. `OtaPhase`, `ResetReason`,
  `OtaWritePhase` in `ota.rs` gain the same derive.

- [ ] **Step 1: Write the failing test** in `tests/rest.rs`, under `#[cfg(feature = "schema")]`

```rust
#[test]
fn api_info_schema_names_the_ota_block() {
    let value = serde_json::to_value(schemars::schema_for!(ApiInfo)).expect("schema is JSON");
    assert!(value["properties"].as_object().expect("root is an object").contains_key("ota"));
    assert_eq!(value["$defs"]["OtaPhase"]["enum"], serde_json::json!(["idle", "uploading", "error", "rebooting"]));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `nix develop -c bash -c 'cd ubos-main/ubos/ubos-api && cargo test --no-default-features --features schema api_info_schema'`
Expected: FAIL, `JsonSchema` not implemented for `ApiInfo`.

- [ ] **Step 3: Add the derives**

`#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]` on the
three structs in `info.rs` and the three enums in `ota.rs`. Fields of
type `heapless::String<N>` or `Vec<_, N>` take
`#[cfg_attr(feature = "schema", schemars(with = "str"))]` or
`with = "&[ApiInfoChain]"` as the branch did.

- [ ] **Step 4: Run the test to verify it passes**, then **Step 5: Commit**

```bash
git commit -am "ubos-api: Derive JSON schemas for the /api/info body"
```

---

### Task 3: Serialize the chain state as an enum

**Files:**
- Create: `ubos/ubos-api/src/schema/state.rs`
- Modify: `ubos/ubos-api/src/schema/mod.rs`, `ubos/ubos-api/src/schema/info.rs:118-160`
- Test: `ubos/ubos-api/src/tests/rest.rs`

**Interfaces:**
- Produces: `pub enum ApiChainState { Unknown, Present, Absent, Faulted }`
  with `#[serde(rename_all = "camelCase")]`, `JsonSchema` under the
  feature, and `impl From<ChainPresence> for ApiChainState`.
  `ApiInfoChain::state: ApiChainState`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn chain_state_serializes_as_before() {
    let mut buf = [0u8; 16];
    for (presence, expected) in [
        (ChainPresence::Unknown, "\"unknown\""),
        (ChainPresence::Present { chips: 1 }, "\"present\""),
        (ChainPresence::Absent, "\"absent\""),
        (ChainPresence::Faulted { found: 0 }, "\"faulted\""),
    ] {
        let n = serde_json_core::to_slice(&ApiChainState::from(presence), &mut buf).expect("fits");
        assert_eq!(core::str::from_utf8(&buf[..n]).expect("utf8"), expected);
    }
}
```

Check the exact `Faulted` payload fields in
`ubos-metrics/src/chain/presence.rs` before writing the test.

- [ ] **Step 2: Run it to verify it fails** (type missing)
- [ ] **Step 3: Implement `state.rs`** with the enum, the `From` impl as an exhaustive match, and replace the string match in `ApiInfoChain::new` with `stats.presence().into()`.
- [ ] **Step 4: Run `cargo test --no-default-features --features schema` for the crate; all pass**
- [ ] **Step 5: Commit** `ubos-api: Serialize the chain state as an enum`

---

### Task 4: Serialize the mining state as an enum

**Files:**
- Modify: `ubos/ubos-api/src/schema/state.rs`, `schema/info.rs:84`, `schema/system_info.rs:189`, `rest.rs:216`, `system.rs:350-352`
- Test: `ubos/ubos-api/src/tests/rest.rs`

**Interfaces:**
- Produces: `pub enum ApiMiningState { WaitingForPool, Starting, Running, Stopped }`,
  camelCase, `From<ubos_mining::MiningState>`. Both info bodies hold
  `mining_state: Option<ApiMiningState>`.
- `MiningState::as_api_str` stays: `ubos-mining/src/publishers.rs:88`
  uses it as a metric label.

- [ ] **Step 1: Write the failing test** `mining_state_serializes_as_api_str`: for each of the four `MiningState` variants, `serde_json_core::to_slice(&ApiMiningState::from(state))` equals `format!("\"{}\"", state.as_api_str())`.
- [ ] **Step 2: Run it to verify it fails**
- [ ] **Step 3: Implement**, replacing the four `as_api_str` call sites in `ubos-api` with `.map(ApiMiningState::from)`.
- [ ] **Step 4: Run the crate tests; all pass**
- [ ] **Step 5: Commit** `ubos-api: Serialize the mining state as an enum`

---

### Task 5: Serialize the pause reason as an enum

**Files:**
- Modify: `ubos/ubos-api/src/schema/state.rs`, `schema/info.rs:89`, `schema/system_info.rs:196`, `rest.rs:220`, `system.rs:355-357`
- Modify: `ubos/ubos-manager/src/pause.rs:63-80`, `ubos/ubos-manager/src/tests/pause.rs:106`
- Modify: `ubos/ubos-webui/web/lib/types.ts:75,100` (comments only)
- Test: `ubos/ubos-api/src/tests/rest.rs`

**Interfaces:**
- Produces: `pub enum ApiPauseReason { DangerousTemperature, BrokenFan, UserRequest, OtaUpdate, TunerFault, TemperatureUnreadable }`,
  camelCase, `From<ubos_manager::PauseReason>`. Both info bodies hold
  `pause_reason: Option<ApiPauseReason>`.
- Removes: `PauseReason::as_api_str` and its test; `ubos-api` was its
  only caller.

- [ ] **Step 1: Write the failing test** `pause_reason_serializes_as_before`: the six reasons serialize to `"dangerousTemperature"`, `"brokenFan"`, `"userRequest"`, `"otaUpdate"`, `"tunerFault"`, `"temperatureUnreadable"`, listed as literals so the test outlives `as_api_str`.
- [ ] **Step 2: Run it to verify it fails**
- [ ] **Step 3: Implement**, then delete `as_api_str` from `ubos-manager` and its test, and point the two `types.ts` comments at `ubos-api/src/schema/state.rs`.
- [ ] **Step 4: Run tests for `ubos-api` and `ubos-manager`; all pass**
- [ ] **Step 5: Commit** as two commits: `ubos-api: Serialize the pause reason as an enum`, then `ubos-manager: Drop the wire string of PauseReason`.

---

### Task 6: Give the /api/info indexes a fixed width

**Files:**
- Modify: `ubos/ubos-api/src/schema/info.rs:118-160`
- Test: `ubos/ubos-api/src/tests/rest.rs`

**Interfaces:**
- Produces: `ApiInfoChain::chain: u8`, `ApiInfoChain::pool: Option<u8>`.
  `const _: () = assert!(MAX_CHAINS <= u8::MAX as usize);` beside the
  struct; the same for `ubos_config::POOL_SLOTS`, which bounds `pool`.

- [ ] **Step 1: Write the failing test** `api_info_chain_schema_uses_uint8`: `schema_for!(ApiInfoChain)` has `properties.chain.format == "uint8"` and `properties.pool.format == "uint8"`.
- [ ] **Step 2: Run it to verify it fails** (`format` is `uint`)
- [ ] **Step 3: Implement**: convert with `u8::try_from(chain).unwrap_or(u8::MAX)` and the const asserts that make the fallback unreachable.
- [ ] **Step 4: Run the crate tests; all pass**
- [ ] **Step 5: Commit** `ubos-api: Give the /api/info indexes a fixed width`

---

### Task 7: Serialize event records and the catalog from structs

Re-applies branch commit `cd1525f7` with the two string fields as enums
and the envelope as a struct.

**Files:**
- Create: `ubos/ubos-api/src/schema/events.rs` (from `git show 651c9800:ubos/ubos-api/src/schema/events.rs`)
- Modify: `ubos/ubos-api/src/schema/mod.rs`, `ubos/ubos-api/src/system.rs` (the two event handlers)
- Test: `ubos/ubos-api/src/tests/system.rs`

**Interfaces:**
- Produces: `EventRecordJson<'r>` with `sev: ApiSeverity`,
  `EventDefJson` with `sev: ApiSeverity` and `args: &'static [ArgWidth]`
  serialized through `ApiArgWidth`;
  `pub enum ApiSeverity { Info, Warn, Error }` lowercase,
  `From<u8>` (1 warn, 2 error, else info) and `From<ubos_events::Severity>`;
  `pub enum ApiArgWidth { U8, U16, U32, I8, I16, I32 }` lowercase,
  `From<ArgWidth>`; `pub struct EventsResponse<'r> { pub events: &'r [EventRecordJson<'r>] }`.
- The handlers still stream one record at a time; `EventsResponse`
  gives the envelope a schema and a test pins the framing.

- [ ] **Step 1: Write the failing tests**
  - `event_severity_serializes_lowercase`: the three variants serialize to `"info"`, `"warn"`, `"error"`.
  - `streamed_events_body_matches_the_envelope_struct`: drive `GET /api/system/events` through the existing mock connection with two records and assert the body equals `serde_json_core::to_slice(&EventsResponse { events: &[..] })`.
- [ ] **Step 2: Run them to verify they fail**
- [ ] **Step 3: Implement** `events.rs`; replace `severity_str` and `argwidth_str` with the enums and `serialize_with` on the slice field; switch the handlers to the structs.
- [ ] **Step 4: Run the crate tests; all pass**
- [ ] **Step 5: Commit** `ubos-api: Serialize event records and the catalog from structs`

---

### Task 8: Describe the ack bodies with structs

Re-applies branch commit `3c1547f5` with a fixed-width offset.

**Files:**
- Create: `ubos/ubos-api/src/schema/ack.rs` (from the old tip)
- Modify: `ubos/ubos-api/src/schema/mod.rs`
- Test: `ubos/ubos-api/src/tests/rest.rs`

**Interfaces:**
- Produces: `Ack { ok: bool }`, `OtaChunkAck { ok: bool, next: Option<u32> }`
  with `skip_serializing_if = "Option::is_none"` on `next`.

- [ ] **Step 1: Write the failing test** `ota_chunk_ack_schema_marks_next_optional`: `required == ["ok"]` and `properties.next.format == "uint32"`.
- [ ] **Step 2: Run it to verify it fails**
- [ ] **Step 3: Implement**; the handlers that write `{"ok":true}` literals keep them, the structs describe the bodies and the three carried tests from `3c1547f5` (`ack_matches_the_literal_body`, `ota_chunk_ack_matches_the_literal_body`, `ota_chunk_ack_without_next_matches_the_final_literal`) pin that.
- [ ] **Step 4: Run the crate tests; all pass**
- [ ] **Step 5: Commit** `ubos-api: Describe the ack bodies with structs`

---

### Task 9: Address the schema descriptions to the client

**Files:**
- Modify: `ubos/ubos-config/src/{lib,pool,api,network,cooling,thermal,miner,mining}.rs` doc comments on `JsonSchema` types
- Modify: `ubos/ubos-api/src/schema/*.rs` doc comments
- Test: `ubos/ubos-api/src/tests/openapi.rs` (added in Task 12; the test is written here and runs once that file exists)

**Interfaces:**
- Produces: no description in any derived schema contains "[`",
  "`None`", "`Some(", "Vec", "docs/" or "ubos-".

- [ ] **Step 1: List the offending comments**

Run: `git show 651c9800:ubos/ubos-api/openapi.json | grep -n -E '\[`|`None`|Some\(|Vec|docs/' `
Expected: the list of descriptions to rewrite, all under `ConfigUpdate`, `PoolEntry`, `CoolingUpdate` and the `/api/info` build field.

- [ ] **Step 2: Rewrite each** in the owning source file: say what the key means on the wire, what an absent key does, and the unit. Drop the pointer to `docs/MULTIPOOL_DESIGN.md`.
- [ ] **Step 3: Run `cargo doc`-free check**: `cargo test --features std,schema` for `ubos-config` and the `ubos-api` tests still pass.
- [ ] **Step 4: Commit** `ubos-config: ubos-api: Address the schema descriptions to the client`

---

### Task 10: State the string capacities in the schemas

**Files:**
- Modify: every `schemars(with = "str")` and `with = "Option<&str>"` site whose field is `heapless::String<N>`:
  `ubos-config/src/pool.rs:124,135,148,153`, `api.rs:40,43`, `network.rs:70`;
  `ubos-api/src/schema/system_info.rs` (`HwString`, four fields and `hashboard_ids`), `system_scoreboard.rs:48`, `system.rs` `NetworkPostRequest.hostname`
- Test: `ubos/ubos-config/src/pool.rs` `schema_tests`

**Interfaces:**
- Produces: each such field carries
  `#[cfg_attr(feature = "schema", schemars(with = "str", length(max = NAME_CAPACITY)))]`
  with the named constant the type uses; `HW_STRING_LEN` for the
  `HwString` fields.

- [ ] **Step 1: Write the failing test** `pool_entry_schema_states_the_capacity`: `schema_for!(PoolEntry)` has `properties.host.maxLength == 128` and `properties.user.maxLength == 128`.
- [ ] **Step 2: Run it to verify it fails**
- [ ] **Step 3: Implement**. If `schemars` 1.2 rejects a constant in `length(max = ...)`, use the literal with a one-line comment naming the constant, and say so in the commit body.
- [ ] **Step 4: Run tests for both crates; all pass**
- [ ] **Step 5: Commit** `ubos-config: ubos-api: State the string capacities in the schemas`

---

### Task 11: Name each route's method and path on Route

**Files:**
- Modify: `ubos/ubos-api/src/system.rs:1718-1792`
- Test: `ubos/ubos-api/src/tests/system.rs`

**Interfaces:**
- Produces: `impl Route { pub(crate) const fn endpoint(self) -> (&'static str, &'static str); pub(crate) const ALL: [Self; 24]; }`
  returning the method in upper case and the path, as an exhaustive
  match. `ALL` sits beside the enum, following `MiningState::ALL`.

- [ ] **Step 1: Write the failing test** `every_route_round_trips`: for each `r in Route::ALL`, `route(method, path) == DispatchOutcome::Handled(r)` with `(method, path) = r.endpoint()`, and `ALL` holds no duplicate.
- [ ] **Step 2: Run it to verify it fails**
- [ ] **Step 3: Implement** `endpoint` and `ALL`.
- [ ] **Step 4: Run the crate tests; all pass**
- [ ] **Step 5: Commit** `ubos-api: Name each route's method and path on Route`

---

### Task 12: Generate the OpenAPI document from the schema types

Re-applies branch commit `16536594` on the new types.

**Files:**
- Create: `ubos/ubos-api/src/tests/openapi.rs` (from `git show 651c9800:ubos/ubos-api/src/tests/openapi.rs`)
- Create: `ubos/ubos-api/openapi.json` (generated)
- Modify: `ubos/ubos-api/src/lib.rs` (module under `cfg(all(test, feature = "schema"))`), `ubos/ubos-api/Cargo.toml` (`serde_json` dev dependency), `nix/source.nix` (`openapi.json` pattern)
- Test: the file itself

**Interfaces:**
- Consumes: `Route::endpoint`, `Route::ALL` (Task 11); every schema type above.
- Produces: `routes() -> Vec<Route>` where system entries take
  `method` and `path` from `Route::endpoint`; `/api/info` and
  `/api/config` keep literal paths, as `rest.rs` serves them outside
  `route()`.
- The `PATCH /api/system` entry: summary
  `"No-op stub: acknowledges and applies nothing"`, `requestBody.required = false`,
  description `"Read and discarded."`.
- The events entry uses `schema::<EventsResponse>` instead of a
  hand-written envelope.

- [ ] **Step 1: Port the generator** and replace the three coverage tests with:
  - `every_route_variant_is_in_the_table`: the set of `(method, path)` from the table contains `r.endpoint()` for every `r in Route::ALL`.
  - `every_table_entry_is_served`: each `/api/system` entry round-trips through `route()`.
  - `document_is_deterministic`: `document()` rendered twice is equal.
  - `descriptions_address_the_client`: no `description` string anywhere in `document()` contains "[`", "`None`", "Some(", "Vec", "docs/".
  - `committed_document_is_current` as on the old tip.
- [ ] **Step 2: Generate**

Run: `nix develop -c bash -c 'cd ubos-main/ubos/ubos-api && UBOS_OPENAPI_WRITE=1 cargo test --no-default-features --features schema committed_document_is_current'`
Expected: `openapi.json` written.

- [ ] **Step 3: Inspect the result**

Run: `python3 -I -c "import json;d=json.load(open('ubos-main/ubos/ubos-api/openapi.json'));s=d['components']['schemas'];print(sorted(s));print(s['ApiPauseReason']['enum'],s['PoolEntry']['properties']['host'])"`
Expected: `ApiChainState`, `ApiMiningState`, `ApiPauseReason`, `ApiSeverity`, `ApiArgWidth`, `EventsResponse` present; the six pause reasons listed; `maxLength: 128` on host.

- [ ] **Step 4: Run the gated check**

Run: `nix build ./ubos-main#checks.x86_64-linux.test-ubos-api ./ubos-main#checks.x86_64-linux.clippy-ubos-api`
Expected: both build.

- [ ] **Step 5: Commit** `ubos-api: Generate the OpenAPI document from the schema types`, body from `16536594`.

---

### Task 13: Point the REST API reference at the OpenAPI document

Re-applies branch commit `651c9800`.

**Files:**
- Modify: `docs/REST_API.md`, `docs/NET_ADDRESS_OWNERSHIP.md`
- Delete: `docs/plan/OPENAPI_PLAN.md` (the work has landed)

- [ ] **Step 1: Cherry-pick** `651c9800`; resolve `REST_API.md` against master's current text, keeping master's server and crate sections and replacing the route tables with the pointer paragraph from the old tip.
- [ ] **Step 2: Delete this plan**, `git rm docs/plan/OPENAPI_PLAN.md`, in the same commit.
- [ ] **Step 3: Commit** `docs: Point the REST API reference at the OpenAPI document`

---

### Task 14: Rebase the avalon branch and re-record the pin

**Files:**
- Modify: `Cargo.toml` (avalon root), the `ubos-main` gitlink

- [ ] **Step 1: Rebase**

```bash
cd "$AVALON"
GIT_EDITOR=true git rebase origin/master
```

Expected: the pin commit `ubos-main: Pin the open half of this branch`
conflicts on the gitlink; resolve with
`git -C ubos-main checkout jtr/UBOS-163/opan-api && git add ubos-main && GIT_EDITOR=true git rebase --continue`.
The commit `cargo: Mirror the schemars workspace dependency` may
conflict in `Cargo.toml`; keep master's lines and add the `schemars`
line.

- [ ] **Step 2: Check the pin**

Run: `git submodule status`
Expected: no leading `+`; the SHA is the open branch tip.

- [ ] **Step 3: Run the pair gate**

Run: `nix run ./ubos-main#gate`
Expected: green except `pins`, which stays red until the open branch is
pushed and merged (`.claude/pair.md`).

- [ ] **Step 4: Print the push the user runs**

```bash
git -C ubos-main push --force-with-lease origin jtr/UBOS-163/opan-api
```

Never run it; the user pushes.
