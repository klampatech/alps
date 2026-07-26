# ALPS — Agentic Loop Programming System

> **Status**: Draft v0.1 — type design + module layout. **Not yet implemented.**

ALPS is a four-step orchestrator that drives a high-stakes prompt from idea to verified, tested, shipped work. It composes Claude Code, Codex (via Ralph), and Hermes into a closed loop with adversarial review, structured receipts, and failure-driven replanning.

```
            ┌──────────────────────────┐
            │ Kyle (human)             │
            │ - initiate with prompt   │
            │ - verify receipts on exit│
            └────────────┬─────────────┘
                         │ prompt
                         ▼
            ┌──────────────────────────┐
            │  ALPS Outer Loop         │
            │  (while !done)           │
            └────────────┬─────────────┘
                         │
        ┌────────────────┼────────────────┐
        ▼                ▼                ▼
   ┌─────────┐   ┌─────────────┐   ┌──────────┐
   │ Plan    │──▶│ Implement   │──▶│ Review   │
   │ Claude  │   │ Ralph+Codex │   │ Claude   │
   └─────────┘   └─────────────┘   └────┬─────┘
        ▲                                │
        │ feedback                       ▼
        │                          ┌──────────┐
        └──────────────────────────│  Judge   │
                                   │  Hermes  │
                                   └─────┬────┘
                                         │ pass
                                         ▼
                                   ┌──────────┐
                                   │  Done    │
                                   │ receipts │
                                   └──────────┘
```

## Spec

- **[SPEC.md](SPEC.md)** — full design, type design, module layout, MVP vs scale

## Diagrams

- **[Happy path](docs/diagram-happy-path.html)** — outer loop runs once
- **[Rejection restart](docs/diagram-rejection-restart.html)** — judge rejects, feedback loop
- **[State machine](docs/diagram-state-machine.html)** — all states and transitions

## Layout

```
alps/
├── README.md
├── SPEC.md
├── docs/                  # HTML diagrams (open in browser)
├── alps-core/             # Rust library — type-state, agents, loop
├── alps-cli/              # Rust binary — CLI entry
└── tasks/                 # Per-task workspaces (git-tracked)
```

## Design principles

1. **Start simple, scale later.** MVP is single-task, file-system state, type-state in core.
2. **Git is the main history.** Each task is a subdirectory of `tasks/`. Every artifact is committed.
3. **Strict typing.** State machine encoded in the type system. Invalid transitions are compile errors.
4. **Ralph is a subprocess, not a library.** ALPS owns the outer loop; Ralph owns the inner implement loop.
5. **Strict separation of concerns.** Plan / Implement / Review / Judge are independent agents.
