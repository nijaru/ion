# Design Specs

Active design memory for the Go rewrite.

## Active

| Spec | Purpose |
| --- | --- |
| [Go Host Architecture](./go-host-architecture.md) | Bubble Tea host structure, transcript/composer/footer model |
| [Session Interface](./session-interface.md) | Canonical `AgentSession` interface and event model |
| [ACP Integration](./acp-integration.md) | Mapping ACP-backed external agents into the session boundary |
| [Native Ion Agent](./native-ion-agent.md) | Native ion runtime responsibilities behind the session boundary |
| [Memory and Context](./memory-and-context.md) | Durable memory, context shaping, compaction, and persistence |
| [Subagents, Swarms, and RLM](./subagents-swarms-rlm.md) | Advanced runtime orchestration patterns |
| [Rewrite Roadmap](./rewrite-roadmap.md) | Ordered implementation plan for the new mainline |

## Retained References

| Spec | Purpose |
| --- | --- |
| [Agent](./agent.md) | Earlier agent/runtime notes still useful as reference |
| [Config System](./config-system.md) | Configuration and layering concepts |
| [Compaction v2](./compaction-v2.md) | Context summarization and compaction design |
| [Permissions v2](./permissions-v2.md) | Permission model concepts |
| [Session Storage](./session-storage.md) | Persistence ideas that can inform the Go rewrite |
| [Plugin Architecture](./plugin-architecture.md) | Extensibility reference |
| [Tool Pass](./tool-pass.md) | Tool behavior and UX reference |

Historical Rust-TUI design material lives under `archive/rust/docs/ai/`.
