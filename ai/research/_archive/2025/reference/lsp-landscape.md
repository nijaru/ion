# LSP Tooling Landscape (2025)

**Created**: 2025-11-20
**Purpose**: Evaluate best-in-class linters/type checkers for LSP integration

## Python

### Linting & Formatting
- **Ruff** ✅ (Current choice)
  - Rust-based, extremely fast (10-100x faster than alternatives)
  - Replaces: flake8, isort, pydocstyle, pyupgrade, autoflake
  - Built-in auto-fix for 700+ rules
  - **Verdict**: Keep for linting/formatting

### Type Checking
- **pyright** (Microsoft, powers VS Code Pylance)
  - Most accurate Python type checker
  - Full PEP 484+ support
  - Node.js-based (slower but comprehensive)

- **ty** ✅ (NEW - Ready for use per CLAUDE.md)
  - Drop-in pyright replacement
  - Faster than pyright
  - Compatible output format
  - Written in Rust
  - **Verdict**: Use ty for type checking (ruff doesn't do type analysis)

**Python Stack**: ruff (lint/format) + ty (types) + vulture (dead code)

## JavaScript/TypeScript

### Linting & Formatting
- **Biome** ✅ (Rust-based, stable)
  - Replaces Prettier + ESLint
  - Fast, single tool
  - Good VS Code integration
  - **Mature**: 1.0+ releases

- **oxc** (Rust-based, newer)
  - Faster than Biome (2-3x)
  - Less mature, evolving API
  - Used by Rspack/Rolldown projects
  - **Wait**: Not production-ready yet

**Verdict**: Biome for JS/TS (stable, proven)

### Type Checking
- **TypeScript (tsc)** ✅
  - Official type checker
  - No alternatives match feature parity
  - **Verdict**: Standard choice

**JS/TS Stack**: biome (lint/format) + tsc (types)

## Go

- **gopls** ✅ (Official Go LSP)
  - Built by Go team
  - Handles formatting (gofmt), linting, types
  - Single tool for everything
  - **Verdict**: Standard, use gopls

- **staticcheck** (Optional enhancement)
  - Advanced static analysis beyond `go vet`
  - Can supplement gopls
  - **Verdict**: Add in Phase 2 if needed

**Go Stack**: gopls (primary)

## Rust

- **rust-analyzer** ✅ (Official Rust LSP)
  - Built by Rust team
  - Handles everything (formatting, linting, types)
  - **Verdict**: Standard, use rust-analyzer

- **clippy** (Included via rust-analyzer)
  - Linting rules
  - Integrated with rust-analyzer
  - **Verdict**: Automatic via rust-analyzer

**Rust Stack**: rust-analyzer (includes clippy)

## Summary

| Language | Linting/Formatting | Type Checking | Dead Code |
|----------|-------------------|---------------|-----------|
| Python   | ruff              | ty            | vulture   |
| JS/TS    | biome             | tsc           | biome     |
| Go       | gopls             | gopls         | gopls     |
| Rust     | rust-analyzer     | rust-analyzer | clippy    |

## Implementation Priority (LSP Auto-Fix)

**Phase 1** (Current - Python):
1. Ruff for diagnostics, formatting, auto-fixes ✅
2. **Add ty for type checking** (new requirement)
3. Skip vulture for now (dead code less critical for LSP)

**Phase 2** (Future - Multi-language):
1. JS/TS: biome + tsc
2. Go: gopls
3. Rust: rust-analyzer

## Key Insight

**Ruff vs Type Checkers**: Ruff doesn't do type analysis (intentional design). For Python LSP, we need:
- Ruff: Syntax errors, imports, formatting, code style
- ty: Type errors, None checks, return type mismatches

**Both are necessary** for comprehensive Python validation.

## Updated Python LSP Design

```python
class PythonLSPAutoFix:
    def validate_and_fix(self, file_path: str, content: str) -> dict:
        # 1. Ruff diagnostics (syntax, imports, style)
        ruff_errors = self._get_ruff_diagnostics(file_path)

        # 2. Auto-format with ruff
        formatted = self._format_with_ruff(file_path)

        # 3. Apply ruff auto-fixes (imports, style)
        ruff_fixed = self._apply_ruff_fixes(file_path)

        # 4. Type check with ty (after ruff fixes)
        ty_errors = self._get_ty_diagnostics(file_path)

        # 5. Combine errors (ruff + ty)
        all_errors = ruff_errors + ty_errors

        return {
            "has_errors": bool(all_errors),
            "errors": all_errors,
            "auto_fixes_available": len(ruff_fixed) > 0,
            "fixed_content": ruff_fixed
        }
```

## Installation

```bash
# Python
uv add ruff vulture  # Already have ruff
uvx ty check .       # ty available via uvx

# JS/TS (future)
bun add -d @biomejs/biome typescript

# Go (future)
go install golang.org/x/tools/gopls@latest

# Rust (future)
# rust-analyzer installed via rustup
rustup component add rust-analyzer
```

## References

- Ruff: https://docs.astral.sh/ruff/
- ty: https://github.com/astral-sh/ty (Astral's new type checker)
- Biome: https://biomejs.dev/
- oxc: https://oxc.rs/
- gopls: https://github.com/golang/tools/tree/master/gopls
- rust-analyzer: https://rust-analyzer.github.io/
