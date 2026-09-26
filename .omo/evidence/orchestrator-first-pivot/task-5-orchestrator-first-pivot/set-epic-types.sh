#!/usr/bin/env bash
# Remediation for the blocked native Epic issue type (see manifest.md §1).
# Owner steps: (1) gh auth refresh -s admin:org; (2) create the Epic type
#   in org arbsec (UI: Organization settings -> Issue types, or GraphQL
#   createIssueType); (3) run this script.
set -euo pipefail
gh issue edit 295 --repo arbsec/orchestraitor --add-type Epic  # E0
gh issue edit 296 --repo arbsec/orchestraitor --add-type Epic  # E1
gh issue edit 297 --repo arbsec/orchestraitor --add-type Epic  # E2
gh issue edit 298 --repo arbsec/orchestraitor --add-type Epic  # E3
gh issue edit 299 --repo arbsec/orchestraitor --add-type Epic  # E4
gh issue edit 300 --repo arbsec/orchestraitor --add-type Epic  # E5
gh issue edit 301 --repo arbsec/orchestraitor --add-type Epic  # E6
gh issue edit 302 --repo arbsec/orchestraitor --add-type Epic  # E7
gh issue edit 303 --repo arbsec/orchestraitor --add-type Epic  # E8
gh issue edit 304 --repo arbsec/orchestraitor --add-type Epic  # E9
gh issue edit 305 --repo arbsec/orchestraitor --add-type Epic  # E10
gh issue edit 306 --repo arbsec/orchestraitor --add-type Epic  # ICE
