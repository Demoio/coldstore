# ColdStore Phase-1 Safe Implementation Plan

> For Hermes: follow strict TDD where practical and keep execution limited to unit tests, fmt, clippy, and compile checks. Do not run integration tests or commands that touch real tape devices or host services.

Goal: make the repository materially more real without risking the host environment by implementing an in-memory metadata service, a functional HDD-backed cache service, safer non-panicking placeholders for unimplemented external-service paths, richer design docs, and Makefile targets that clearly separate unit-only verification from future integration work.

Architecture:
1. Metadata service becomes the source of truth for unit-testable CRUD/state-transition behavior using an in-memory state store behind async locks.
2. Cache service becomes a real local data/cache layer using the existing HDD backend plus an in-memory secondary index for object lookup, streaming, deletion, and stats.
3. Scheduler/tape/gateway keep distributed boundaries intact but stop using todo!() in externally callable handlers where practical, returning explicit gRPC errors instead of panicking when phase-2 features are not implemented yet.
4. Design docs are corrected to distinguish phase-1 safe local implementation from phase-2 distributed Raft/tape integration.

Tech stack: Rust, Tokio, tonic/prost, serde, chrono/prost_types timestamps, filesystem-backed HDD cache, unit tests only.

Planned file scope:
- Modify: docs/DESIGN.md
- Create: docs/plans/2026-04-15-phase1-safe-implementation-plan.md
- Modify/Create under crates/metadata/src/: service.rs plus in-memory store helpers/tests
- Modify/Create under crates/cache/src/: service.rs plus index/helpers/tests
- Modify: crates/cache/src/backend.rs
- Modify: crates/cache/src/hdd.rs
- Modify: crates/common/src/models.rs if state-transition helpers need tests or small corrections
- Modify: crates/scheduler/src/service.rs (replace panicking todo! with safe explicit unimplemented paths or implement minimal pure helpers if small)
- Modify: crates/tape/src/service.rs (same principle)
- Modify: Makefile

Verification commands (unit-only):
- cargo fmt --all -- --check
- cargo clippy --workspace --all-targets -- -D warnings
- cargo test --workspace --lib --bins
- make unit
- make check-safe

Non-goals for this session:
- No Raft/OpenRaft integration
- No RocksDB integration
- No real tape device access
- No end-to-end multi-process integration tests
- No host-environment mutation outside local build/test artifacts
