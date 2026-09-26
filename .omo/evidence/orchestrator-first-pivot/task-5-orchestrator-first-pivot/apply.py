#!/usr/bin/env python3
"""Apply the backlog manifest to the live board (arbsec project 1 + 2 gap issues).

Discipline:
- Every mutation is logged (name, inputs w/ node IDs, duration, result) to
  mutation-log.jsonl in the evidence dir (node IDs REDACTED there; the raw
  log + state.json with real IDs live ONLY in /tmp/orchestraitor-board-cache/).
- A failed call aborts the run; state.json makes the run resumable
  (--resume skips already-created items).
- Sequential calls with a small pause (secondary rate limits).
"""

import json
import os
import re
import subprocess
import sys
import time

import backlog_data as B

CACHE = "/tmp/orchestraitor-board-cache"
EVIDENCE = os.path.dirname(os.path.abspath(__file__))
STATE_PATH = os.path.join(CACHE, "state.json")
RAW_LOG_PATH = os.path.join(CACHE, "mutation-log-raw.jsonl")
REDACTED_LOG_PATH = os.path.join(EVIDENCE, "mutation-log.jsonl")
PAUSE_S = 0.25

NODE_ID_RE = re.compile(r"\b[A-Z][A-Za-z0-9]{0,24}_[A-Za-z0-9]{16,}\b")


def redact(obj):
    if isinstance(obj, dict):
        return {
            k: ("<redacted>" if k.endswith("id") or k.endswith("Id") else redact(v))
            for k, v in obj.items()
        }
    if isinstance(obj, list):
        return [redact(v) for v in obj]
    if isinstance(obj, str):
        return NODE_ID_RE.sub("<node-id>", obj)
    return obj


class Log:
    def __init__(self):
        self.raw = open(RAW_LOG_PATH, "a", encoding="utf-8")
        self.redacted = open(REDACTED_LOG_PATH, "a", encoding="utf-8")

    def entry(self, **kw):
        rec = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"), **kw}
        self.raw.write(json.dumps(rec) + "\n")
        self.raw.flush()
        self.redacted.write(json.dumps(redact(rec)) + "\n")
        self.redacted.flush()

    def close(self):
        self.raw.close()
        self.redacted.close()


LOG = None  # initialized in main()


def run(cmd, check=True):
    t0 = time.monotonic()
    p = subprocess.run(cmd, capture_output=True, text=True)
    dur = round(time.monotonic() - t0, 3)
    ok = p.returncode == 0
    LOG.entry(
        op="cmd",
        cmd=[c for c in cmd if not NODE_ID_RE.match(c)],
        duration_s=dur,
        ok=ok,
        stdout=p.stdout.strip()[:500] if ok else p.stdout.strip()[:2000],
        stderr=p.stderr.strip()[:2000] if not ok else p.stderr.strip()[:500],
    )
    if not ok and check:
        raise RuntimeError(
            f"command failed ({dur}s): {' '.join(cmd[:6])}...\n{p.stderr[:2000]}"
        )
    if dur > 30:
        LOG.entry(op="note", note=f"SLOW CALL {dur}s (hung_or_long_commands probe)")
    time.sleep(PAUSE_S)
    return p.stdout.strip()


def gql(query, variables, check=True):
    t0 = time.monotonic()
    payload = json.dumps({"query": query, "variables": variables})
    p = subprocess.run(
        ["gh", "api", "graphql", "--input", "-"],
        input=payload, capture_output=True, text=True,
    )
    dur = round(time.monotonic() - t0, 3)
    ok = p.returncode == 0
    data = None
    err = ""
    if ok:
        try:
            parsed = json.loads(p.stdout)
            if parsed.get("errors"):
                ok = False
                err = json.dumps(parsed["errors"])[:2000]
            else:
                data = parsed["data"]
        except json.JSONDecodeError:
            ok = False
            err = "non-json response: " + p.stdout[:500]
    if not ok:
        err = err or p.stderr[:2000]
    name = query.strip().split("\n")[1].strip().split("(")[0]
    LOG.entry(op="graphql", mutation=name, variables=variables,
              duration_s=dur, ok=ok, error=err or None)
    if not ok and check:
        raise RuntimeError(f"graphql failed ({dur}s): {err}")
    if dur > 30:
        LOG.entry(op="note", note=f"SLOW CALL {dur}s (hung_or_long_commands probe)")
    time.sleep(PAUSE_S)
    return data


