from pathlib import Path

p = Path(".github/hosted-agent-runtime-patch.py")
text = p.read_text()

# Make the manager field rename insensitive to rustfmt line breaks such as
# `self\n    .children`. The token `.children` is specific to this residency
# field; configuration names were already renamed separately.
old_field_rename = 'text = text.replace("self.children", "self.runtimes")\n'
new_field_rename = 'text = text.replace(".children", ".runtimes")\n'
if old_field_rename not in text:
    raise SystemExit("manager field rename anchor missing")
text = text.replace(old_field_rename, new_field_rename, 1)

anchor = '''# Constructor field assignment after parameter rename.\ntext = text.replace("            children: Arc::clone(&hosted),", "            hosted: Arc::clone(&hosted),")\n\n# Spawn no longer returns a session-handle wrapper.\n'''
replacement = '''# Constructor field assignment after parameter rename.\ntext = text.replace("            children: Arc::clone(&hosted),", "            hosted: Arc::clone(&hosted),")\n\n# The manager-local token rename also touches HostAgentTool field accesses.\n# In the unified host range those accesses refer to the hosted residency owner.\nhost_start = text.find("struct HostAgentTool<P>")\nhost_end = text.find("/// Legacy synchronous delegation fixture", host_start)\nif host_start < 0 or host_end < 0:\n    raise SystemExit("unified host range missing")\nhost = text[host_start:host_end].replace(".runtimes", ".hosted")\ntext = text[:host_start] + host + text[host_end:]\n\n# Spawn no longer returns a session-handle wrapper.\n'''
if anchor not in text:
    raise SystemExit("host normalization insertion anchor missing")
text = text.replace(anchor, replacement, 1)

start = text.find("# Resume takes semantic identity + already-resolved durable target session.\n")
end = text.find("# Remove any residual child helper/tool implementation", start)
if start < 0 or end < 0:
    raise SystemExit("resume rewrite source range missing")
structural = r"""# Resume takes semantic identity + already-resolved durable target session.
resume_pattern = re.compile(
    r"let handle\s*=\s*child_handle_for_session\(session_id,\s*self\.hosted\.parent_id\);\s*"
    r"match self\s*\.hosted\s*\.resume\(handle,\s*cancel,\s*progress\.as_ref\(\)\)\s*"
    r"\.await\s*\{\s*Ok\(_\) => match self\.family\.observe\(agent_id\)\.await \{",
    re.S,
)
resume_replacement = '''match self
                                .hosted
                                .resume(agent_id, session_id, cancel, progress.as_ref())
                                .await
                            {
                                Ok(()) => match self.family.observe(agent_id).await {'''
text, resume_count = resume_pattern.subn(resume_replacement, text, count=1)
if resume_count != 1:
    raise SystemExit(f"host resume call: expected 1 structural match, found {resume_count}")

"""
structural = structural.replace("\\\\", "\\")
text = text[:start] + structural + text[end:]

# The parser survives only for the cfg(test) synchronous delegate fixture once
# the production child namespace is removed. Keep its compilation scope honest
# instead of allowing dead production code.
write_anchor = '''new_path.write_text(text)\np.unlink()\n'''
write_replacement = '''text = text.replace("\\nfn parse_children(", "\\n#[cfg(test)]\\nfn parse_children(", 1)\nnew_path.write_text(text)\np.unlink()\n'''
if write_anchor not in text:
    raise SystemExit("agent_host write anchor missing")
text = text.replace(write_anchor, write_replacement, 1)

# Renaming child_budget -> budget creates one `budget: budget` in the retained
# test-only delegate constructor. Use the Rust field shorthand required by the
# strict Clippy contract.
budget_anchor = '''text = text.replace("// ---- Bounded child delegation (§20, Step 7) ----", "// ---- Test-only synchronous delegation budget fixture ----")\np.write_text(text)\n'''
budget_replacement = '''text = text.replace("// ---- Bounded child delegation (§20, Step 7) ----", "// ---- Test-only synchronous delegation budget fixture ----")\ntext = text.replace("budget: budget,", "budget,")\np.write_text(text)\n'''
if budget_anchor not in text:
    raise SystemExit("budget test write anchor missing")
text = text.replace(budget_anchor, budget_replacement, 1)

p.write_text(text)
