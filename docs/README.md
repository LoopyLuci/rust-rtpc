# Documentation

RTPC is documented across several files. The canonical overview is the
[`README.md` at the repo root](README.md); the detailed technical documents
live here.

| Document | What it covers |
|---|---|
| [README.md](../README.md) | Overview, features, quick start, architecture, contribution |
| [ARCHITECTURE.md](ARCHITECTURE.md) | System architecture, components, data flow, daemon lifecycle |
| [PROTOCOL.md](PROTOCOL.md) | Wire protocol (line-based JSON), ops, request/response shapes |
| [ENGINE.md](ENGINE.md) | `IncrementalEngine` trait, mock engine, Salsa engine (ra_ap backend) |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Dev workflow, F1 gate, module-add checklist, clippy/format rules |
| [ADR/0001-daemon-as-a-library.md](ADR/0001-daemon-as-a-library.md) | Architecture decision: daemon as a library with `DaemonService` |