def load_state():
    if os.path.exists(STATE_PATH):
        with open(STATE_PATH, encoding="utf-8") as f:
            return json.load(f)
    return {"ids": {}, "numbers": {}, "gap_numbers": {}, "items": {}, "edges_done": []}


def save_state(st):
    with open(STATE_PATH, "w", encoding="utf-8") as f:
        json.dump(st, f, indent=1)


Q_ADD_ITEM = """
mutation AddItem($projectId: ID!, $contentId: ID!) {
  addProjectV2ItemById(input: {projectId: $projectId, contentId: $contentId}) {
    item { id }
  }
}
"""

Q_SET_FIELD = """
mutation SetField($projectId: ID!, $itemId: ID!, $fieldId: ID!, $value: ProjectV2FieldValue!) {
  updateProjectV2ItemFieldValue(input: {
    projectId: $projectId, itemId: $itemId, fieldId: $fieldId, value: $value
  }) { projectV2Item { id } }
}
"""

Q_ADD_BLOCKED_BY = """
mutation AddBlockedBy($issueId: ID!, $blockingIssueId: ID!) {
  addBlockedBy(input: {issueId: $issueId, blockingIssueId: $blockingIssueId}) {
    issue { number }
    blockingIssue { number }
  }
}
"""

Q_CREATE_ISSUE_TYPE = """
mutation CreateEpicType($ownerId: ID!) {
  createIssueType(input: {ownerId: $ownerId, name: "Epic", isEnabled: true, color: PURPLE}) {
    issueType { id name }
  }
}
"""


def find_issue_by_title(repo, title):
    p = run(["gh", "issue", "list", "--repo", repo, "--state", "open",
             "--search", f'in:title "{title}"', "--limit", "5",
             "--json", "number,title"], check=False)
    try:
        items = json.loads(p) if p else []
    except json.JSONDecodeError:
        items = []
    for it in items:
        if it["title"] == title:
            return it["number"]
    return None


def write_body(body):
    path = os.path.join(CACHE, "body.md")
    with open(path, "w", encoding="utf-8") as f:
        f.write(body)
    return path


def create_issue(repo, title, body, issue_type=None, parent=None, labels=None):
    """Create (or reuse by exact title) an issue; return (number, created)."""
    existing = find_issue_by_title(repo, title)
    if existing is not None:
        LOG.entry(op="reuse", repo=repo, number=existing, title=title[:100])
        return existing, False
    cmd = ["gh", "issue", "create", "--repo", repo,
           "--title", title, "--body-file", write_body(body)]
    if issue_type:
        cmd += ["--type", issue_type]
    if parent:
        cmd += ["--parent", str(parent)]
    for lb in labels or []:
        cmd += ["--label", lb]
    url = run(cmd)
    return int(url.rstrip("/").split("/")[-1]), True


def issue_node_id(repo, number):
    p = run(["gh", "issue", "view", str(number), "--repo", repo, "--json", "id"])
    return json.loads(p)["id"]


def sel(F, field, option):
    return single_select(F[field]["id"], F[field]["options"][option])


def single_select(_field_id, option_id):
    return {"singleSelectOptionId": option_id}


def set_field(project_id, item_id, field_id, value):
    gql(Q_SET_FIELD, {"projectId": project_id, "itemId": item_id,
                      "fieldId": field_id, "value": value})


