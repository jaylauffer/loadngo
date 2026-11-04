# The control for this round's loss count on the canvas's record: rewrites a copy of js/web3d.js so
# the count is kept per realm controller again, as before probe NV found the bug. Run on a copy of
# pkg/web3d.js in a directory outside the repository, then serve today's probe page with that pkg/:
#   python3 losses-per-realm.py <dir>/web3d.js
#   PLAYWRIGHT_CORE=... node ../../critic11-rerun/tools/against.mjs <port> <dir> demo/probe.html \
#     probes-losses-per-realm.json --isolated --label "..."
# probes-losses-per-realm.json holds the result: 80 of 80 probes pass, and the page logs 18 WebGL
# INVALID_OPERATION warnings "object does not belong to this context" (check.mjs fails on any), from
# NV's navigated-away renderer disposed after this window lost and restored its canvas.
# BSD-3-Clause; see the repository's LICENSE.
import pathlib
import sys

p = pathlib.Path(sys.argv[1])
s = p.read_text()
reps = [
    ("  const create = { armedBefore: controller.armed, losses: record.losses, lost: false };",
     "  const create = { armedBefore: controller.armed, losses: controller.losses, lost: false };"),
    ("    throw new Error(create.armedBefore || record.losses !== create.losses ? LOST_AT_CREATE : LOST_BEFORE_WEB3D);",
     "    throw new Error(create.armedBefore || controller.losses !== create.losses ? LOST_AT_CREATE : LOST_BEFORE_WEB3D);"),
    ("  const losses = record.losses;\n  shim.bind(gl, () => record.losses !== losses);",
     "  const losses = controller.losses;\n  shim.bind(gl, () => controller.losses !== losses);"),
    ("  c = { current: null, waiting: new Set(), armed: false, ref: null, lostListener: null, restoredListener: null };",
     "  c = { losses: 0, current: null, waiting: new Set(), armed: false, ref: null, lostListener: null, restoredListener: null };"),
    ("    record.losses++;", "    c.losses++;"),
]
for old, new in reps:
    assert s.count(old) == 1, old[:60]
    s = s.replace(old, new)
p.write_text(s)
print("the loss count is per realm controller again")
