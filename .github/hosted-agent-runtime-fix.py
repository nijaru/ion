from pathlib import Path

p = Path(".github/hosted-agent-runtime-patch.py")
text = p.read_text()
anchor = '''# Constructor field assignment after parameter rename.\ntext = text.replace("            children: Arc::clone(&hosted),", "            hosted: Arc::clone(&hosted),")\n\n# Spawn no longer returns a session-handle wrapper.\n'''
replacement = '''# Constructor field assignment after parameter rename.\ntext = text.replace("            children: Arc::clone(&hosted),", "            hosted: Arc::clone(&hosted),")\n\n# The earlier manager-local rename changes every `self.children` to\n# `self.runtimes`. Inside the unified host only, that receiver is the hosted\n# residency field rather than the manager's internal map.\nhost_start = text.find("struct HostAgentTool<P>")\nhost_end = text.find("/// Legacy synchronous delegation fixture", host_start)\nif host_start < 0 or host_end < 0:\n    raise SystemExit("unified host range missing")\nhost = text[host_start:host_end].replace("self.runtimes", "self.hosted")\ntext = text[:host_start] + host + text[host_end:]\n\n# Spawn no longer returns a session-handle wrapper.\n'''
if anchor not in text:
    raise SystemExit("host normalization insertion anchor missing")
p.write_text(text.replace(anchor, replacement, 1))