def main():
    global LOG
    resume = "--resume" in sys.argv
    os.makedirs(CACHE, exist_ok=True)
    LOG = Log()
    st = load_state()
    if st["numbers"] and not resume:
        print("state.json exists (previous run?). Re-run with --resume to continue, "
              "or delete it to start fresh.")
        raise SystemExit(2)

    # ---- 1. resolve ids (from the pre-populated cache) ----
    with open(os.path.join(CACHE, "project-fields.json"), encoding="utf-8") as f:
        fields = json.load(f)["data"]["node"]["fields"]["nodes"]
    F = {}
    for fld in fields:
        if fld.get("options") is not None:
            F[fld["name"]] = {"id": fld["id"],
                              "options": {o["name"]: o["id"] for o in fld["options"]}}
        else:
            F[fld["name"]] = {"id": fld["id"]}
    with open(os.path.join(CACHE, "org-project.json"), encoding="utf-8") as f:
        orgproj = json.load(f)["data"]["organization"]
    ORG_ID = orgproj["id"]
    PROJECT_ID = orgproj["projectV2"]["id"]
    types = {t["name"]: t["id"] for t in orgproj["issueTypes"]["nodes"]}
    LOG.entry(op="resolved", fields=sorted(F), types=sorted(types))

    # ---- 2. prerequisite: Epic issue type — BLOCKED, fallback to `epic` label ----
    # createIssueType requires admin:org/user scope; the owner token lacks it and
    # scope elevation is an interactive owner action. Epics carry the `epic` label
    # (the board's current type dimension per issue #252) instead; the native
    # type can be applied later via set-epic-types.sh (generated at the end).
    if "Epic" in types:
        LOG.entry(op="note", note="Epic issue type already present")
    else:
        LOG.entry(op="blocked-prerequisite", note=(
            "org-level Epic issue type missing and createIssueType requires "
            "admin:org scope (token has gist/project/read:org/repo/workflow); "
            "fallback: epic label on the 12 epics; remediation: set-epic-types.sh "
            "after the owner creates the type (requested by arbsec/orchestraitor#252)"))

    # ---- 3. prerequisite: needs-human-review label ----
    have = {l["name"] for l in json.loads(
        run(["gh", "label", "list", "--repo", B.REPO, "--limit", "200", "--json", "name"]))}
    if "needs-human-review" not in have:
        run(["gh", "label", "create", "needs-human-review", "--repo", B.REPO,
             "--color", "d73a4a",
             "--description", "security-sensitive; requires human sign-off"])
        LOG.entry(op="deviation", note=(
            "created repo label needs-human-review (documented board label in "
            "github-project.example.toml; required on Critical guard tasks)"))
    else:
        LOG.entry(op="note", note="needs-human-review label already present")

    # ---- 4. gap issues (arbsec/arbitraitor) ----
    for g in B.GAP_ISSUES:
        if g["id"] in st["gap_numbers"]:
            continue
        num, created = create_issue(g["repo"], g["title"], g["body"], labels=g["labels"])
        st["gap_numbers"][g["id"]] = num
        st["ids"][g["id"]] = issue_node_id(g["repo"], num)
        save_state(st)
        LOG.entry(op="gap-issue", gap=g["id"], repo=g["repo"], number=num, created=created)

    # ---- 5. epics ----
    for e in B.EPICS:
        if e["key"] in st["numbers"]:
            continue
        num, created = create_issue(B.REPO, e["title"], e["body"], labels=["epic"])
        node = issue_node_id(B.REPO, num)
        item = gql(Q_ADD_ITEM, {"projectId": PROJECT_ID, "contentId": node})[
            "addProjectV2ItemById"]["item"]["id"]
        set_field(PROJECT_ID, item, F["Status"]["id"], sel(F, "Status", "Backlog"))
        set_field(PROJECT_ID, item, F["Target"]["id"], sel(F, "Target", e["target"]))
        set_field(PROJECT_ID, item, F["Priority"]["id"], sel(F, "Priority", e["priority"]))
        set_field(PROJECT_ID, item, F["Risk"]["id"], sel(F, "Risk", e["risk"]))
        st["numbers"][e["key"]] = num
        st["ids"][e["key"]] = node
        st["items"][e["key"]] = item
        save_state(st)
        LOG.entry(op="epic", key=e["key"], number=num, created=created)

    # ---- 6. tasks ----
    for t in B.TASKS:
        if t["id"] in st["numbers"]:
            continue
        d = B.task_defaults(t)
        labels = list(d.get("labels", []))
        if d["target"] == "MVP" and "MVP" not in labels:
            labels.append("MVP")
        num, created = create_issue(B.REPO, d["title"], d["body"], issue_type="Task",
                                    parent=st["numbers"][d["epic"]], labels=labels)
        node = issue_node_id(B.REPO, num)
        item = gql(Q_ADD_ITEM, {"projectId": PROJECT_ID, "contentId": node})[
            "addProjectV2ItemById"]["item"]["id"]
        set_field(PROJECT_ID, item, F["Status"]["id"], sel(F, "Status", d["status"]))
        set_field(PROJECT_ID, item, F["Target"]["id"], sel(F, "Target", d["target"]))
        set_field(PROJECT_ID, item, F["Priority"]["id"], sel(F, "Priority", d["priority"]))
        set_field(PROJECT_ID, item, F["Risk"]["id"], sel(F, "Risk", d["risk"]))
        set_field(PROJECT_ID, item, F["Estimate"]["id"], {"number": float(d["estimate"])})
        st["numbers"][t["id"]] = num
        st["ids"][t["id"]] = node
        st["items"][t["id"]] = item
        save_state(st)
        LOG.entry(op="task", id=t["id"], number=num, created=created)

    # ---- 7. edges ----
    def edge(blocked_key, blocker_key, kind):
        tag = f"{blocked_key}<-{blocker_key}"
        if tag in st["edges_done"]:
            return
        gql(Q_ADD_BLOCKED_BY, {"issueId": st["ids"][blocked_key],
                               "blockingIssueId": st["ids"][blocker_key]})
        st["edges_done"].append(tag)
        save_state(st)
        LOG.entry(op="edge", kind=kind, blocked=blocked_key, blocker=blocker_key)

    for blocked, blocker in B.EPIC_EDGES:
        edge(blocked, blocker, "epic-wave")
    for blocked, blocker in B.all_task_edges():
        edge(blocked, blocker, "task")
    LOG.entry(op="done", epics=len(B.EPICS), tasks=len(B.TASKS),
              edges=len(st["edges_done"]), gaps=len(B.GAP_ISSUES))

    # ---- 8. remediation helper for the blocked Epic native type ----
    with open(os.path.join(EVIDENCE, "set-epic-types.sh"), "w", encoding="utf-8") as f:
        f.write("#!/usr/bin/env bash\n")
        f.write("# Remediation for the blocked native Epic issue type (see manifest.md §1).\n")
        f.write("# Owner steps: (1) gh auth refresh -s admin:org; (2) create the Epic type\n")
        f.write("#   in org arbsec (UI: Organization settings -> Issue types, or GraphQL\n")
        f.write("#   createIssueType); (3) run this script.\n")
        f.write("set -euo pipefail\n")
        for e in B.EPICS:
            if e["key"] in st["numbers"]:
                f.write(f'gh issue edit {st["numbers"][e["key"]]} '
                        f'--repo arbsec/orchestraitor --add-type Epic  # {e["key"]}\n')
    print("APPLY COMPLETE")
    print(json.dumps({"epics": len(B.EPICS), "tasks": len(B.TASKS),
                      "edges": len(st["edges_done"]), "gaps": st["gap_numbers"]},
                     indent=1))


if __name__ == "__main__":
    main()
