# Plan for Unambiguously Better Agent Chat Retrieval

Status: initial year-scale plan
Date: 2026-05-20
Focal object: the "find, inspect, and resume the agent chat I mean" pathway across local agent histories
Phase: design and roadmap

## Thesis

The product should stop treating indexed hybrid search as the default answer to every agent-chat retrieval problem.

There are two different products tangled together today:

1. A zero-friction retrieval tool for "find the chat that said this exact thing, right now."
2. A heavyweight archive product with SQLite, lexical indexes, semantic vectors, reranking, analytics, remote sync, TUI, packs, and enrichment.

The first product must be tiny, fresh, bounded, explainable, and impossible to block on derived state. The second product can exist, but only as optional enrichment. If the archive layer is busy, stale, rebuilding, corrupted, or running a semantic backfill, exact chat retrieval must still work.

The target is not "CASS, but faster." The target is a source-log-first retrieval system with an enrichment layer bolted on carefully enough that it can never make the common case worse.

## Non-negotiable properties

These are release blockers for the replacement path.

| Property | Requirement | Why it matters |
| --- | --- | --- |
| Foreground freshness | Exact source-log search must see provider files without indexing first. | Agent chats are live artifacts. Waiting on an index is unacceptable. |
| CPU boundedness | Default foreground search uses at most one core and exits quickly. Background jobs are off by default or explicitly budgeted. | The old watcher burning CPU is the failure mode to design against. |
| No index dependency | `find/resume` commands do not open the cass DB, Tantivy index, vector store, reranker, or daemon. | No `index-busy`, no stale-index friction, no derived-state coupling. |
| Provider breadth | Codex, Claude, Gemini, Kimi, Hermes, Pi-Agent, Cursor, and user-defined local providers are first-class. | The use case is "my agent chats", not "one provider's rows." |
| Provenance | Every result carries exact file path, line/event pointer, provider, role, timestamp if available, and parser confidence. | Agents need to resume, cite, inspect, and debug the search itself. |
| Retry intelligence | Empty or partial results return next searches to try, not a dead end. | The system should advocate for the operator instead of silently failing. |
| Reversibility | No default persistent watcher, no source mutation, no hidden deletion, no irreversible repairs. | Search should never put the machine or evidence at risk. |
| Testability | Real-world search cases become fixtures, benchmarks, and regression tests. | "Feels better" is not enough; prove it on past uses. |

## Current failure analysis

The recent live grep patch proved the shape of the better path:

- Exact current-session recovery found the target in tens of milliseconds.
- Recent instruction search found the right session in tens of milliseconds.
- Old prompt archaeology worked with oldest-first scan in a few seconds.
- Indexed `cass search` hit `index-busy` after about 30 seconds for the same task.
- A LaunchAgent watcher hardcoded to the Homebrew binary could run at nearly one full CPU core.

That is not a marginal perf bug. It is an architectural mismatch. A system that needs source-log lookup should not enter the enrichment machinery unless the user asked for enrichment.

## Property sweep

### Performance and cost

Wanted property:
Foreground retrieval is cheap under realistic local archives.

Current evidence:
`cass grep` can find exact phrases without the index. The binary still carries the full dependency graph and the known provider path list is narrow.

Main failure mode:
The CLI grows into another "ball of grass": every command links, initializes, or touches heavyweight subsystems.

Severity and horizon:
High, immediate. CPU friction destroys trust quickly.

Concrete improvement:
Split the retrieval path into a minimal module and eventually a minimal binary feature set. Make the query planner prove which expensive subsystems were not touched and include that in `_meta`.

### Reliability and failure containment

Wanted property:
Index problems, watcher problems, vector problems, and DB locks cannot break exact chat recovery.

Current evidence:
`cass grep` bypasses derived assets. Existing docs and habits still point agents toward `cass search`.

Main failure mode:
Operators and skills keep using indexed search first, hit `index-busy`, then waste time on repair instead of finding the chat.

Severity and horizon:
High, immediate.

Concrete improvement:
Make `cass find` the first-class direct retrieval command. Teach `cass search` to route exact/fresh/recovery-shaped queries through `find` before touching the index. Keep explicit archive search available as `cass archive search` or `cass search --indexed`.

