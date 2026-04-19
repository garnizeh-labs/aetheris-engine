---
Version: 0.1.0-draft
Status: Phase 1 — Proposed
Phase: All Phases
Last Updated: 2026-04-18
Authors: Team (Antigravity)
Spec References: [ENGINE_DESIGN, PROTOCOL_DESIGN, SECURITY_DESIGN]
Tier: 2
---

# Aetheris Repository Architecture & Open-Core Strategy — Technical Design Document

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Motivation & Goals](#2-motivation--goals)
3. [Repository Tier Architecture](#3-repository-tier-architecture)
4. [Dependency Graph](#4-dependency-graph)
5. [The Open-Core Boundary](#5-the-open-core-boundary)
6. [The Seam Pattern](#6-the-seam-pattern)
7. [Documentation Architecture](#7-documentation-architecture)
8. [Technical Integration & Workflow](#8-technical-integration--workflow)
9. [Migration Strategy](#9-migration-strategy)
10. [Phased Delivery Roadmap](#10-phased-delivery-roadmap)
11. [Open Questions](#11-open-questions)
12. [Appendix A — Glossary](#appendix-a--glossary)
13. [Appendix B — Decision Log](#appendix-b--decision-log)

---

## Executive Summary

This document defines the multi-repository architecture for the Aetheris ecosystem and the strategy for evolving the current monorepo into a structured set of repositories organised by contract, license, and volatility.

The project follows an **Open-Core model**: the protocol contracts, simulation engine, client runtime, and the flagship world (*Void Rush*) remain **Open Source (MIT / CC-BY)**. Advanced governance, global federation, and enterprise-grade security modules are maintained in a separate repository (currently restricted) as part of a future **Professional** offering.

The driving constraint is that the two tiers must remain **structurally compatible** — no forking of the engine. All advanced extensions are injected through Trait-based seams defined in the public protocol crate, so the authoritative simulation pipeline is identical in both tiers.

---

## 2. Motivation & Goals

### 2.1 Why Split the Monorepo

The current single-workspace `aetheris` monorepo is appropriate for the MVP phase where velocity matters more than boundary enforcement. As the project scales, three pressures emerge:

| Pressure | Risk if ignored |
|---|---|
| **License heterogeneity** | MIT code shipped alongside proprietary code in one repo creates legal ambiguity for contributors |
| **Dependency volatility** | High-churn game logic (Void Rush balance) coupled with low-churn protocol traits slows down protocol stability guarantees |
| **Security surface** | Advanced modules (AI audit, enterprise SSO, CockroachDB federation) must not be exposed in the core repo even by accident |

### 2.2 Design Goals

- **G1** — No engine fork. The same simulation binary powers both Open Source and Professional deployments.
- **G2** — Zero friction for community contributors. Core tiers must be self-sufficient (build, test, run) without access to restricted repositories.
- **G3** — Stable contract surface. `aetheris-protocol` changes must be rare and versioned semantically; downstream breakage must be catchable at compile time.
- **G4** — Clean licensing. Each repository carries a single, unambiguous license file.
- **G5** — Incremental migration. The split must be achievable in stages without a "big bang" repo restructure.

---

## 3. Repository Tier Architecture

The ecosystem is organised into five functional tiers. Tiers 1–4 are public; Tier 5 is private.

```text
┌───────────────────────────────────────────────────────────────────────────┐
│  TIER 5 — (Restricted / Proprietary)                                     │
│                                                                           │
│  AI Audit Worker · Global Federation Coordinator · Enterprise SSO · SSR  │
│                                                                           │
│  Injects into Tier 2 via Trait seams. Never modifies engine source.       │
└────────────────────────────┬──────────────────────────────────────────────┘
                             │ uses (restricted registry)
┌────────────────────────────▼──────────────────────────────────────────────┐
│  TIER 4 — void-rush  (Public / CC-BY)                                    │
│                                                                           │
│  Ships, Asteroids, Ore · Combat Systems · ECS Components · World Assets   │
│                                                                           │
│  Reference implementation. Proves the engine works for complex MMOs.      │
└────────────────────────────┬──────────────────────────────────────────────┘
                             │ uses
┌────────────────────────────▼──────────────────────────────────────────────┐
│  TIER 3 — aetheris-client  (Public / MIT)                                │
│  TIER 2 — aetheris-engine  (Public / MIT)                                │
│                                                                           │
│  Client: 3-Worker arch · SAB management · wgpu pipelines · Playground    │
│  Engine: Spatial Hash · Priority Channels · Interest Management · Auth   │
└────────────────────────────┬──────────────────────────────────────────────┘
                             │ uses
┌────────────────────────────▼──────────────────────────────────────────────┐
│  TIER 1 — aetheris-protocol  (Public / MIT)                              │
│                                                                           │
│  WorldState · GameTransport · Encoder traits · NetworkId · .proto files  │
└───────────────────────────────────────────────────────────────────────────┘
```

### 3.1 Tier 1 — `aetheris-protocol` (Public / MIT)

**Role:** The Contract Repository. Defines the binary interface shared by all other tiers.

**Contents:**

- Core traits: `WorldState`, `GameTransport`, `Encoder`, `AuditSink`
- `NetworkId`, `ComponentKind`, and error enums
- Protobuf definitions (`.proto` files) for the Control Plane gRPC API
- No implementations, no heavy dependencies

**Volatility:** Very Low. Changes here are breaking by definition and require a semver major bump.

**Rationale:** Isolating this crate means WASM clients, Rust servers, and Professional modules all share the exact same binary contract without pulling in engine implementation dependencies. This is the single "pinning point" for ecosystem compatibility.

### 3.2 Tier 2 — `aetheris-engine` (Public / MIT)

**Role:** The Framework Repository. Core simulation pipeline implementation.

**Contents:**

- Spatial Hash Grid
- Priority Channel pipeline (`ChannelRegistry`)
- Interest Management (sector-based delta compression)
- Standard authentication (OIDC / PASETO)
- `NoOp` default implementations for all Tier 5 seams

**Volatility:** Medium. Algorithm refinements are frequent; trait signatures are stable.

**Rationale:** Provides the "Lego bricks" for any authoritative multiplayer simulation. The `NoOp` defaults ensure the engine compiles and runs without any private dependency.

### 3.3 Tier 3 — `aetheris-client` (Public / MIT)

**Role:** The Frontend Repository. Browser-side execution environment.

**Contents:**

- Three-Worker architecture (Main, Game, Render)
- `SharedArrayBuffer` memory management
- `wgpu` render pipelines
- WASM bindings (`aetheris-client-wasm`)
- Aetheris Playground (`playground/`)

**Volatility:** High. Rendering and UX changes are continuous.

**Rationale:** Centralising the complex WASM/TypeScript orchestration in one repo allows rendering engineers to validate pipeline changes in the Playground without touching the server-side codebase.

### 3.4 Tier 4 — `void-rush` (Public / CC-BY)

**Role:** The Application Repository. The flagship Open Source world.

**Contents:**

- ECS component definitions: Ships, Asteroids, Ore
- Combat and physics systems
- Balance configuration
- World assets (art, audio metadata)

**Volatility:** Very High — balance and content tuning are near-daily activities.

**License note:** CC-BY rather than MIT communicates that this is a *creative work* (game content) as much as a software artefact, and mandates attribution when the world is forked.

**Rationale:** A playable, well-documented reference world lowers the barrier for community contributors to understand the engine without reading implementation source.

### 3.5 Tier 5 — (Restricted / Proprietary)

**Role:** The Enterprise Repository. Advanced extensions injected via Trait seams.

**Contents:**

- `ProBehavioralAudit` — AI-powered anomaly detection Worker (replaces `NoOpAudit`)
- `GlobalFederationCoordinator` — Multi-region CockroachDB synchronisation (replaces `NoOpFederation`)
- Enterprise SSO adapter (Okta, Azure AD, SAML 2.0; replaces OIDC/PASETO auth)
- Server-Side Rendering (SSR) snapshots for admin dashboards
- Multi-tenant orchestrator

**Volatility:** Medium. Professional features evolve on longer cycles than game content.

**Access:** [This module is not available yet].

---

## 4. Dependency Graph

```text
(Restricted Tier)
    ├── aetheris-engine   (Tier 2)
    └── aetheris-protocol (Tier 1)

void-rush
    ├── aetheris-engine   (Tier 2)
    └── aetheris-protocol (Tier 1)

aetheris-client
    └── aetheris-protocol (Tier 1)   ← WASM bindings only; no engine dep

aetheris-engine
    └── aetheris-protocol (Tier 1)
```

**Key invariants:**

- `aetheris-client` never depends on `aetheris-engine`. The client knows only the protocol traits.
- Tier 5 never modifies Tier 1 or Tier 2 source — it only implements traits.
- Tier 4 (`void-rush`) is not a dependency of any other tier.

---

## 5. The Open-Core Boundary

The following table defines the feature split between the Open Source and commercial tiers.

| Feature | Open Source (Core) | Professional (Future) |
|---|---|---|
| **Simulation** | Authoritative 60 Hz pipeline | Identical + SSR snapshot API |
| **Security** | Trait-based invariant clamping | AI Behavioral Audit (Not available) |
| **Scale** | Regional shard (single cluster) | Global topology (multi-region CockroachDB) |
| **Authentication** | Google OAuth / Email OTP (PASETO) | Enterprise SSO (Okta, Azure AD, SAML 2.0) |
| **Tenancy** | Single world per process | Multi-tenant orchestrator |
| **Audit** | `NoOpAudit` (event log only) | `ProBehavioralAudit` + anomaly alerts |
| **Support** | Community-driven (GitHub Issues) | SLA-guaranteed (Not available) |
| **Dashboard** | Playground sandbox | SSR admin snapshots + usage analytics |

### 5.1 What is Never Gated

The following capabilities are always Open Source regardless of customer tier. Gating them would undermine contributor trust and violate the core thesis that the engine should be auditable:

- The authoritative tick pipeline and all `WorldState` mutations
- The Priority Channel and Interest Management algorithms
- The Spatial Hash Grid
- The Merkle Chain and Event Ledger schema
- All `.proto` definitions

---

## 6. The Seam Pattern

All private features are injected through **Traits defined in `aetheris-protocol`** (Tier 1). This is the architectural guarantee that the engine is never forked.

### 6.1 Pattern: `NoOp` defaults in `aetheris-engine`, advanced implementations in restricted repo

```rust
// Tier 1 — aetheris-protocol: defines the seam
pub trait AuditSink: Send + Sync + 'static {
    fn record_mutation(&self, entity: NetworkId, mutation: &StateMutation);
    fn flush(&self) -> Vec<AuditEvent>;
}

// Tier 2 — aetheris-engine: ships a no-op default (Open Source path)
pub struct NoOpAudit;
impl AuditSink for NoOpAudit {
    fn record_mutation(&self, _: NetworkId, _: &StateMutation) {}
    fn flush(&self) -> Vec<AuditEvent> { vec![] }
}

// Tier 5: injects the real implementation (Advanced path)
pub struct ProBehavioralAudit { /* ML model, anomaly thresholds, ... */ }
impl AuditSink for ProBehavioralAudit {
    fn record_mutation(&self, entity: NetworkId, mutation: &StateMutation) { /* ... */ }
    fn flush(&self) -> Vec<AuditEvent> { /* ... */ }
}
```

### 6.2 Seam Inventory

| Seam Trait | `NoOp` (OS default) | `Pro` implementation | Owner |
|---|---|---|---|
| `AuditSink` | `NoOpAudit` | `ProBehavioralAudit` | Tier 5 |
| `FederationCoordinator` | `NoOpFederation` | `GlobalFederationCoordinator` | Tier 5 |
| `SsoProvider` | `OidcProvider` (PASETO) | `EnterpriseSsoAdapter` | Tier 5 |
| `TenantOrchestrator` | `SingleWorldOrchestrator` | `MultiTenantOrchestrator` | Tier 5 |

### 6.3 Compile-time Safety

Because `Pro` structs implement the same Trait as the `NoOp` defaults, the type checker enforces correctness at compile time. There is no runtime `if cfg!(feature = "pro")` branching in the engine — the pro binary is simply a different `main.rs` that wires different implementations into the builder.

---

## 7. Documentation Architecture

Documentation follows the same Open-Core boundary as code. The governing rule is: **docs live with the code they describe**. A design document that covers a Professional seam implementation belongs in the restricted repository; one that covers a public crate ships alongside that crate and is visible to all contributors.

### 7.1 Visibility Levels

Three visibility levels apply to every document in the ecosystem:

| Level | Audience | Location |
|---|---|---|
| **Public** | Community · Contributors · Customers | Repo `docs/`, GitHub Pages, or `docs.aetheris.io` |
| **Internal** | Core team only | `docs/ideas/`; never published or indexed |
| **Private** | Core team only | Restricted docs |

### 7.2 Document Distribution by Tier

The table below maps every design document to its target repository and visibility level after the multi-repo migration. During Phase 1, all documents remain in the `docs/` folder of the current monorepo.

#### Tier 1 — `aetheris-protocol` (Public)

Documents specifying the contract surface: trait definitions, binary formats, protobuf schemas, and versioning policy.

| Document | Current path | Post-split location |
|---|---|---|
| `PROTOCOL_DESIGN.md` | `docs/design/` | `aetheris-protocol/docs/` |
| `ENCODER_DESIGN.md` | `docs/design/` | `aetheris-protocol/docs/` |
| `CONTROL_PLANE_DESIGN.md` | `docs/design/` | `aetheris-protocol/docs/` |
| `API_DESIGN.md` | `docs/design/` | `aetheris-protocol/docs/` |
| `NETWORKING_DESIGN.md` | `docs/design/` | `aetheris-protocol/docs/` |
| `TRANSPORT_DESIGN.md` | `docs/design/` | `aetheris-protocol/docs/` |

#### Tier 2 — `aetheris-engine` (Public)

Documents covering the authoritative simulation pipeline, spatial algorithms, and server-side infrastructure. Includes this document.

| Document | Current path | Post-split location |
|---|---|---|
| `ENGINE_DESIGN.md` | `docs/design/` | `aetheris-engine/docs/` |
| `SPATIAL_PARTITIONING_DESIGN.md` | `docs/design/` | `aetheris-engine/docs/` |
| `PRIORITY_CHANNELS_DESIGN.md` | `docs/design/` | `aetheris-engine/docs/` |
| `INTEREST_MANAGEMENT_DESIGN.md` | `docs/design/` | `aetheris-engine/docs/` |
| `ROOM_AND_INSTANCE_DESIGN.md` | `docs/design/` | `aetheris-engine/docs/` |
| `INTEGRATION_DESIGN.md` | `docs/design/` | `aetheris-engine/docs/` |
| `MIGRATION_DESIGN.md` | `docs/design/` | `aetheris-engine/docs/` |
| `TESTING_DESIGN.md` | `docs/design/` | `aetheris-engine/docs/` |
| `CONFIGURATION_DESIGN.md` | `docs/design/` | `aetheris-engine/docs/` |
| `ERROR_HANDLING_DESIGN.md` | `docs/design/` | `aetheris-engine/docs/` |
| `DEVELOPER_GUIDE.md` | `docs/design/` | `aetheris-engine/docs/` |
| `REPOSITORY_ARCHITECTURE_DESIGN.md` | `docs/design/` | `aetheris-engine/docs/` |
| `SECURITY_DESIGN.md` ¹ | `docs/design/` | `aetheris-engine/docs/` |
| `OBSERVABILITY_DESIGN.md` ¹ | `docs/design/` | `aetheris-engine/docs/` |
| `PERSISTENCE_DESIGN.md` ¹ | `docs/design/` | `aetheris-engine/docs/` |
| `DEPLOYMENT_DESIGN.md` ¹ | `docs/design/` | `aetheris-engine/docs/` |

> ¹ These documents describe both Open Source and Professional concerns. During migration they will be **split**: the OS sections remain in the public `aetheris-engine/docs/` file and a companion `*_PRO.md` extension is created in the restricted repository.

#### Tier 3 — `aetheris-client` (Public)

Documents covering browser-side execution: Workers, WASM, rendering, and the Playground.

| Document | Current path | Post-split location |
|---|---|---|
| `CLIENT_DESIGN.md` | `docs/design/` | `aetheris-client/docs/` |
| `WORKER_COMMUNICATION_DESIGN.md` | `docs/design/` | `aetheris-client/docs/` |
| `INPUT_PIPELINE_DESIGN.md` | `docs/design/` | `aetheris-client/docs/` |
| `ASSET_STREAMING_DESIGN.md` | `docs/design/` | `aetheris-client/docs/` |
| `PLAYGROUND_DESIGN.md` | `docs/design/` | `aetheris-client/docs/` |

#### Tier 4 — `void-rush` (Public / CC-BY)

Game-specific documents: game design doc, ECS components, themed world specs, and platform design. Licensed CC-BY.

| Document | Current path | Post-split location |
|---|---|---|
| `VOID_RUSH_GDD.md` | `docs/design/` | `void-rush/docs/` |
| `ECS_DESIGN.md` | `docs/design/` | `void-rush/docs/` |
| `THEME_WORLD_DESIGN.md` | `docs/design/` | `void-rush/docs/` |
| `PLATFORM_DESIGN.md` | `docs/design/` | `void-rush/docs/` |

| `SSR_DESIGN.md` | `docs/design/` | (Restricted) |
| `FEDERATION_DESIGN.md` | `docs/design/` | (Restricted) |
| `AUDIT_DESIGN.md` | `docs/design/` | (Restricted) |
| `PLATFORM_SPEC_PRO.md` | `docs/design/` | (Restricted) |
| `SECURITY_DESIGN_PRO.md` ² | (to be created) | (Restricted) |
| `OBSERVABILITY_DESIGN_PRO.md` ² | (to be created) | (Restricted) |
| `PERSISTENCE_DESIGN_PRO.md` ² | (to be created) | (Restricted) |
| `DEPLOYMENT_DESIGN_PRO.md` ² | (to be created) | (Restricted) |

> ² Pro-only companion files split from the documents marked ¹ above.

#### Shared / Cross-Cutting (monorepo root → public docs site)

Top-level documentation spanning all tiers. These remain at the monorepo root during Phase 1 and are published to the community-facing documentation site post-split.

| Document | Current path | Post-split location |
|---|---|---|
| `README.md` | `docs/` | root of each repo + docs site |
| `GLOSSARY.md` | `docs/` | docs site |
| `ROADMAP.md` | `docs/` | docs site |
| `getting-started.md` | `docs/` | docs site |
| `release.md` | `docs/` | docs site |
| `security.md` | `docs/` | docs site (security disclosure policy) |
| `architecture/crates_overview.md` | `docs/` | docs site |
| `benchmarks/STRESS_TEST_RESULTS.md` | `docs/` | docs site |

#### Internal Only (not published)

The following documents are strictly internal and must not be published to any public site or repository.

| Document | Current path | Rationale |
|---|---|---|
| `ideas/BACKLOG.md` | `docs/` | Unvetted backlog; publication creates expectation of delivery |
| `ideas/TODO.md` | `docs/` | Internal task tracking |

### 7.3 Invariants

- **I1** — No document describing a Pro seam *implementation* (Tier 5 content) may be committed to a public repository.
- **I2** — Every public crate ships with at least a `README.md` and a `docs/` subfolder containing its relevant design documents.
- **I3** — Shared cross-cutting documents (glossary, getting-started, security policy) are maintained at the ecosystem root and are the single source of truth — not duplicated per-repo.
- **I4** — Internal-only documents (`docs/ideas/`) are never published to a docs site, added to CI artifact outputs, or referenced in public-facing `README`s.
- **I5** — Documents marked ¹ (split documents) must not contain Pro implementation details in their public form. A placeholder sentence pointing to the private companion file is sufficient.

### 7.4 Documentation Hosting (Post-Split)

[Documentation hosting details are not available yet.]

---

## 8. Technical Integration & Workflow

### 7.1 Local Development (Current Monorepo Phase)

During Phase 1, all crates remain in the current `aetheris` workspace using `path` dependencies. This preserves developer velocity while the seam interfaces are being defined.

```toml
# Cargo.toml (workspace root) — Phase 1
[workspace]
members = [
    "crates/aetheris-protocol",
    "crates/aetheris-server",      # future: aetheris-engine
    "crates/aetheris-client-wasm", # future: aetheris-client
    # ...
]
```

### 7.2 Post-Split (Phase 3+)

Once the seam traits are stabilised, the crates are extracted to their own repositories. Consumers switch from `path` to versioned registry dependencies.

```toml
# Cargo.toml in void-rush (Phase 3+)
[dependencies]
aetheris-protocol = "1.0"   # from crates.io
aetheris-engine   = "1.0"   # from crates.io
```

aetheris-protocol = "1.0"   # from registry
aetheris-engine   = "1.0"   # from registry
# Professional modules (restricted)
# ...

| Repository | CI target | Registry |
|---|---|---|
| `aetheris-protocol` | Publish on tag | Public |
| `aetheris-engine` | Publish on tag | Public |
| `aetheris-client` | Build WASM | Public |
| `void-rush` | Integration test | Public |
| Tier 5 | Internal CI | Restricted |

### 7.4 Protocol Versioning Contract

`aetheris-protocol` follows strict **semver**:

- **Patch** — doc fixes, non-breaking additions to error enums.
- **Minor** — new optional trait methods with default implementations.
- **Major** — any breaking change to an existing trait method signature.

All other tiers pin to a `~major.minor` range. Breaking protocol changes require coordinated releases across the ecosystem.

---

## 9. Migration Strategy

The monorepo → multi-repo migration follows a phased approach to avoid disruption.

### Phase A — Boundary Enforcement (Phase 1, no repo split)

- Enforce crate boundary via `[workspace.dependencies]` pinning.
- Add `#[doc(hidden)]` and `pub(crate)` visibility discipline to identify what truly belongs to each future repo.
- Add CI check: `aetheris-protocol` must have zero dependencies outside `std` and `prost`.
- Define all seam traits in `aetheris-protocol` even while implementations live in-tree.

### Phase B — Protocol Extraction (Phase 2)

- Extract `crates/aetheris-protocol` to its own repository.
- Publish `aetheris-protocol = "0.1"` to `crates.io`.
- Remaining monorepo crates switch to the registry dependency.
- Validation: `void-rush` (still in monorepo) builds cleanly against the extracted crate.

### Phase C — Engine & Client Extraction (Phase 3)

- Extract server-side engine crates to `aetheris-engine`.
- Extract client-side crates and Playground to `aetheris-client`.
- Publish both to `crates.io`.

- Move game-specific code to the `void-rush` repository.
- Bootstrap restricted repository with advanced implementations.
- [Phase not available yet].

---

## 10. Phased Delivery Roadmap

### Phase 1 — Foundation (Current)

- [x] Define seam traits in `aetheris-protocol`
- [ ] Add `NoOp` default implementations for all seams
- [ ] Enforce zero external deps in `aetheris-protocol` (CI gate)
- [ ] Document the Seam Inventory (§6.2)
- [ ] Audit all `docs/design/` documents and assign a target tier (§7.2)
- [ ] Add `README.md` to `docs/ideas/` marking it as internal-only (never published)

### Phase 2 — Protocol Extraction

- [ ] Extract `aetheris-protocol` to standalone repo
- [ ] Publish `0.1.0` to `crates.io`
- [ ] Validate monorepo builds against registry crate
- [ ] Establish semver governance policy
- [ ] Migrate Tier 1 design documents to `aetheris-protocol/docs/`

### Phase 3 — Engine, Client & Void Rush Extraction

- [ ] Extract `aetheris-engine` repository
- [ ] Extract `aetheris-client` repository
- [ ] Extract `void-rush` repository
- [ ] Publish all three to `crates.io`
- [ ] Set up integration test matrix across repos
- [ ] Migrate Tier 2 design documents to `aetheris-engine/docs/`
- [ ] Migrate Tier 3 design documents to `aetheris-client/docs/`
- [ ] Migrate Tier 4 design documents to `void-rush/docs/`
- [ ] Split mixed-tier documents (`SECURITY_DESIGN`, `OBSERVABILITY_DESIGN`, `PERSISTENCE_DESIGN`, `DEPLOYMENT_DESIGN`) into public and `_PRO` variants
- [Phase not available yet]

---

## 11. Open Questions

| # | Question | Owner | Priority |
|---|---|---|---|
| OQ1 | Which registry to use for restricted crates? | Infra | P2 |
| OQ2 | Should `void-rush` be a monorepo (content + code) or split into `void-rush-server` and `void-rush-assets`? | Game Team | P3 |
| OQ3 | How do we handle `aetheris-protocol` patch releases when a Pro customer needs a hotfix but the public minor hasn't shipped? | Eng Lead | P3 |
| OQ4 | License compatibility: can CC-BY `void-rush` assets be used by Pro customers in a proprietary derivative world? | Legal | P2 |
| OQ5 | Which static site generator for `docs.aetheris.io`: mdBook, Docusaurus, or MkDocs? Each has different multi-repo aggregation support. | Docs Team | P3 |
| OQ6 | Should `benchmarks/STRESS_TEST_RESULTS.md` be auto-updated by CI and published, or remain a manually curated snapshot? | Eng Lead | P3 |

---

## Appendix A — Glossary

| Term | Definition |
|---|---|
| **Open Core** | Business model where the core product is open source and commercial value is added through proprietary extensions |
| **Seam** | A Trait-defined injection point where a `NoOp` default can be replaced with a Pro implementation without modifying engine source |
| **Tier** | A functional grouping of repositories by contract stability, license, and volatility |
| **`NoOp`** | A zero-cost default implementation of a seam trait; ships with the open source engine |
| **Professional** | Advanced modules containing proprietary implementations |
| **registry** | A package host; public for open source tiers, restricted for Professional modules |
| **Volatility** | The expected rate of change of a codebase; drives the decision of how tightly to couple repositories |

---

## Appendix B — Decision Log

| # | Decision | Rationale |
|---|---|---|
| D1 | Protocol isolation in Tier 1 | Prevents circular dependencies between Client and Server; enables independent semver lifecycle |
| D2 | `NoOp` defaults in public engine | Ensures the engine compiles and runs without any private dependency; contributors need no Pro access |
| D3 | `void-rush` as CC-BY, not MIT | Game content (art direction, balance data) warrants attribution requirements that MIT does not enforce |
| D4 | No `cfg!(feature = "pro")` in engine source | Feature flags in the engine would require Pro customers to build the engine from source, creating a security risk; instead, Pro is a separate binary that wires different trait impls |
| D5 | Restricted SSR | SSR is infrastructure-heavy (GPU-on-server, snapshot scheduling); keeping it restricted avoids overwhelming OS contributors |
| D6 | `aetheris-client` has no dep on `aetheris-engine` | The client knows only the protocol traits; this prevents accidental server logic from leaking into WASM bundles |
| D7 | Phased migration, no big-bang split | Incremental extraction preserves CI green status; each phase is independently shippable |
| D8 | Open Source Void Rush | A playable, community-accessible proof-of-concept lowers the contributor barrier and validates the engine for complex MMO workloads in public |
| D9 | Docs live with their code tier | Each tier's documentation ships in its own repository; this enforces the same open-core boundary as the code and prevents accidental disclosure of Pro feature details in public commits |
| D10 | `docs/ideas/` is internal-only | Backlog and TODO items represent unvetted work; publishing them creates community expectations and may reveal strategic intent ahead of schedule |
| D11 | [Not available yet] |
