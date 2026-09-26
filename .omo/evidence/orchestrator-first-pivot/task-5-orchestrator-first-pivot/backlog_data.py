#!/usr/bin/env python3
"""Assembler: full backlog model (epics, tasks, edges, gap issues).

Edge model:
- EPIC_EDGES: epic-level wave edges ("E1 -> E3" means epic E3 is blockedBy
  epic E1). Mirrors the reviewed wave structure: board/routing before tools;
  tools+worker+mediation before engine; engine before daemon; daemon before
  chat/hardening.
- Task-level intra-epic chains live in each task's `blocked_by` (task ids).
- Cross-repo Arbitraitor gap edges: E5-T7/E5-T8 blocked_by GAP-* ids.
"""

import bd_common
import bd_gaps
import bd_e0a
import bd_e0b
import bd_e1
import bd_e2
import bd_e3
import bd_e4
import bd_e5
import bd_e67
import bd_e8
import bd_e910

ORG = bd_common.ORG
PROJECT_NUMBER = bd_common.PROJECT_NUMBER
REPO = bd_common.REPO
SIBLING_REPO = bd_common.SIBLING_REPO
BUDGETS = bd_common.BUDGETS
BUDGET_BLOCK = bd_common.BUDGET_BLOCK

EPICS = bd_common.EPICS
GAP_ISSUES = bd_gaps.GAP_ISSUES

TASKS = (
    bd_e0a.TASKS
    + bd_e0b.TASKS
    + bd_e1.TASKS
    + bd_e2.TASKS
    + bd_e3.TASKS
    + bd_e4.TASKS
    + bd_e5.TASKS
    + bd_e67.TASKS
    + bd_e8.TASKS
    + bd_e910.TASKS
)

# Epic-level wave edges: (blocked_epic_key, blocker_epic_key)
EPIC_EDGES = [
    ("E3", "E1"),
    ("E3", "E2"),
    ("E7", "E1"),
    ("E7", "E3"),
    ("E7", "E4"),
    ("E7", "E5"),
    ("E7", "E6"),
    ("E8", "E7"),
    ("E9", "E8"),
    ("E10", "E8"),
]

EPIC_BY_KEY = {e["key"]: e for e in EPICS}
TASK_BY_ID = {t["id"]: t for t in TASKS}
GAP_BY_ID = {g["id"]: g for g in GAP_ISSUES}


def task_defaults(t):
    """Fill derived defaults: status, target, priority inherit the epic."""
    d = dict(t)
    epic = EPIC_BY_KEY[d["epic"]]
    d.setdefault("status", "Backlog" if d["epic"] == "ICE" else "Ready")
    d.setdefault("target", epic["target"])
    d.setdefault("priority", epic["priority"])
    return d


def all_task_edges():
    """Yield (blocked_task_id, blocker_ref) for every task-level edge."""
    for t in TASKS:
        for blocker in t.get("blocked_by", []):
            yield (t["id"], blocker)


def validate():
    errors = []
    ids = [t["id"] for t in TASKS]
    if len(ids) != len(set(ids)):
        errors.append("duplicate task ids")
    for t in TASKS:
        if t["epic"] not in EPIC_BY_KEY:
            errors.append(f"{t['id']}: unknown epic {t['epic']}")
        for blocker in t.get("blocked_by", []):
            if blocker not in TASK_BY_ID and blocker not in GAP_BY_ID:
                errors.append(f"{t['id']}: unknown blocker {blocker}")
        if t.get("status", "Ready") == "Ready" and any(
            b in GAP_BY_ID for b in t.get("blocked_by", [])
        ):
            errors.append(f"{t['id']}: gap-linked task must be Backlog")
    for blocked, blocker in EPIC_EDGES:
        if blocked not in EPIC_BY_KEY or blocker not in EPIC_BY_KEY:
            errors.append(f"bad epic edge {blocker}->{blocked}")
    # every epic has >= 1 leaf task
    for e in EPICS:
        if not any(t["epic"] == e["key"] for t in TASKS):
            errors.append(f"epic {e['key']} has no leaf tasks")
    # DoR body checks: non-empty, spec anchor, AC + QA sections
    for t in TASKS:
        body = t["body"]
        if "docs/spec/" not in body:
            errors.append(f"{t['id']}: body lacks docs/spec/ anchor")
        if "## Acceptance criteria" not in body:
            errors.append(f"{t['id']}: body lacks acceptance criteria")
        if "## QA scenarios" not in body:
            errors.append(f"{t['id']}: body lacks QA scenarios")
    for e in EPICS:
        if "Focus:" not in e["body"]:
            errors.append(f"epic {e['key']}: body lacks Focus: hint line")
    return errors


if __name__ == "__main__":
    errs = validate()
    if errs:
        for e in errs:
            print("ERROR:", e)
        raise SystemExit(1)
    n_tasks = len(TASKS)
    n_edges = len(list(all_task_edges())) + len(EPIC_EDGES)
    print(f"OK: {len(EPICS)} epics, {n_tasks} tasks, {n_edges} edges, {len(GAP_ISSUES)} gap issues")
    for e in EPICS:
        leaves = [t["id"] for t in TASKS if t["epic"] == e["key"]]
        print(f"  {e['key']:4s} P={e['priority']} risk={e['risk']:8s} target={e['target']:7s} leaves={len(leaves)}")
