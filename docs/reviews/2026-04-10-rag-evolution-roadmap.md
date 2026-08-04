# Akashic Record — RAG Evolution Roadmap

Date: 2026-04-10

## Purpose

This document defines the **next-stage evolution roadmap** for Akashic Record's RAG system.

It is based on the current implementation, which already has:

- hybrid retrieval
- multi-space retrieval (`Code`, `Doc`, `Human`)
- query-aware weighting
- RRF fusion
- graph expansion via Neo4j
- AST-aware chunking
- pgvector + BM25 indexes

The goal is **not** to replace the current direction.

The goal is to make the current system:

1. more precise
2. more measurable
3. more stable in production
4. more adaptable as corpus size grows

---

## Executive Direction

## Core conclusion

**Do not rewrite the RAG architecture.**

The current direction is already stronger than a typical vector-only system. The next stage should be:

> **calibrate → measure → rerank → specialize → optimize**

Not:

> **throw away the current GraphRAG design and start over**

---

## Current Strengths To Preserve

These are strategic assets and should remain part of the design:

### 1. Multi-space retrieval

The separation of retrieval across:

- `Code`
- `Doc`
- `Human`

is the correct product decision.

This avoids flattening all knowledge into a single noisy vector space.

### 2. Hybrid retrieval

The combination of:

- BM25 / full-text
- vector similarity
- graph expansion

is the right baseline for developer knowledge retrieval.

### 3. Query-aware routing / weighting

The current `query_analyzer` that shifts low-level vs high-level weighting is a very good direction.

This is especially useful for distinguishing:

- symbol lookup queries
- conceptual / architecture queries
- note-oriented memory queries

### 4. Graph augmentation

Neo4j-based expansion from seed nodes is valuable and should remain.

It gives the system a way to retrieve:

- neighbors
- explanations
- attached notes
- module/chunk relationships
- flow context

that pure vector search cannot express reliably.

### 5. AST-aware chunking

Language-aware chunking is materially better than naive fixed-size splitting and is already a competitive advantage.

---

## Main Gaps To Solve Next

The system is already architecturally good, but it has several likely next-stage bottlenecks.

## Gap 1 — Ranking quality ceiling

Right now, the system does strong retrieval and strong fusion, but it still depends mostly on:

- BM25 rank
- vector rank
- graph-derived score shaping

That creates a **quality ceiling**.

At some point, the top retrieved set will still contain “reasonable but not best” candidates.

### Needed next step

Add a **reranking layer**.

This is the highest-ROI next upgrade.

---

## Gap 2 — Lack of evaluation discipline

The current system has thoughtful heuristics and verification tests, but it still needs a more explicit retrieval quality loop.

Without measurement, future changes will drift into:

- complexity without clear gain
- regressions in recall / precision
- over-tuning for anecdotal examples

### Needed next step

Add a lightweight but explicit **RAG evaluation framework**.

---

## Gap 3 — Embedding strategy is still generalized

Current embedding support is pragmatic and good:

- local MiniLM via Candle
- remote OpenAI-compatible provider

But code retrieval quality will eventually be limited by using mostly general-purpose embedding models.

### Needed next step

Move toward **space-aware model strategy**, especially for `Code`.

---

## Gap 4 — Graph expansion may become noisy at scale

Graph expansion is currently a strength, but it is also a likely future precision leak.

As the corpus grows, hop expansion can bring in:

- structurally connected but semantically irrelevant nodes
- overly generic modules
- low-signal attached notes

### Needed next step

Make graph expansion more selective and more measurable.

---

## Gap 5 — Retrieval units are good, but not fully layered yet

You already have:

- chunks
- large chunks
- modules
- sections
- notes

This is strong.

But the next stage should tighten the distinction between:

- retrieval units
- explanation units
- final prompt assembly units

---

## Roadmap Principles

Before listing phases, these principles should guide all RAG work:

### Principle 1 — Optimize precision first

For your product, users care more about:

- “this result is actually relevant”

than:

- “we retrieved many plausible results”

So the roadmap should prioritize **precision@top-k**, not just recall.

### Principle 2 — Don’t let graph complexity outrun evidence

Graph expansion is powerful, but every extra hop adds noise risk.

Expand only where measurement shows real benefit.

### Principle 3 — Don’t overfit to one query class

Your system serves multiple query modes:

