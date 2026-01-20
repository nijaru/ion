# TUI Coding Agents: SOTA Feature Analysis (2025)

**Research Date**: October 30, 2025
**Purpose**: Identify SOTA features from leading TUI agents to guide Aircher development

## Executive Summary

Analyzed 8 leading terminal-based coding agents to identify state-of-the-art features. Key findings:
- **LSP integration** is table stakes (OpenCode, Crush CLI)
- **Multi-session/parallel** execution emerging as critical (OpenCode, Zed)
- **MCP (Model Context Protocol)** gaining rapid adoption (Crush CLI, 12+ frameworks)
- **Voice-to-code** differentiator (Aider)
- **Watch mode** for IDE integration without leaving terminal (Aider)
- **Shareable sessions** for collaboration (OpenCode)
- **Privacy-first** architecture for enterprise (OpenCode, Cline)

## Agents Analyzed

### 1. OpenCode (26K⭐, 200K monthly users)

**Status**: Open source, production-validated, terminal-native

**Key Features**:
- ✅ **Native TUI**: Responsive, themeable terminal UI
- ✅ **LSP Integration**: Automatically loads appropriate LSPs for LLM context
- ✅ **Multi-Session**: Run multiple agents in parallel on same project
- ✅ **Shareable Sessions**: Share links to sessions for collaboration/debugging
- ✅ **Privacy-First**: No code storage, operates in sensitive environments
- ✅ **Claude Pro Integration**: Direct auth for Pro/Max accounts
- ✅ **75+ Providers**: Model flexibility via Models.dev
- ✅ **Any Editor**: Terminal-native works with any IDE

**Architecture**: Terminal-based, provider-agnostic, session-per-project

**Competitive Advantage**: Privacy + multi-session + LSP

---

### 2. Aider (Open source, established)

**Status**: Mature, widely adopted, git-native

**Key Features**:
- ✅ **Voice-to-Code**: Speak about code changes
- ✅ **Watch Mode**: Monitor files, respond to AI comments in IDEs
- ✅ **Multiple Chat Modes**: Code, architect, ask, help modes
- ✅ **Prompt Caching**: Cost savings and faster responses
- ✅ **Git Integration**: Tight integration with version control
- ✅ **Images & Web Pages**: Add visual context to chats
- ✅ **Browser Mode**: Runs in browser, not just CLI
- ✅ **Auto-Linting/Testing**: Automatically fixes lint and test errors
- ✅ **Repository Mapping**: Uses git structure for intelligent context
- ✅ **15+ LLM Providers**: OpenAI, Anthropic, Gemini, etc.

**Architecture**: Git-first, terminal-native, multi-modal input

**Competitive Advantage**: Voice input + watch mode + mature ecosystem

**Notable**: GitHub issue #1107 discussing LSP integration (not yet implemented as of Aug 2024)

---

### 3. Crush CLI (New, comprehensive)

**Status**: Production-ready, cross-platform, modern

**Key Features**:
- ✅ **Multi-Model**: OpenAI, Anthropic, Gemini, Groq, OpenRouter, Bedrock
- ✅ **Local Model Support**: Ollama, LM Studio via OpenAI-compatible APIs
- ✅ **Session-Based**: Per-project memory and context
- ✅ **LSP-Enhanced**: Semantics across languages for richer assistance
- ✅ **MCP Support**: First-class Model Context Protocol (http/stdio/sse)
- ✅ **Cross-Platform**: macOS, Linux, Windows (PowerShell/WSL)
- ✅ **Permissioned Tools**: Explicit approval with "yolo" escape hatch
- ✅ **Configuration**: Flexible config system

**Architecture**: Session-based, LSP-aware, MCP-native, cross-platform

**Competitive Advantage**: MCP integration + LSP + cross-platform

---

### 4. Claude Code (Anthropic, closed source)

**Status**: Production, rapidly evolving, enterprise-focused

