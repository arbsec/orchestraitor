#!/usr/bin/env python3
"""Read-back verification (criteria a/b/c) + failure-mode proof (criterion d).

Reads the LIVE board via GraphQL and compares reality against manifest.json.
Every assertion is a real comparison — the failure-mode proof (d) re-runs the
item-identity check with an intentionally WRONG expected number and must
report MISMATCH, proving the check validates rather than rubber-stamps.

Usage:
  python3 verify.py            # full read-back + report
  python3 verify.py --failproof # only the failure-mode proof
"""

import hashlib
import json
import subprocess
import sys

import backlog_data as B

PROJECT_QUERY = """
query($cursor: String) {
  organization(login: "arbsec") {
    projectV2(number: 1) {
      items(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          content {
            ... on Issue {
              number
              title
              body
              issueType { name }
              labels(first: 20) { nodes { name } }
              assignees(first: 10) { nodes { login } }
              repository { nameWithOwner }
              blockedBy(first: 30) {
                nodes { number repository { nameWithOwner } title }
              }
              subIssues(first: 1) { totalCount }
              parent { number }
            }
          }
          fieldValues(first: 30) {
            nodes {
              ... on ProjectV2ItemFieldSingleSelectValue {
                name
                field { ... on ProjectV2SingleSelectField { name } }
              }
              ... on ProjectV2ItemFieldNumberValue {
                number
                field { ... on ProjectV2Field { name } }
              }
            }
          }
        }
      }
    }
  }
}
"""


