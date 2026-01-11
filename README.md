# Tape-based Cold Storage Archive System (Cold Storage with Tape)

## 1. Project Background

As object storage systems continue to grow in scale, the cost and energy consumption of online storage media (SSD / HDD) increasingly become system bottlenecks. For data that is accessed extremely infrequently but must be retained for long periods (cold data), using tape as a cold storage medium—combined with automated hot/cold tiering and retrieval mechanisms—is a proven, industry‑standard, and cost‑optimal solution.

The goal of this project is to build a system that is:

> **S3‑compatible, tape‑backed, and capable of automatic hot/cold tiering and on‑demand restore (thawing)**

The system targets government, enterprise, research, finance, and media workloads that require low cost, long‑term retention, and high reliability.

---

## 2. Design Goals

* Maintain full S3 protocol transparency for upper‑layer applications, requiring no business logic changes
* Support automatic migration of object data to tape and other cold storage media
* Provide controllable, observable, and extensible restore and rehydration capabilities
* Support enterprise‑grade archive workflows involving offline tapes and human intervention
* Achieve a practical engineering balance among performance, capacity, and reliability

---

## 3. Core Functional Requirements

### 3.1 Protocol and Interface Compatibility

* Support standard S3 semantics for object upload, download, and metadata operations
* Support triggering cold data retrieval via extended APIs or S3 Restore semantics
* Do not break existing S3 clients, SDKs, or applications

---

### 3.2 Automatic Cold Data Tiering and Archival

Support policy‑based migration of object data from general‑purpose storage to cold storage media:

* Lifecycle rules (time‑based)
* Access frequency (hot/cold identification)
* Object tags or bucket‑level policies

After archival:

* Object metadata remains visible and accessible
* Data and metadata are decoupled
* Metadata permanently resides on online storage

---

### 3.3 Restore and Data Rehydration

* Provide restore APIs for users to explicitly request data retrieval

* Automatically rehydrate data back to hot/warm storage after restore

* Implement a complete restore state machine:

  * `pending`
  * `in‑progress`
  * `completed`
  * `expired`

* Support TTL‑based expiration of restored data

---

### 3.4 Cache and Access Optimization

* Provide a cache layer for restored data to avoid repeated tape reads
* Serve GET requests directly from cache for recently accessed objects
* Cache policies support:

  * Capacity limits
  * Access frequency
  * TTL‑based eviction

---

### 3.5 Performance and Scalability Targets

* Cold data archival write throughput ≥ **300 MB/s**
* System throughput scales near‑linearly via horizontal expansion
* Overall bandwidth increases by adding nodes and tape drives

---

### 3.6 Concurrent Restore Capability

* Restore throughput ≥ **1 restore task per 5 minutes**
* Restore capacity scales near‑linearly with resource growth
* Implement restore queues and scheduling to minimize tape swaps

---

### 3.7 Offline Tape and Human‑in‑the‑Loop Collaboration

* Support offline tape storage scenarios

* When a restore request hits an offline tape:

  * Automatically send notifications (messages / tickets / alerts)
  * Prompt operators to scan or confirm tape identifiers

* Support batch restore merging:

  * Merge multiple requests targeting the same tape or archive bundle
  * Execute unified notification, loading, and reading workflows

---

### 3.8 Data Protection and Capacity Efficiency

Support multiple cold data reliability strategies:

* Dual‑tape replication (multiple copies)
* Tape‑level or file‑level erasure coding (EC)

Reliability and storage efficiency are configurable trade‑offs.

---

### 3.9 Cold Data Lifecycle Maintenance

* Support automatic update and deletion of cold data
* Allow multiple updates or deletions during the data lifecycle
* Support at least **one full cold‑data maintenance cycle per year**
* Provide verifiable and traceable consistency between metadata and tape data

---

### 3.10 Hardware and Media Compatibility

* Support mainstream tape libraries and tape devices
* At minimum, support **LTO‑9** and **LTO‑10**
* Remain vendor‑neutral with respect to tape manufacturers

---

## 4. Overall Technical Architecture

### 4.1 Architecture Overview

The system adopts a layered, decoupled architecture:

* **S3 Access Layer**: External object access interface
* **Metadata & Policy Management Layer**: Hot/cold state, lifecycle, and indexing
* **Tiering & Scheduling Layer**: Core archival and restore orchestration
* **Cache Layer**: Online cache for restored data
* **Tape Management Layer**: Unified abstraction for tapes, drives, and libraries
* **Notification & Collaboration Layer**: Offline tape and human interaction

---

### 4.2 Key Components

#### S3 Access Layer

* Handles PUT / GET / HEAD requests
* Intercepts access to cold objects and evaluates state
* Can be implemented via MinIO or a custom S3 proxy

#### Metadata Service

* Maintains object hot/cold state and archive locations
* Manages tape indexing and restore state machines
* Metadata is always stored on online storage (KV / RDB)

#### Archive Scheduler

* Scans objects eligible for archival
* Performs batch aggregation and sequential tape writes
* Coordinates tape drives for write operations

#### Recall Scheduler

* Manages restore request queues
* Merges restore requests targeting the same tape or archive bundle
* Controls concurrency and minimizes tape swaps

#### Cache Layer

* Stores restored data
* Serves GET requests with low latency
* Supports multi‑tier caching (SSD / HDD)

#### Tape Management & Notification Module