**Key Features** (Sept-Oct 2025):
- ✅ **Checkpoints**: State save/restore before significant edits
- ✅ **Sandboxing**: 84% reduction in permission prompts (Docker-based)
- ✅ **Skills System**: Model-invoked modular capabilities (SKILL.md)
- ✅ **Subagents**: Dynamic selection, per-subagent model choices
- ✅ **Plan Mode**: Separate from execution mode
- ✅ **Budget Limits**: `--max-budget-usd` flag
- ✅ **VS Code Extension**: Native IDE integration
- ✅ **Bash Tool**: Direct command execution

**Architecture**: Agentic, checkpoint-based, sandboxed, multi-modal

**Competitive Advantage**: Sandboxing + skills + checkpoints

---

### 5. Cline (Privacy-focused)

**Status**: Open source, security-first, terminal-native

**Key Features**:
- ✅ **Data Privacy**: Offline/local usage capability
- ✅ **Custom Model Integration**: Self-hosted LLMs
- ✅ **Flexible Workflows**: Adapts to existing dev setups
- ✅ **Transparent Pricing**: Freedom from vendor lock-in
- ✅ **Real-Time Code Editing**: With security in mind
- ✅ **Terminal-Native**: Works directly in terminal

**Architecture**: Privacy-first, local-capable, security-focused

**Competitive Advantage**: Privacy + local models + security

---

### 6. Cursor CLI (Commercial, Oct 2025)

**Status**: Production, latest models, automation-focused

**Key Features**:
- ✅ **Latest Models**: GPT-5, Claude 4.5, Grok, Gemini - all cutting edge
- ✅ **Scripting/Automations**: Build custom coding agents
- ✅ **IDE Integration**: Works in preferred IDE
- ✅ **Real-Time Diffs**: Dedicated sidebar panel with inline diffs
- ✅ **Cross-Environment**: Same commands anywhere

**Architecture**: IDE-integrated, automation-capable, latest models

**Competitive Advantage**: Latest models + scripting + IDE integration

---

### 7. Zed Agentic (Open source, performance-focused)

**Status**: Production, 120fps native, multiplayer

**Key Features**:
- ✅ **120fps Native**: Incredibly fast rendering
- ✅ **Multiplayer**: Natively multiplayer IDE
- ✅ **Background Execution**: Agents run in background, notify when ready
- ✅ **Context Building**: Smart, well-designed context system
- ✅ **Amazon Bedrock**: AWS integration
- ✅ **Fast Review**: Excellent code review experience

**Architecture**: Native IDE, multiplayer-first, performance-optimized

**Competitive Advantage**: Speed + multiplayer + background execution

---

### 8. Goose (Alternative, mentioned in research)

**Status**: Open source, alternative approach

**Limited Information**: Mentioned alongside Aider and Cline as alternatives, but no detailed feature list found in research.

---

## Model Context Protocol (MCP) - Emerging Standard

**What is MCP?**
- "USB-C for AI" - standardized communication protocol
- Connects LLM applications to external data and tools
- Launched late 2024, becoming de facto standard by 2025

**Adoption**:
- ✅ OpenAI, Anthropic, Gemini, Vertex AI support
- ✅ 90% of organizations expected to adopt by end of 2025
- ✅ 12+ major agent SDKs with MCP support
- ✅ Servers from GitHub, AWS, ClickHouse, etc.

**What MCP Provides**:
- **Resources**: File-like data for context (API responses, file contents)
- **Tools**: Actions the LLM can take
- **Prompts**: Reusable prompt templates
- **Standardized Communication**: Unified way to interact with external systems

**Frameworks with MCP Support** (as of Oct 2025):
1. Claude Agent SDK (security-first)
2. OpenAI Agents SDK (delegation patterns)
3. CrewAI (multi-agent workflows)
4. LangChain (ecosystem breadth)
5. Agno (minimal code)
6. DSPy (prompt optimization)
7. + 6 more major frameworks

**Impact**: MCP is becoming table stakes for modern AI agents

