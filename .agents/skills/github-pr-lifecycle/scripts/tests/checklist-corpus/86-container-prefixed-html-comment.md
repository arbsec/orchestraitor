# CASE 86: an HTML construct CONTAINER-PREFIXED by a list marker
# (arbsec/orchestraitor#799 R18). Inside the list item `- <!--` opens a comment
# that hides everything up to the next `-->`, so the checked marker below is not
# visible evidence; at top level the line is ordinary text. The scanner models
# neither reading, so the reading is REFUSED (`container_scope`). Remedy: put the
# construct in a column-0 fenced block.
- <!--
  - [x] <!-- orc:tests --> example hidden by the comment
-->