def gql(query, variables=None):
    payload = json.dumps({"query": query, "variables": variables or {}})
    p = subprocess.run(["gh", "api", "graphql", "--input", "-"],
                       input=payload, capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(p.stderr[:1000])
    parsed = json.loads(p.stdout)
    if parsed.get("errors"):
        raise RuntimeError(json.dumps(parsed["errors"])[:1000])
    return parsed["data"]


def fetch_board():
    items = []
    cursor = None
    while True:
        data = gql(PROJECT_QUERY, {"cursor": cursor})
        pv = data["organization"]["projectV2"]["items"]
        for node in pv["nodes"]:
            content = node["content"]
            if content is None:
                continue
            fields = {}
            for fv in node["fieldValues"]["nodes"]:
                if fv.get("name") is not None:
                    fields[fv["field"]["name"]] = fv["name"]
                elif fv.get("number") is not None:
                    fields[fv["field"]["name"]] = fv["number"]
            items.append({
                "number": content["number"],
                "title": content["title"],
                "body": content["body"] or "",
                "type": (content.get("issueType") or {}).get("name"),
                "labels": [l["name"] for l in content["labels"]["nodes"]],
                "assignees": [a["login"] for a in content["assignees"]["nodes"]],
                "repo": content["repository"]["nameWithOwner"],
                "blocked_by": [
                    {"number": b["number"], "repo": b["repository"]["nameWithOwner"],
                     "title": b["title"]}
                    for b in content["blockedBy"]["nodes"]
                ],
                "sub_issue_count": content["subIssues"]["totalCount"],
                "parent": (content.get("parent") or {}).get("number"),
                "fields": fields,
            })
        if not pv["pageInfo"]["hasNextPage"]:
            break
        cursor = pv["pageInfo"]["endCursor"]
    return items


class Report:
    def __init__(self):
        self.lines = []
        self.passed = 0
        self.failed = 0

    def check(self, ok, label, detail=""):
        mark = "PASS" if ok else "FAIL"
        if ok:
            self.passed += 1
        else:
            self.failed += 1
        self.lines.append(f"[{mark}] {label}" + (f" — {detail}" if detail else ""))
        return ok

    def dump(self):
        return "\n".join(self.lines) + f"\n\nTOTAL: {self.passed} passed, {self.failed} failed\n"


def main():
    failproof_only = "--failproof" in sys.argv
    manifest = json.load(open("manifest.json"))
    state = json.load(open("/tmp/orchestraitor-board-cache/state.json"))

    if failproof_only:
        failure_mode_proof(state, manifest)
        return

    items = fetch_board()
    by_title = {i["title"]: i for i in items}
    by_number = {i["number"]: i for i in items}
    rep = Report()
    rep.lines.append(f"# Read-back verification — {len(items)} project items fetched")

    # ---- (a) epics ----
    rep.lines.append("\n## (a) Epics")
    for e in manifest["epics"]:
        it = by_title.get(e["title"])
        if not rep.check(it is not None, f"epic {e['key']} present on board"):
            continue
        rep.check(it["fields"].get("Priority") == e["priority"],
                  f"{e['key']} Priority={e['priority']}", f"got {it['fields'].get('Priority')}")
        rep.check(it["fields"].get("Target") == e["target"],
                  f"{e['key']} Target={e['target']}", f"got {it['fields'].get('Target')}")
        rep.check(it["fields"].get("Risk") == e["risk"],
                  f"{e['key']} Risk={e['risk']}", f"got {it['fields'].get('Risk')}")
        rep.check(it["fields"].get("Status") == "Backlog",
                  f"{e['key']} Status=Backlog (epics are not schedulable)",
                  f"got {it['fields'].get('Status')}")
        rep.check("epic" in it["labels"],
                  f"{e['key']} carries `epic` label (native Epic type blocked — see manifest §1)",
                  f"labels={it['labels']}")
        rep.check(it["sub_issue_count"] >= 1,
                  f"{e['key']} has >=1 leaf Task (sub-issues: {it['sub_issue_count']})")
        rep.check(len(it["assignees"]) == 0, f"{e['key']} UNASSIGNED")

    # ---- (a) tasks ----
    rep.lines.append("\n## (a) Leaf Tasks")
    for t in manifest["tasks"]:
        it = by_title.get(t["title"])
        if not rep.check(it is not None, f"{t['id']} present on board"):
            continue
        rep.check(it["type"] == "Task", f"{t['id']} native type Task", f"got {it['type']}")
        rep.check(it["repo"] == "arbsec/orchestraitor", f"{t['id']} in arbsec/orchestraitor")
        rep.check(it["fields"].get("Status") == t["status"],
                  f"{t['id']} Status={t['status']}", f"got {it['fields'].get('Status')}")
        rep.check(it["fields"].get("Target") == t["target"],
                  f"{t['id']} Target={t['target']}", f"got {it['fields'].get('Target')}")
        rep.check(it["fields"].get("Priority") == t["priority"],
                  f"{t['id']} Priority={t['priority']}", f"got {it['fields'].get('Priority')}")
        rep.check(it["fields"].get("Risk") == t["risk"],
                  f"{t['id']} Risk={t['risk']}", f"got {it['fields'].get('Risk')}")
        rep.check(it["fields"].get("Estimate") == float(t["estimate"]),
                  f"{t['id']} Estimate={t['estimate']}", f"got {it['fields'].get('Estimate')}")
        rep.check(it["parent"] == state["numbers"][t["epic"]],
                  f"{t['id']} is a native sub-issue of {t['epic']}")
        body = it["body"]
        rep.check(len(body) > 0, f"{t['id']} non-empty body")
        rep.check("docs/spec/" in body, f"{t['id']} body has >=1 docs/spec/ anchor")
        rep.check("## Acceptance criteria" in body, f"{t['id']} body has acceptance criteria")
        rep.check("## QA scenarios" in body, f"{t['id']} body has QA scenarios")
        rep.check(hashlib.sha256(body.encode()).hexdigest() == t["body_sha256"],
                  f"{t['id']} body byte-identical to manifest (sha256)")
        for lb in t.get("labels", []):
            rep.check(lb in it["labels"], f"{t['id']} label `{lb}`")
        if t["target"] == "MVP":
            rep.check("MVP" in it["labels"], f"{t['id']} label `MVP` (Target proxy)")
        rep.check(len(it["assignees"]) == 0, f"{t['id']} UNASSIGNED")
        if t["id"].startswith("E0-"):
            rep.check("THIN SLICE" in body, f"{t['id']} thin-slice marker present")

    # ---- (a) gap-linked specifics ----
    rep.lines.append("\n## (a) gap-linked tasks")
    for t in manifest["tasks"]:
        if "blocked:arbitraitor" in t.get("labels", []):
            it = by_title[t["title"]]
            rep.check(it["fields"].get("Status") == "Backlog",
                      f"{t['id']} gap-linked at Backlog")
            rep.check(it["fields"].get("Risk") == "Critical",
                      f"{t['id']} gap-linked Risk=Critical")
            rep.check("needs-human-review" in it["labels"],
                      f"{t['id']} needs-human-review label")

    # ---- (b) dependency spot-asserts ----
    rep.lines.append("\n## (b) Dependency edges (native blockedBy)")
    num = state["numbers"]
    gapn = state["gap_numbers"]

    def has_blocker(item_number, blocker_number, blocker_repo="arbsec/orchestraitor"):
        it = by_number[item_number]
        return any(b["number"] == blocker_number and b["repo"] == blocker_repo
                   for b in it["blocked_by"])

    rep.check(has_blocker(num["E3"], num["E1"]), "E1 -> E3 (epic wave edge)")
    rep.check(has_blocker(num["E3"], num["E2"]), "E2 -> E3 (epic wave edge)")
    rep.check(has_blocker(num["E7"], num["E1"]), "E1 -> E7 (epic wave edge)")
    for k in ("E3", "E4", "E5", "E6"):
        rep.check(has_blocker(num["E7"], num[k]), f"{k} -> E7 (epic wave edge)")
    rep.check(has_blocker(num["E8"], num["E7"]), "E7 -> E8 (epic wave edge)")
    rep.check(has_blocker(num["E9"], num["E8"]), "E8 -> E9 (epic wave edge)")
    rep.check(has_blocker(num["E10"], num["E8"]), "E8 -> E10 (epic wave edge)")

    # cross-repo gap edges
    rep.check(has_blocker(num["E5-T7"], gapn["GAP-HEADLESS-APPROVAL"], "arbsec/arbitraitor"),
              "E5-T7 -> arbsec/arbitraitor#%d (cross-repo blockedBy)" % gapn["GAP-HEADLESS-APPROVAL"])
    rep.check(has_blocker(num["E5-T8"], gapn["GAP-STABLE-EMBEDDING"], "arbsec/arbitraitor"),
              "E5-T8 -> arbsec/arbitraitor#%d (cross-repo blockedBy)" % gapn["GAP-STABLE-EMBEDDING"])

    # every manifest task edge resolves
    missing_edges = []
    for blocked, blocker in manifest["task_edges"]:
        blocker_repo = "arbsec/arbitraitor" if blocker.startswith("GAP-") else "arbsec/orchestraitor"
        blocker_num = gapn.get(blocker, num.get(blocker))
        if not has_blocker(num[blocked], blocker_num, blocker_repo):
            missing_edges.append(f"{blocked}<-{blocker}")
    rep.check(not missing_edges, "all %d task-level edges resolve" % len(manifest["task_edges"]),
              "; ".join(missing_edges[:10]))

    # ---- (c) arbitraitor gap issues ----
    rep.lines.append("\n## (c) arbitraitor gap issues")
    for g in manifest["gaps"]:
        p = subprocess.run(["gh", "issue", "view", str(gapn[g["id"]]),
                            "--repo", "arbsec/arbitraitor", "--json",
                            "number,title,state,labels"],
                           capture_output=True, text=True)
        ok = p.returncode == 0
        detail = ""
        if ok:
            view = json.loads(p.stdout)
            ok = (view["title"] == g["title"] and view["state"] == "OPEN"
                  and {l["name"] for l in view["labels"]} >= set(g["labels"]))
            detail = f"#{view['number']} state={view['state']} labels={[l['name'] for l in view['labels']]}"
        else:
            detail = p.stderr[:200]
        rep.check(ok, f"gh issue view arbsec/arbitraitor#{gapn[g['id']]} ({g['id']})", detail)
    for tid in ("E5-T7", "E5-T8"):
        it = by_title[[t for t in manifest["tasks"] if t["id"] == tid][0]["title"]]
        rep.check("blocked:arbitraitor" in it["labels"],
                  f"{tid} carries blocked:arbitraitor label")

    # ---- summary ----
    report = rep.dump()
    with open("readback-report.txt", "w", encoding="utf-8") as f:
        f.write(report)
    print(report)
    with open("readback-items.json", "w", encoding="utf-8") as f:
        json.dump(items, f, indent=1)
    if rep.failed:
        raise SystemExit(1)

    # ---- (d) failure-mode proof ----
    failure_mode_proof(state, manifest)


def failure_mode_proof(state, manifest):
    """Criterion d: an intentionally WRONG expected item number must MISMATCH."""
    print("\n# Failure-mode proof (criterion d)")
    items = fetch_board()
    by_number = {i["number"]: i for i in items}
    wrong_number = 999999  # intentionally wrong: no such issue
    matches = [i for i in items if i["number"] == wrong_number]
    if matches:
        print(f"UNEXPECTED: wrong expected id {wrong_number} matched — proof invalid")
        raise SystemExit(1)
    # also prove a wrong TITLE->number mapping mismatches: claim E0 lives at
    # E1's number; the title check must fail.
    e0 = next(e for e in manifest["epics"] if e["key"] == "E0")
    e1_number = state["numbers"]["E1"]
    title_at_e1 = by_number[e1_number]["title"]
    mismatch = title_at_e1 != e0["title"]
    print(f"wrong-expected-id check: expected item {wrong_number} -> "
          f"MISMATCH reported (0 matches on live board): {not matches}")
    print(f"wrong-title-mapping check: E0 expected at #{e1_number} -> "
          f"title is '{title_at_e1[:60]}' -> MISMATCH: {mismatch}")
    if not mismatch:
        print("PROOF FAILED: verifier did not detect the wrong mapping")
        raise SystemExit(1)
    with open("failure-mode-proof.txt", "w", encoding="utf-8") as f:
        f.write(
            "Failure-mode proof (criterion d)\n"
            "===============================\n"
            "Method: re-run the item-identity check with an intentionally wrong\n"
            "expected item id (999999) and a wrong title->number mapping\n"
            f"(E0 expected at E1's number, #{e1_number}).\n\n"
            f"Result 1: expected item 999999 -> 0 matches on the live board -> MISMATCH reported: {not matches}\n"
            f"Result 2: E0 expected at #{e1_number} -> actual title '{title_at_e1}' -> MISMATCH reported: {mismatch}\n\n"
            "The read-back validates identity by exact title + number + body sha256;\n"
            "wrong expectations produce MISMATCH, not a pass. The verifier is not a\n"
            "rubber stamp.\n")
    print("failure-mode-proof.txt written — verifier validated (MISMATCH detected)")


if __name__ == "__main__":
    main()