### State integrity

Wanted property:
Source logs are canonical; derived catalogs are disposable; no background state can become a hidden source of truth.

Current evidence:
The indexed product already says derived assets can be rebuilt, but the user experience contradicts that when search blocks on derived state.

Main failure mode:
Sidecar catalogs drift and become another stale index.

Severity and horizon:
Medium to high over the first quarter.

Concrete improvement:
Build the direct scanner to work from source files alone. Add an optional append-only metadata catalog only as a speed hint, with mandatory fallback to source traversal and explicit freshness fields.

### Provenance and auditability

Wanted property:
Every hit explains what was scanned, what was skipped, and why it ranked.

Current evidence:
`grep` returns paths, snippets, roles, and scan counters. It does not yet expose provider parser confidence, session IDs, resume hints, or skipped reason buckets.

Main failure mode:
The user sees a plausible result but cannot tell whether other likely locations were missed.

Severity and horizon:
High in multi-provider use.

Concrete improvement:
Return `coverage[]`, `provider_status[]`, `skipped_reasons`, `query_plan`, and `retry_suggestions[]` in robot output.

### UX and recoverability

Wanted property:
The command shape matches the user's actual intent: find the chat, inspect it, resume it.

Current evidence:
`cass grep` works but sounds like a raw primitive. `cass search` still sounds like the obvious command.

Main failure mode:
Users and agents reach for the wrong surface.

Severity and horizon:
High, immediate.

Concrete improvement:
Add `cass find`, `cass inspect`, and `cass resume-hints`. Keep `grep` as a low-level alias. Error and empty-result outputs must include concrete next commands.

### Interoperability and composition

Wanted property:
Provider support is declarative where possible and parser-backed where needed.

Current evidence:
Known roots include Codex, Claude, Pi-Agent, Gemini, Cursor. Existing indexed connectors cover more providers.

Main failure mode:
Direct search and indexed search support different provider sets, causing confusing misses.

Severity and horizon:
High over the first two quarters.

Concrete improvement:
Create a single provider manifest interface consumed by direct search, indexed ingestion, docs, and tests. Provider manifests define roots, date layouts, file globs, role extraction, timestamp extraction, session identity, workspace extraction, and resume hints.

### Security and privacy

Wanted property:
Search never leaks secrets, never mutates provider logs, and makes sensitive output policy explicit.

Current evidence:
Direct grep outputs raw snippets. Existing pack path has redaction concepts.

Main failure mode:
A broad direct query returns secrets from tool output or hidden provider cache files.

Severity and horizon:
Medium now, high before wider release.

Concrete improvement:
Add redaction profiles to direct search robot output, with `--raw` as an explicit opt-in. Default role filters and snippets should avoid huge tool blobs unless requested.

### Governance and maintainership

Wanted property:
This becomes an obvious product doctrine, not a one-off optimization.

Current evidence:
The current codebase has extensive search/index docs that still promote the archive-first mental model.

Main failure mode:
Future agents re-add background watchers, semantic defaults, or index-first routing because the doctrine is not encoded.

Severity and horizon:
High over the year.

Concrete improvement:
Add a "source-log-first retrieval" contract to docs, tests, CLI help, triage recommendations, and skill docs. CI should fail if `find` starts depending on index modules or semantic crates.

## Product shape

### Commands

| Command | Purpose | Dependency budget |
| --- | --- | --- |
| `cass find <query>` | Default direct source-log search for agent chats. | Filesystem only; no DB/index/vector/daemon. |
| `cass grep <query>` | Low-level exact/regex scan, kept as power-user alias. | Filesystem only. |
| `cass inspect <session> --around <line>` | Show contextual transcript from source logs. | Filesystem only. |
| `cass resume-hints <session>` | Emit provider-specific resume/open commands and session metadata. | Filesystem plus provider parser. |
| `cass providers list --json` | Show detected providers, roots, coverage, and parser health. | Filesystem only by default. |
| `cass archive search <query>` | Indexed lexical/semantic/archive search. | DB/index/vector as requested. |
| `cass enrich ...` | Explicit indexing, semantic, rerank, analytics, remote sync, and packs. | Heavy operations, never implicit. |

