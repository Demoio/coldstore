# Phase 2 Metadata Persistence and Raft Implementation Plan

> **For Hermes:** Use subagent-driven-development skill to implement this plan task-by-task.

**Goal:** Evolve ColdStore Metadata from the current in-process state machine plus local snapshot into a durable OpenRaft + RocksDB metadata cluster.

**Architecture:** Keep the existing `MetadataState` semantics as the single source of truth. Phase 2A first persists the state machine through an opt-in local binary snapshot, which is already implemented in `MetadataServiceImpl::new_with_snapshot(config, path)`. Phase 2B wraps the same mutations as Raft commands, applies them through an OpenRaft state machine, and stores Raft log/state machine data in RocksDB/openraft-rocksstore.

**Tech Stack:** Rust, tonic, prost, tokio, OpenRaft, RocksDB/openraft-rocksstore, existing `coldstore-proto` messages.

---

## Current baseline

Current code has:

- `crates/metadata/src/service.rs`
  - `MetadataState` with objects, buckets, archive bundles, archive tasks, recall tasks, tapes, scheduler/cache/tape worker registries.
  - `MetadataServiceImpl::new(config)` for in-memory Phase 1 behavior.
  - `MetadataServiceImpl::new_with_snapshot(config, snapshot_path)` for opt-in Phase 2A single-node snapshot persistence.
  - Binary snapshot encode/decode using existing prost message encodings.
- Unit test:
  - `persistent_snapshot_survives_service_restart`

Safe verification command:

```bash
cargo test -p coldstore-metadata --lib
```

Do not run integration tests or start multi-process services unless explicitly requested.

---

## Task 1: Extract mutation semantics into explicit metadata commands

**Objective:** Define a Raft-friendly command enum without changing runtime behavior yet.

**Files:**
- Modify: `crates/metadata/src/service.rs`
- Test: `crates/metadata/src/service.rs`

**Steps:**
1. Add an internal `MetadataCommand` enum covering existing mutating operations: create/delete bucket, put/delete object, update storage class, update archive location, update restore status, put/update archive bundle/task/recall/tape, register/deregister/update worker, heartbeat.
2. Add a pure `apply_command(state: &mut MetadataState, command: MetadataCommand) -> Result<(), Status>` helper.
3. Move one low-risk operation first, e.g. create bucket, behind `apply_command`.
4. Add a unit test proving duplicate bucket still returns `AlreadyExists`.
5. Repeat for the remaining mutations in small commits.

**Verification:**

```bash
cargo test -p coldstore-metadata --lib
```

---

## Task 2: Make snapshot persistence command-oriented

**Objective:** Ensure every state mutation goes through a single apply path, then persists snapshot after successful apply.

**Files:**
- Modify: `crates/metadata/src/service.rs`

**Steps:**
1. Add `MetadataServiceImpl::apply_and_persist(command)`.
2. For `snapshot_path = None`, only apply in memory.
3. For `snapshot_path = Some`, apply then atomically write snapshot.
4. Replace direct write-lock mutation blocks with `apply_and_persist`.
5. Keep read-only methods unchanged.

**Verification:**

```bash
cargo test -p coldstore-metadata persistent_snapshot_survives_service_restart --lib
cargo test -p coldstore-metadata --lib
```

---

## Task 3: Introduce a Raft state-machine facade behind a feature flag

**Objective:** Add OpenRaft dependency and compile a minimal state-machine type without making the default service use Raft.

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/metadata/Cargo.toml`
- Create: `crates/metadata/src/raft.rs`
- Modify: `crates/metadata/src/lib.rs`

**Steps:**
1. Add optional workspace dependencies for `openraft` and RocksDB/openraft-rocksstore.
2. Add a `metadata-raft` feature to `coldstore-metadata`.
3. Create `raft.rs` with ColdStore-specific Raft type aliases and state-machine shell.
4. Compile with default features off first; then compile with `--features metadata-raft`.

**Verification:**

```bash
cargo check -p coldstore-metadata
cargo check -p coldstore-metadata --features metadata-raft
```

---

## Task 4: Persist Raft state machine snapshots into RocksDB

**Objective:** Replace the Phase 2A file snapshot with a RocksDB-backed state-machine snapshot for Raft-enabled builds.

**Files:**
- Modify: `crates/metadata/src/raft.rs`
- Add tests under `crates/metadata/src/raft.rs` guarded by `#[cfg(test)]` and feature flags.

**Steps:**
1. Define a deterministic snapshot format reusing `encode_snapshot`/`decode_snapshot`.
2. Store snapshots under a dedicated RocksDB column family or key prefix.
3. Add a unit test with a temp RocksDB directory.
4. Do not start networked Raft nodes yet.

**Verification:**

```bash
cargo test -p coldstore-metadata --features metadata-raft --lib
```

---

## Task 5: Route MetadataService writes through Raft when enabled

**Objective:** Make gRPC write APIs propose commands to Raft in `metadata-raft` mode, while keeping existing local mode for safe tests.

**Files:**
- Modify: `crates/metadata/src/service.rs`
- Modify: `crates/metadata/src/raft.rs`

**Steps:**
1. Add a backend enum: `Local` and `Raft`.
2. For local mode, use existing `apply_and_persist`.
3. For Raft mode, submit `MetadataCommand` to OpenRaft.
4. Keep read APIs using the applied state machine.
5. Add unit tests around backend selection without opening network sockets.

**Verification:**

```bash
cargo test -p coldstore-metadata --lib
cargo test -p coldstore-metadata --features metadata-raft --lib
```

---

## Task 6: Add safe single-process Raft tests

**Objective:** Verify Raft command application without external services or host-risky resources.

**Files:**
- Modify: `crates/metadata/src/raft.rs`

**Steps:**
1. Build a single-node in-process Raft test harness.
2. Propose create bucket + put object commands.
3. Assert state machine contains expected metadata.
4. Assert persisted RocksDB state can be reopened.

**Verification:**

```bash
cargo test -p coldstore-metadata --features metadata-raft --lib
```

---

## Non-goals for this plan

- No real tape device access.
- No SPDK access.
- No destructive host-level integration tests.
- No multi-process cluster startup unless the user explicitly asks for it.
- No production migration tooling until the command/state-machine shape is stable.