* Abstracts tape libraries, drives, and media
* Tracks offline tape status
* Integrates with notification systems for human intervention

---

## 5. Typical Workflows

### 5.1 Cold Data Archival Flow

1. Object is written via S3
2. Lifecycle policy is triggered
3. Scheduler aggregates objects and writes them to tape
4. Metadata hot/cold state is updated
5. Online storage space is reclaimed

### 5.2 Cold Data Restore Flow

1. User issues a restore request
2. System checks tape online/offline status
3. Online: automatic scheduling and rehydration
4. Offline: notification and human intervention
5. Data is restored and written into cache
6. Object becomes readable again

---

## 6. Non‑Functional Design Considerations

* **High Availability**: Metadata and schedulers must support HA
* **Observability**: Full monitoring of archive, restore, and tape states
* **Compliance**: Optional WORM and long‑term retention policies
* **Extensibility**: Adding tapes or drives must not disrupt existing systems

---

## 7. Implementation Based on Familiar Technology Stack

This section translates the design into an implementable architecture using object storage, S3, custom schedulers, and tape systems.

### 7.1 Technology Selection

| Layer        | Technology                   | Description                                       |
| ------------ | ---------------------------- | ------------------------------------------------- |
| S3 Access    | MinIO / RustFS               | S3‑compatible API, PUT/GET/HEAD/Restore semantics |
| Metadata     | KV / RDB (etcd / PostgreSQL) | Hot/cold state, tape index, task state            |
| Scheduling   | Custom Scheduler (Rust)      | Core archive and recall logic                     |
| Cache        | Local SSD / HDD + LRU        | Restored data cache                               |
| Tape Access  | LTFS / Vendor SDK / SCSI     | LTO‑9 / LTO‑10 integration                        |
| Notification | Webhook / MQ / Ticketing     | Human collaboration                               |

---

### 7.2 S3 Integration and Cold Object Semantics

#### 7.2.1 Cold Object Access Control

* After archival, only metadata and placeholders remain online
* Regular GET on cold objects returns an S3‑like `InvalidObjectState`
* RestoreObject or extended APIs trigger recall scheduling

This can be implemented via:

* MinIO Lifecycle / ILM extensions, or
* Direct integration into RustFS object access paths

---

### 7.3 Metadata Model (Core)

#### 7.3.1 Object Metadata

* bucket / object / version
* `storage_class`: HOT / WARM / COLD
* `archive_id` (archive bundle ID)
* `tape_id` / `tape_set`
* checksum / size
* `restore_status` / `restore_expire_at`

#### 7.3.2 Archive Bundle

* Aggregation unit for sequential tape writes
* Minimum scheduling unit for tape I/O
* Used to:

  * Maximize throughput
  * Merge restore requests

---

### 7.4 Cold Data Archival Implementation

**Write Path**

1. Object written to MinIO / RustFS
2. Lifecycle policy marks it as `COLD_PENDING`
3. Archive Scheduler aggregates objects
4. Sequential tape write (≥ 300 MB/s)
5. Metadata updated and online data released

**Key Points**

* Tape writes must be sequential
* Support dual‑write or EC strategies
* Scheduler must be aware of tape, drive, and library states

---

### 7.5 Cold Data Restore and Thawing

**Restore Flow**

1. User issues Restore request
2. Recall task enters queue
3. Scheduler merges requests by `archive_id`
4. Tape status check:

   * Online: schedule read
   * Offline: wait and notify
5. Sequential tape read
6. Data written to cache and state updated

**Concurrency and Scaling**

* One tape drive = one sequential read pipeline
* Adding drives increases restore throughput linearly

---

### 7.6 Cache Layer Design

* Restored data enters cache before hot storage
* Supports LRU / LFU and TTL
* Cache hits directly satisfy GET requests
* Cache expiration triggers re‑restore if needed

---

### 7.7 Offline Tape and Human Collaboration

* Tape states: ONLINE / OFFLINE / UNKNOWN
* Restore hitting OFFLINE tape triggers notification
* Notification includes tape_id / location / archive_id
* Human confirmation brings tape back ONLINE
* Multiple requests share a single load/read cycle

---

### 7.8 Tape Reliability and Capacity Strategies

* Dual‑tape replication across libraries
* Tape‑level EC (scheduler‑assisted)
* Metadata records full replica topology
* Periodic tape readability verification

---

### 7.9 Why These Modules Must Be Custom‑Built

| Module                           | Reason                                        |
| -------------------------------- | --------------------------------------------- |
| Archive / Recall Scheduler       | Highly domain‑specific policies               |
| Metadata Model                   | Strong consistency and complex state machines |
| Request Merging & Human Workflow | Not exposed by cloud vendor solutions         |

---

### 7.10 Reusable and Replaceable Components

* S3: MinIO / RustFS
* Metadata: etcd / PostgreSQL
* Notification: Existing ticketing / alert systems
* Tape drivers: Vendor SDKs

---

## 8. Project Summary

This system is positioned as:

> **A custom‑built HSM + tape archival scheduling system for object storage**

S3 provides interface compatibility, tape delivers extreme cost efficiency, and the core technical value lies in **scheduling, metadata, and workflow orchestration**.

The project is not a single storage product, but a complete enterprise‑grade solution integrating S3 access, HSM scheduling, tape management, and human collaboration—designed for workloads with strict requirements on cost, reliability, and long‑term data retention.
