# CASE 16: a 4-space-indented fence opener is a CONTAINER-SCOPE construct
# (arbsec/orchestraitor#799 R18). At top level this line is literal indented
# code and the marker below is real evidence; inside a list item it opens a
# region and the marker below is an EXAMPLE. The scanner has no container model,
# the two readings disagree, so the reading is REFUSED — the marker still counts
# (the count is a pure function of the grammar) but no verdict is derived from
# it. Remedy: dedent the opener to column 0.
    ```text
- [x] <!-- orc:tests --> real item (only if this is top-level indented code)