---

## Comprehensive Feature Matrix

| Feature | Aircher | OpenCode | Aider | Crush CLI | Claude Code | Cline | Cursor | Zed |
|---------|---------|----------|-------|-----------|-------------|-------|--------|-----|
| **Core Features** | | | | | | | | |
| Terminal UI | ✅ TUI | ✅ Native | ✅ CLI | ✅ CLI | ✅ CLI | ✅ CLI | ✅ CLI | ❌ IDE |
| LSP Integration | ✅ (Issue 2) | ✅ Auto | ⚠️ Planned | ✅ Enhanced | ❌ | ❌ | ❌ | ✅ Native |
| Git Integration | ✅ Snapshots | ✅ | ✅ Native | ✅ | ✅ Checkpoints | ❌ | ❌ | ✅ |
| Multi-Session | ❌ | ✅ Parallel | ❌ | ✅ Per-project | ❌ | ❌ | ❌ | ✅ Multiplayer |
| **Memory & Context** | | | | | | | | |
| Episodic Memory | ✅ DuckDB | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Knowledge Graph | ✅ petgraph | ❌ | ⚠️ Repo map | ❌ | ❌ | ❌ | ❌ | ❌ |
| Working Memory | ✅ Dynamic | ❌ | ❌ | ✅ Session | ❌ | ❌ | ❌ | ✅ Context |
| Prompt Caching | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Agent Features** | | | | | | | | |
| Plan/Build Modes | ✅ (Week 7) | ❌ | ⚠️ Chat modes | ❌ | ✅ | ❌ | ❌ | ❌ |
| Subagents | ✅ Research | ❌ | ❌ | ❌ | ✅ Dynamic | ❌ | ❌ | ❌ |
| Skills System | ⚠️ Planned | ❌ | ❌ | ❌ | ✅ SKILL.md | ❌ | ❌ | ❌ |
| Checkpoints | ✅ Git | ❌ | ❌ | ❌ | ✅ State | ❌ | ❌ | ❌ |
| Sandboxing | ⚠️ Planned | ❌ | ❌ | ⚠️ Permissions | ✅ Docker | ❌ | ❌ | ❌ |
| **Advanced Features** | | | | | | | | |
| Voice-to-Code | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Watch Mode | ❌ | ❌ | ✅ IDE | ❌ | ❌ | ❌ | ❌ | ❌ |
| MCP Support | ❌ | ❌ | ❌ | ✅ First-class | ❌ | ❌ | ❌ | ❌ |
| Shareable Sessions | ❌ | ✅ Links | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ Multiplayer |
| Background Execution | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ Notify |
| **Model Support** | | | | | | | | |
| Multi-Provider | ✅ 4 | ✅ 75+ | ✅ 15+ | ✅ Many | ⚠️ Anthropic | ✅ Custom | ✅ All | ✅ Many |
| Local Models | ✅ Ollama | ✅ | ✅ | ✅ OpenAI-compat | ❌ | ✅ | ❌ | ❌ |
| Model Routing | ✅ Cost-aware | ❌ | ❌ | ✅ Switch | ⚠️ Per-subagent | ❌ | ✅ | ❌ |
| **Privacy & Security** | | | | | | | | |
| Privacy-First | ✅ | ✅ No storage | ❌ | ❌ | ❌ | ✅ Local | ❌ | ❌ |
| Permission System | ⚠️ Planned | ❌ | ❌ | ✅ Explicit | ✅ Sandbox | ❌ | ❌ | ❌ |
| Enterprise-Ready | ⚠️ | ✅ | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ |

---

## Key Insights & Trends

### 1. **LSP Integration is Table Stakes**
- OpenCode: Auto-loads appropriate LSPs
- Crush CLI: LSP-enhanced semantics
- Zed: Native LSP integration
- **Aircher**: ✅ Already have (Issue 2 complete)

### 2. **Multi-Session/Parallel Execution Emerging**
- OpenCode: Multiple agents on same project
- Zed: Multiplayer with 120fps
- **Aircher**: ❌ Gap - single session only