`cass search` should eventually become an intent router:

- exact phrase, fresh time filter, role filter, or "resume/find chat" wording -> direct `find`;
- aggregations, broad fuzzy ranking, answer packs, analytics, or explicit semantic flags -> archive path;
- index busy -> direct fallback if the query can be served from source logs.

### Query planner

The planner must produce a machine-readable plan before work starts:

```json
{
  "intent": "direct_find",
  "reason": "exact_phrase_with_today_filter",
  "will_touch": ["source_files"],
  "will_not_touch": ["sqlite", "tantivy", "semantic_vectors", "reranker", "daemon"],
  "provider_scope": ["codex"],
  "time_scope": "today",
  "budget": {
    "timeout_ms": 3000,
    "max_files": 2000,
    "max_bytes_per_file": 4194304,
    "threads": 1
  }
}
```

This is not decorative. It is the contract that prevents accidental heaviness.

### Result contract

Direct results should have stable robot fields:

```json
{
  "query": "...",
  "intent": "direct_find",
  "sessions": [],
  "hits": [],
  "coverage": [],
  "retry_suggestions": [],
  "_meta": {
    "elapsed_ms": 57,
    "candidate_files": 44,
    "scanned_files": 40,
    "scanned_bytes": 1234567,
    "skipped_reasons": {},
    "touched_subsystems": ["source_files"],
    "did_not_touch_subsystems": ["sqlite", "tantivy", "semantic", "reranker"],
    "timed_out": false
  }
}
```

### Ranking

Direct ranking should be simple and inspectable:

1. Exact phrase in user prompt.
2. Exact phrase in assistant answer.
3. Exact phrase in tool command or output.
4. All terms present in a message.
5. Nearby terms within a session.
6. Workspace match.
7. Time preference: newest by default, oldest when requested, with explicit retry hints.
8. Provider confidence and parser confidence.

No neural reranker in the foreground path. If rerank is useful, it runs after direct candidates are found and only when explicitly requested.

## Architecture

### Layer 1: Provider manifests

Provider manifests should be the shared source of truth for direct search and indexed ingestion.

A manifest defines:

- provider slug and aliases;
- default roots and env overrides;
- file globs;
- known date directory layouts;
- session ID extraction;
- timestamp extraction;
- role extraction;
- workspace extraction;
- message text extraction;
- compacted/history/noise detection;
- resume/open hints;
- privacy risk flags.

First-class provider targets:

| Provider | Direct path target | Notes |
| --- | --- | --- |
| Codex | `~/.codex/tabs`, `~/.codex/sessions` | Current strongest path. Needs session ID and resume hint extraction. |
| Claude Code | `~/.claude/projects` | Needs robust JSONL parser and project/workspace mapping. |
| Gemini CLI | `~/.gemini/tmp` and known checkpoints/logs | Needs checkpoint and chat-log parsing. |
| Kimi Code | `~/.kimi/sessions/.../wire.jsonl` | Add direct manifest and fixtures. |
| Hermes/conscientious-agent | Hermes session/log locations plus fork-specific roots | Needs local operator-specific discovery. |
| Pi-Agent | `~/.pi/agent/sessions` | Current root exists; parse typed events. |
| Cursor | Cursor workspace storage and chat/history DB/cache files | Need careful privacy and binary/SQLite handling. |
| Amp/Cline/Roo/OpenCode/Qwen/Factory/Aider/Copilot | Existing indexed connector knowledge should be translated into direct manifests. | Direct parity prevents confusing misses. |
| User-defined local providers | TOML manifest under config | Handles "Oh My Pie" or any local harness without code changes. |

### Layer 2: Candidate planning

Candidate collection should be strategy-based:

- dated directory jump for known date layouts;
- mtime bounded scan for recent windows;
- workspace bounded scan when `--workspace` is present;
- provider manifest glob scan;
- optional metadata catalog hint;
- fallback WalkDir with hard caps.

The planner should never scan "everything" without saying so in `_meta` and emitting a better retry if the result is partial.

### Layer 3: Parsing and matching

Do not match only raw lines forever. Use a staged parser:

