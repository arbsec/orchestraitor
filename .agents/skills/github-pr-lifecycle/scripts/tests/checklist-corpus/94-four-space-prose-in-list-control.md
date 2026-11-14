# CASE 94: CONTROL — 4-space-indented prose inside a list item still carries no
# construct, so it does not refuse the reading (arbsec/orchestraitor#799 R18):
# only a FENCE OPENER or an HTML construct at an indeterminate scope does. The
# real marker is evidence.
- Example:

    this paragraph is prose, not a construct

- [x] <!-- orc:tests --> real item, still evidence