- exact code symbol lookup
- architecture exploration
- documentation lookup
- human memory / historical rationale

The roadmap should keep these modes explicit.

### Principle 4 — Prefer calibration over reinvention

Most next-stage improvements should refine:

- ranking
- weighting
- evaluation
- retrieval granularity

not replace the whole stack.

---

## Phase 1 — Precision Upgrade

## Goal

Increase top-result quality without changing the current architecture.

## Work items

### 1. Add reranking after retrieval/fusion

Current shape:

- retrieve seeds
- graph expand
- rank and prune
- assemble context

Target shape:

- retrieve candidates
- graph expand
- produce top-N candidates
- **rerank top-N**
- prune to token budget
- assemble context

### Why

This will likely produce the biggest quality jump with the least architectural disruption.

### Suggested first implementation

- rerank top 20–40 candidates
- keep top 5–12 for final assembly
- start with a single reranker applied across all spaces

### 2. Tune graph expansion scoring

Current scoring adds:

- inherited parent score
- hop bonus
- space weighting

Next step:

- log which expanded nodes survive final pruning
- measure whether hop-1 and hop-2 nodes actually improve final result quality
- reduce default weight for noisy relationship types if needed

### 3. Add query-class evaluation buckets

At minimum, evaluate separately for:

- symbol lookup
- architecture questions
- documentation lookup
- note/rationale lookup

## Success criteria

- measurable improvement in top-5 relevance
- fewer “almost right but not quite” retrievals
- no major latency explosion

---

## Phase 2 — Measurement And Evaluation Layer

## Goal

Make retrieval quality observable and safe to evolve.

## Work items

### 1. Build a gold query set

Create a versioned internal evaluation dataset with queries like:

- exact symbol lookups
- “how does X work” architecture questions
- “where is Y implemented” questions
- note/history/rationale queries
- doc lookup queries

For each query, define expected good results such as:

- target chunk(s)
- relevant module(s)
- relevant section(s)
- relevant note(s)

### 2. Track retrieval metrics

Recommended initial metrics:

- precision@5
- precision@10
- recall@10
- MRR / reciprocal rank of best target
- per-space hit rate

### 3. Track system behavior metrics

Add observability for:

- query class chosen by analyzer
- weights used
- candidate counts by space
- graph expansion counts
- latency per stage
- final context token usage

### 4. Add regression gates

Before changing retrieval heuristics, compare against the gold query set.

Do not ship ranking changes blindly.

## Success criteria

- retrieval changes become measurable
- ranking regressions are caught early
- query analyzer decisions become inspectable

---

## Phase 3 — Embedding Strategy Specialization

## Goal

Improve semantic quality by choosing models more deliberately.

## Current state

- Local: `all-MiniLM-L6-v2` (384 dim)
- Remote: OpenAI-compatible embeddings

This is good for flexibility, but not yet optimal for all spaces.

## Work items

### 1. Separate embedding strategy by space

Possible direction:

- `Code` → code-aware or stronger general retrieval model
- `Doc` → strong general semantic model
- `Human` → same general model may remain sufficient

This does **not** have to be implemented as three different live providers immediately.

A practical first step is:

- benchmark current single-model approach
- benchmark one stronger code-oriented option for code space

### 2. Establish model evaluation policy

For every candidate model, compare:

- retrieval quality
- latency
- memory / CPU cost
- indexing cost
- compatibility with self-hosted mode

### 3. Keep local-first path viable

Do not lose the current self-hosted / privacy-preserving strength.

Even if a stronger hosted model is introduced, preserve:

- a local baseline mode
- clear dimensional compatibility checks
- migration plan for re-embedding

## Success criteria

- improved code-space semantic retrieval
- explicit tradeoff table between local and hosted options
- no hidden schema/model mismatch surprises

---

## Phase 4 — Retrieval Unit Refinement

## Goal

Make the system better at retrieving small precise evidence while still assembling rich context.

## Work items

### 1. Formalize parent-child retrieval strategy

Current system already has several granularities. The next step is to make their roles explicit:

- **small chunk** = best unit for precise retrieval
- **module / large chunk / section** = best unit for context expansion
- **note** = best unit for human rationale/context

### 2. Reduce over-truncation risk

Right now large content can be truncated during chunk processing.

That is acceptable operationally, but long term the system should avoid losing high-value context that ought to remain available in a parent unit.