1. Fast raw byte/literal match to shortlist lines or files.
2. Provider-specific event parse for role, timestamp, message text, and session ID.
3. Transcript context hydration only for selected sessions.

For exact matching, use cheap primitives:

- `memchr`/substring for single phrase;
- `aho-corasick` for multiple terms;
- regex only when requested;
- no semantic embeddings in the direct path.

### Layer 4: Resume and inspect

The real workflow ends when the operator can act.

Direct results should support:

- print session path;
- print a stable session identity if provider exposes one;
- show surrounding transcript;
- copy a provider-specific resume command when possible;
- open in editor at line;
- emit a small answer pack from source logs without archive search.

### Layer 5: Optional catalog

Add a tiny source-log catalog only after the source-only path is correct.

Rules:

- It is a hint, not authority.
- It is append-only or rebuildable.
- It records file path, mtime, size, provider, session ID, workspace, first/last timestamp, message count, and content fingerprints.
- It can be ignored with `--no-catalog`.
- It cannot make source-only search fail.

### Layer 6: Archive enrichment

Move heavy functions behind explicit commands and budget gates:

- indexing;
- semantic embeddings;
- vector search;
- reranking;
- remote sync;
- TUI warm workers;
- answer-pack ranking over indexed corpora;
- analytics.

No KeepAlive watcher by default. If a watcher exists:

- it must be opt-in;
- it must run the current binary path;
- it must use `nice`/idle scheduling where available;
- it must have CPU and memory budgets;
- it must expose heartbeat and last work;
- it must stop or back off under thermal, battery, or user-active pressure;
- `cass doctor` must show the exact LaunchAgent/systemd unit and disable command.

## Performance SLOs

| Scenario | Target |
| --- | --- |
| Current-session exact phrase, one provider, today | p50 < 100 ms, p95 < 250 ms |
| Last 7 days exact phrase, all providers | p50 < 500 ms, p95 < 1500 ms |
| All-time exact phrase, newest first, 10k session files | first useful result < 3 s or partial with retry |
| Old prompt archaeology, oldest first, 10k session files | first useful result < 5 s |
| Empty query or invalid regex | fail before scanning |
| CPU foreground | default <= 1 core |
| Memory foreground | p95 RSS delta < 100 MB for direct search |
| Index busy | direct search unaffected; archive search reports clear fallback |
| Provider missing | visible coverage warning, not silent miss |

## Real-world bakeoff suite

Build a local redacted corpus and a test runner around actual operator-style cases:

1. Find the current Codex chat by a long exact user quote.
2. Find a prompt from yesterday by partial phrase.
3. Find an old "moredakka" request with oldest-first retry.
4. Find a Claude Code session by tool command.
5. Find a Gemini prompt in checkpoint/chat-log files.
6. Find a Kimi Code conversation by user wording.
7. Find a Hermes/conscientious-agent handoff by AGENTS.md text.
8. Find a session by workspace path plus vague phrase.
9. Find a session by assistant output phrase.
10. Find a session by shell command in tool output.
11. Confirm compacted Codex blobs are skipped by default.
12. Confirm compacted blobs can be included deliberately.
13. Confirm a broad all-time query returns partial metadata before timeout.
14. Confirm provider coverage reports roots that do not exist.
15. Confirm direct search works while archive index is locked.
16. Confirm direct search works with no cass data dir at all.
17. Confirm direct search does not spawn background workers.
18. Confirm direct search does not open semantic assets.
19. Confirm launchd/systemd watcher diagnostics identify stale binaries.
20. Confirm empty result includes retry suggestions that find the target.

Every case should run against:

- source-only mode;
- catalog-hinted mode;
- archive search where applicable;
- old upstream/Homebrew baseline where safe.

The release claim is not "faster in theory." The release claim is "wins these cases without hidden CPU or index friction."

## Year roadmap

### Month 1: Stop the bleeding

Deliverables:

- Keep `cass grep` stable and documented as the agent-chat recovery path.
- Add `cass find` as the product-facing alias or replacement.
- Update CLI help, README, robot docs, capabilities, and local skills.
- Add query-plan metadata proving no heavy subsystem was touched.
- Add tests that fail if direct search opens the cass DB, index, semantic assets, or daemon.
- Add `cass doctor watchers --json` or equivalent diagnostics for LaunchAgents/systemd units that run old binaries.

