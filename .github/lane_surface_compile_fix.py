from pathlib import Path
import re


def regex_once(path: Path, pattern: str, replacement: str, label: str) -> None:
    text = path.read_text()
    rewritten, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    path.write_text(rewritten)


runtime = Path("crates/ion-core/src/runtime/mod.rs")

# The saturation test hook is deliberately a main-lane convenience, matching
# RuntimeHandle's single-session shutdown/control role.
text = runtime.read_text()
old = '''            .try_send(SessionCommand::SubmitIfIdle {
                prompt: String::from("fill"),
                reply,
            })'''
new = '''            .try_send(SessionCommand::SubmitIfIdle {
                lane_name: crate::session::lane::MAIN.to_owned(),
                prompt: String::from("fill"),
                reply,
            })'''
count = text.count(old)
if count != 1:
    raise SystemExit(f"saturated submit initializer: expected 1 match, found {count}")
runtime.write_text(text.replace(old, new, 1))

# Generic lane mutation/configuration now owns these paths. Remove singleton
# wrappers rather than retaining dead aliases that invite main-only coupling.
regex_once(
    runtime,
    r'''\n    fn main_lane_mut\(&mut self\) -> &mut crate::session::lane::Lane \{\n        self\.lane_mut\(crate::session::lane::MAIN\)\n            \.expect\("main lane exists while session runtime is live"\)\n    \}\n''',
    "\n",
    "remove main lane mut wrapper",
)
regex_once(
    runtime,
    r'''\n    fn main_lane_live_mut\(&mut self\) -> &mut LaneResidency \{\n        self\.lane_live_mut\(crate::session::lane::MAIN\)\n            \.expect\("main lane exists while session runtime is live"\)\n    \}\n''',
    "\n",
    "remove main lane live mut wrapper",
)
regex_once(
    runtime,
    r'''\n    fn main_pending_next_run\(&self\) -> Option<&crate::session::lane::NextRun> \{\n        self\.lane_pending_next_run\(crate::session::lane::MAIN\)\n    \}\n''',
    "\n",
    "remove main pending next-run wrapper",
)