### 3. **MCP Becoming Essential**
- Crush CLI: First-class MCP support
- 12+ frameworks adopting
- 90% expected adoption by end of 2025
- **Aircher**: ❌ Major gap - no MCP support

### 4. **Memory Systems are Unique to Aircher**
- ✅ Episodic Memory (DuckDB)
- ✅ Knowledge Graph (petgraph)
- ✅ Working Memory (dynamic pruning)
- **Competitive Advantage**: Nobody else has this

### 5. **Sandboxing for Safety**
- Claude Code: 84% fewer permission prompts
- Crush CLI: Permission system with "yolo" mode
- **Aircher**: ⚠️ Planned but not implemented

### 6. **Skills System for Extensibility**
- Claude Code: SKILL.md with progressive loading
- **Aircher**: ⚠️ Planned but not implemented

### 7. **Voice/Multimodal Input**
- Aider: Voice-to-code
- Aider: Images & web pages
- **Aircher**: ❌ Text-only

### 8. **Watch Mode for IDE Integration**
- Aider: Monitor files, respond to comments
- **Aircher**: ❌ No IDE watch capability

### 9. **Shareable Sessions for Collaboration**
- OpenCode: Share session links
- Zed: Multiplayer editing
- **Aircher**: ❌ No sharing capability

### 10. **Background Execution**
- Zed: Agents run in background, notify when ready
- **Aircher**: ❌ Foreground only

---

## Priority Features for Aircher

Based on SOTA analysis and our current strengths:

### ✅ Already Have (Competitive)
1. ✅ LSP Integration (Issue 2 - complete)
2. ✅ Git Snapshots (Issue 4 - complete)
3. ✅ Memory Systems (unique advantage - complete)
4. ✅ Plan/Build Modes (Week 7 - complete)
5. ✅ Subagents for Research (Week 8 - complete)
6. ✅ Multi-Provider (4 providers)
7. ✅ Model Routing (cost-aware)

### 🎯 High Priority (Missing SOTA Features)
1. **MCP Support** (Emerging standard, 90% adoption expected)
   - Effort: MEDIUM (protocol integration)
   - Impact: HIGH (ecosystem compatibility)
   - Timeline: Week 10-11

2. **Skills System** (User extensibility)
   - Effort: MEDIUM (SKILL.md loading + discovery)
   - Impact: HIGH (community contributions)
   - Timeline: Week 10-11

3. **Multi-Session Support** (Parallel execution)
   - Effort: MEDIUM (session management)
   - Impact: MEDIUM (productivity boost)
   - Timeline: Week 11-12

### 🔮 Medium Priority (Nice to Have)
4. **Sandboxing** (Safety, 84% fewer prompts)
   - Effort: HIGH (Docker integration)
   - Impact: MEDIUM (UX improvement)
   - Timeline: Phase 4

5. **Watch Mode** (IDE integration)
   - Effort: MEDIUM (file watching + comment parsing)
   - Impact: MEDIUM (workflow integration)
   - Timeline: Phase 4

6. **Shareable Sessions** (Collaboration)
   - Effort: LOW (URL generation + state export)
   - Impact: LOW (niche use case)
   - Timeline: Phase 5

### ⏸️ Low Priority (Defer)
7. **Voice-to-Code** (Unique to Aider)
   - Effort: HIGH (speech recognition integration)
   - Impact: LOW (niche audience)
   - Timeline: Not planned

8. **Background Execution** (Zed feature)
   - Effort: MEDIUM (async execution + notifications)
   - Impact: LOW (terminal already async)
   - Timeline: Not planned

9. **Budget Limits** (Complex edge cases)
   - Effort: HIGH (many edge cases as you noted)
   - Impact: LOW (users can track manually)
   - Timeline: Not planned

---

## Competitive Positioning After SOTA Analysis

