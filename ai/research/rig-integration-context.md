# Rig Framework Integration Strategy

> **Status**: Evaluation complete. Rig removed - not actively used. See `ai/DECISIONS.md`.

## Conclusion (2026-01-18)

**Decision**: Remove Rig entirely. Not actively used; MCP handles ecosystem interop.

| Component  | Decision | Rationale                     |
| ---------- | -------- | ----------------------------- |
| Agent Loop | Custom   | Plan-Act-Verify + OmenDB      |
| Providers  | Custom   | Working implementations exist |
| Tools      | Custom   | MCP handles ecosystem interop |
| MCP        | Custom   | `mcp-sdk-rs` complete         |

**Removed**: `rig-core` dependency and `src/rig_compat/` module.

---

## Original Evaluation Context (Archived)

Evaluated Rig for ecosystem compatibility. Key findings:

- **Complexity vs. Benefit**: Rig's advanced Rust (associated types, `schemars`) adds "framework tax"
- **We already have working code**: Providers, MCP, context assembly implemented and tested
- **MCP is sufficient**: Handles ecosystem tool interop without Rig dependency