### 3. Improve context assembly policy

Current XML-style assembly is fine, but the next stage should ensure:

- less duplicated content
- better diversity across spaces
- more intentional inclusion of rationale notes only when helpful

## Success criteria

- better evidence granularity
- fewer redundant context blocks
- improved answer grounding quality

---

## Phase 5 — GraphRAG Calibration At Scale

## Goal

Keep graph expansion useful as data grows.

## Work items

### 1. Evaluate relationship types separately

Not all graph edges are equally useful.

Measure the retrieval value of edges such as:

- `HAS_CHUNK`
- `ATTACHED_TO`
- `EXPLAINS`
- flow-related edges
- import/call topology

### 2. Add selective expansion policies

Examples:

- allow hop-2 only for certain query classes
- limit expansion by edge type
- cap neighbors per relationship type instead of one flat cap

### 3. Consider path-aware scoring

Longer-term, scoring should incorporate not just hop count, but path meaning.

For example:

- direct explanatory doc hit may deserve more weight than a merely adjacent module
- attached human note may deserve higher weight only for rationale/history queries

## Success criteria

- graph expansion remains helpful at larger corpus sizes
- less irrelevant spillover from structurally connected nodes

---

## Phase 6 — Productionization Of The RAG Loop

## Goal

Make the retrieval system reliable in real operation, not just accurate in theory.

## Work items

### 1. Cache where it actually pays off

Candidates:

- query embeddings
- hot retrieval results for repeated repo queries
- reranker inputs/results for repeated evaluation workloads

### 2. Add latency budgets per stage

Track and enforce approximate stage budgets for:

- embedding
- scatter search
- graph expansion
- reranking
- final assembly

### 3. Add indexing / re-embedding workflows

As models evolve, you need a clean path for:

- full re-embedding
- partial re-embedding
- validating dimensional compatibility
- safe index rebuilds

### 4. Add failure-mode handling

Examples:

- embedding provider unavailable
- Neo4j unavailable
- pgvector degraded / index cold
- partial retrieval success fallback behavior

## Success criteria

- predictable latency profile
- safer model/index migrations
- graceful degradation during dependency failures

---

## Recommended Execution Order

If only a few things are done next, the priority order should be:

### Priority 1

1. **Add reranking**
2. **Create gold evaluation set**
3. **Track retrieval metrics and query-stage telemetry**

### Priority 2

4. **Tune graph expansion with evidence**
5. **Refine retrieval-unit roles (small chunk vs parent context)**
6. **Benchmark stronger code-aware embedding options**

### Priority 3

7. **Add caching and latency budgeting**
8. **Add safer re-embedding / reindex workflows**
9. **Make path-aware graph scoring more sophisticated**

---

## What Not To Do

These are the main anti-patterns to avoid.

### 1. Do not replace GraphRAG with vector-only retrieval

That would simplify the architecture but make the product weaker.

### 2. Do not overcomplicate graph logic before measurement exists

More graph scoring rules without evaluation data will create complexity faster than value.

### 3. Do not chase benchmark models without product-specific tests

The best embedding model on a leaderboard may not be the best one for:

- code symbol lookup
- mixed code/doc/human memory retrieval
- self-hosted latency constraints

### 4. Do not optimize latency before fixing ranking quality

Fast wrong answers are worse than slightly slower high-confidence answers for this product stage.

### 5. Do not flatten all spaces into one retrieval policy

Your product benefits from keeping code, docs, and human notes distinct.

---

## Suggested 30 / 60 / 90 Day Plan

## First 30 days

- add reranker prototype
- create first gold evaluation set
- instrument stage-by-stage retrieval telemetry
- compare before/after on top query classes

## 60 days

- tune graph expansion rules with measured evidence
- refine retrieval-unit policy
- benchmark one stronger code-aware embedding option
- establish model comparison workflow

## 90 days

- productionize reranking path
- add caching and latency budgets
- implement re-embedding / reindex operational flow
- make graph scoring more selective per query class

---

## Final Recommendation

Akashic Record's RAG system is already on a strong path.

The next stage should focus on:

1. **precision**
2. **measurement**
3. **specialization**
4. **operational reliability**

The most important strategic message is:

> **You do not need a new RAG direction. You need a sharper, more measurable version of the current one.**

That is a good place to be.