Exit criteria:

- Exact current-session recovery is boring.
- No default docs steer agents to indexed search for exact/fresh chat recovery.
- Old watcher problems are visible and reversible.

### Month 2: Provider manifest foundation

Deliverables:

- Introduce provider manifest structs.
- Convert Codex and Claude direct discovery to manifests.
- Add provider coverage output.
- Add parser confidence and skipped reason buckets.
- Add direct fixtures for Codex and Claude.

Exit criteria:

- Direct search and indexed ingestion can share provider facts.
- Missing provider roots are visible in robot output.

### Month 3: High-value provider expansion

Deliverables:

- Add Gemini, Kimi, Hermes, Pi-Agent, and Cursor manifests.
- Add minimal provider-specific role/timestamp/session parsing.
- Add `cass providers list --json`.
- Add user-defined TOML provider manifests.

Exit criteria:

- The system can credibly say it searches the operator's agent chats, not just Codex.

### Month 4: Inspect and resume workflows

Deliverables:

- Add `cass inspect` over source logs.
- Add `cass resume-hints`.
- Add source-log answer pack for selected direct sessions.
- Add editor/open commands.

Exit criteria:

- A successful search naturally ends in resume/open/inspect, not manual path archaeology.

### Month 5: Ranking and retry intelligence

Deliverables:

- Implement transparent direct ranking.
- Add retry suggestions for empty and partial results.
- Add automatic newest/oldest retry hints.
- Add workspace-aware and role-aware scoring.

Exit criteria:

- Empty results are actionable.
- The system behaves like an advocate for finding the chat.

### Month 6: Performance harness and CPU guarantees

Deliverables:

- Build the real-world bakeoff harness.
- Add CPU, memory, elapsed-time, scanned-file, and scanned-byte assertions.
- Add source-only versus indexed baseline comparison.
- Add regression artifacts under docs/artifacts.

Exit criteria:

- Performance claims are attached to commands and artifacts, not anecdotes.

### Month 7: Catalog hints without authority drift

Deliverables:

- Add optional source catalog.
- Add invalidation by mtime/size/fingerprint.
- Add `--no-catalog` and source-only fallback.
- Add catalog correctness tests against direct traversal.

Exit criteria:

- The catalog improves latency without becoming another stale index trap.

### Month 8: Archive layer isolation

Deliverables:

- Move heavy archive behaviors behind explicit command names or explicit flags.
- Make `cass search` an intent router or split it into `find` and `archive search`.
- Ensure archive busy states return direct fallback suggestions.
- Remove or disable automatic watcher assumptions from install docs.

Exit criteria:

- A user cannot accidentally invoke semantic/index machinery for direct chat lookup.

### Month 9: Binary and feature diet

Deliverables:

- Audit startup and linking costs.
- Feature-gate semantic/ONNX/rerank/TUI where practical.
- Consider a minimal `cass-find` binary or lightweight build profile if one binary cannot stay clean.
- Add CI checks for direct path dependency creep.

Exit criteria:

- The direct retrieval tool no longer feels like it is carrying the whole archive product on its back.

### Month 10: Remote and multi-machine direct search

Deliverables:

- Direct source-log search over configured remote snapshots.
- Provenance for machine/source.
- Explicit sync freshness.
- No implicit SSH or remote mutation in direct search.

Exit criteria:

- Multi-machine recall works without making local search heavier.

### Month 11: Privacy, redaction, and policy hardening

Deliverables:

- Direct-search redaction profiles.
- Secret-pattern warnings.
- Raw-output opt-in.
- Provider-specific privacy notes.
- Golden robot schemas for direct results.

Exit criteria:

- Direct search is safe enough for routine agent use and handoff contexts.

### Month 12: Replacement decision and migration

Deliverables:

- Run a full bakeoff against old CASS usage patterns.
- Decide whether `cass search` should default to direct routing for most agent tasks.
- Update all skills, docs, examples, install defaults, and troubleshooting.
- Publish a migration note explaining what changed and why.

Exit criteria:

- We can honestly say the new path is unambiguously better for agent-chat retrieval, and the remaining archive product is clearly optional enrichment.

## Work breakdown

### Track A: Direct retrieval core

- Add `cass find`.
- Convert `grep` internals into reusable direct retrieval engine.
- Add query-plan metadata.
- Add touched-subsystem guards.
- Add scanned-byte accounting.
- Add skip reason accounting.
- Add result ranking.
- Add retry suggestions.

### Track B: Provider coverage

- Build manifest interface.
- Convert Codex direct support.
- Convert Claude direct support.
- Add Gemini.
- Add Kimi.
- Add Hermes/conscientious-agent.
- Add Pi-Agent.
- Add Cursor.
- Add user-defined provider TOML.

### Track C: Workflow completion

- Add inspect context.
- Add resume hints.
- Add source-log handoff packs.
- Add editor/open integration.
- Add session path copy modes and robot formats.

### Track D: Operations and CPU control

- Add watcher diagnostics.
- Disable default background watchers in docs/install.
- Add stale binary detection for launchd/systemd.
- Add CPU/memory budget metadata.
- Add background job policy docs.
- Add doctor recommendations that are reversible and non-destructive.

### Track E: Tests and evidence

- Build redacted real-world corpus.
- Add fixture generators.
- Add differential tests against direct `rg`.
- Add property tests for parser stability.
- Add perf SLO tests.
- Add end-to-end bakeoff runner.
- Add docs artifacts for each release claim.

### Track F: Archive isolation

- Make archive search explicit.
- Route exact/fresh queries to direct search.
- Keep semantic/rerank opt-in.
- Add direct fallback on index busy.
- Feature-gate heavy dependencies where practical.

## Design tensions

### Direct scan versus catalog

Direct scan is simpler and more trustworthy. Catalog hints are faster at scale but risk becoming another stale index. The resolution is strict: source-only must always work, catalog must be optional, and catalog output must carry freshness proof.

### Broad provider support versus parser quality

Raw search can cover many providers quickly, but good resume and role filtering require parsers. The resolution is staged provider confidence: raw path first, parsed metadata next, resume hints last.

### Rich ranking versus inspectability

Neural rerankers can improve fuzzy recall but damage latency and explainability. The resolution is no neural ranking in the direct foreground path. Use transparent heuristics first, optional enrichment later.

### One binary versus lightweight feel

One binary is convenient, but heavy dependencies create build/startup and trust costs. The resolution is first to prevent runtime touches, then feature-gate, then split binaries only if evidence says the single binary still feels wrong.

## Definition of "unambiguously better"

This work is not done until all of these are true:

- It finds exact current chats without an index.
- It finds old prompt archaeology with clear scan order and retries.
- It searches Codex, Claude, Gemini, Kimi, Hermes, Pi-Agent, Cursor, and user-defined providers.
- It never burns a CPU core in the background unless the operator explicitly opted in.
- It can run while the archive index is busy, stale, missing, or corrupt.
- It explains coverage and partial failures.
- It leads to inspect/resume actions.
- It has a real-world bakeoff suite proving past CASS use cases.
- Skills and docs teach agents to use the low-friction path first.
- The archive layer is still valuable, but it is no longer in the way.

## Immediate next implementation moves

- [x] Add `cass find` as the product-facing command backed by the current live grep engine.
  - Implemented 2026-05-20: `find` is a first-class direct CLI command; `grep` remains the low-level direct scanner.
- [x] Add direct query-plan metadata and touched-subsystem assertions.
  - Implemented 2026-05-20: direct JSON output now reports `_meta.intent`, `_meta.query_plan`, `_meta.touched_subsystems`, and `_meta.did_not_touch_subsystems`.
- [ ] Add `providers list --json` for direct search coverage.
- [ ] Add provider manifests for Codex and Claude.
- [ ] Add a bakeoff fixture for the exact sessions already tested in this work.
- [ ] Add watcher diagnostics for `com.xyra.cass-index-watch`-style stale binaries.
- [ ] Teach `cass search` to fall back to direct find on `index-busy` for exact/fresh queries.

These moves turn the current improvement from a useful patch into the start of the replacement product.