### Our Unique Strengths (Nobody Else Has):
1. ✅ **Three-Layer Memory System** (Episodic + Knowledge Graph + Working)
2. ✅ **Intent-Driven Strategy Selection** (UserIntent classification)
3. ✅ **Dynamic Context Pruning** (continuous work without restart)
4. ✅ **LSP Self-Correction** (real-time diagnostics feedback)
5. ✅ **Hybrid Architecture** (combining best patterns from 4 SOTA tools)

### Critical Gaps to Address:
1. ❌ **No MCP Support** (emerging standard)
2. ❌ **No Skills System** (user extensibility)
3. ❌ **Single Session Only** (vs OpenCode multi-session)
4. ❌ **No Sandboxing** (vs Claude Code 84% reduction)

### Strategic Recommendation:

**Phase 1 (Week 9 - NOW)**:
- ✅ Complete empirical validation
- ✅ Benchmark memory advantages
- ✅ Measure LSP self-correction impact

**Phase 2 (Week 10-11)**:
- 🎯 **MCP Support** - ecosystem compatibility (CRITICAL)
- 🎯 **Skills System** - user extensibility (HIGH VALUE)
- 📊 Research paper + open source release

**Phase 3 (Week 12+)**:
- Multi-session support (productivity)
- Sandboxing (if validation shows approval fatigue)
- Watch mode (IDE integration)

---

## Budget Limits: Why Postpone?

You're absolutely right about complexity. Edge cases include:

### Ollama Paid Subscriptions
- **Discovery**: Ollama now has paid tiers (I was unaware)
- **Impact**: Free/paid distinction complicates "local model = $0" assumption
- **Complexity**: Need to track Ollama usage separately

### Rate Limits vs Usage Limits
- **API Rate Limits**: 5-hour, 1-week windows per provider
- **Usage Budgets**: Daily/weekly/monthly spending caps
- **Complexity**: Different providers, different limit types

### Burst Usage
- **Problem**: User makes 10 requests in parallel
- **Question**: Estimate all 10 against budget? What if some fail?
- **Complexity**: Need request queueing with budget checks

### Multi-Session Tracking
- **Problem**: If we add multi-session, how to track budget across sessions?
- **Question**: Per-session budgets or global budget?
- **Complexity**: Requires session coordination

### Rollback/Refund Logic
- **Problem**: Request fails after budget deducted
- **Question**: Do we "refund" the estimated cost?
- **Complexity**: Actual vs estimated cost tracking

### Time-Window Resets
- **Problem**: Daily limit resets at midnight - which timezone?
- **Question**: UTC, user local, provider timezone?
- **Complexity**: Time-window management

### Concurrent Request Handling
- **Problem**: Two requests check budget simultaneously, both proceed
- **Question**: Race condition causing budget exceed?
- **Complexity**: Need atomic budget checks

### Estimation Accuracy
- **Problem**: We estimate tokens before call, actual usage differs
- **Question**: Block based on estimate or actual?
- **Complexity**: Over-blocking vs budget violations

**Conclusion**: Budget limits have **8+ major edge cases**. Let's postpone until after validation when we understand real usage patterns.

---

## Recommendations

### Immediate Actions (Week 9):
1. ✅ **Continue with empirical validation** - benchmark memory advantages
2. 📊 **Document competitive position** - we have unique strengths
3. 🔍 **Identify biggest user pain points** - validate vs features

### Next Phase (Week 10-11):
1. 🎯 **MCP Support** - emerging standard, ecosystem critical
2. 🎯 **Skills System** - enables community contributions
3. 📝 **Research paper** - document hybrid architecture advantages

### Future (Week 12+):
1. **Multi-session** - if validation shows need
2. **Sandboxing** - if approval fatigue is real problem
3. **Watch mode** - if IDE integration requested

**Bottom Line**: Focus on validating our unique advantages (memory systems, LSP, hybrid architecture) before adding more features. MCP and Skills are the only SOTA gaps that matter for ecosystem compatibility.
