# CASE 85: a fence opener CONTAINER-PREFIXED by a list marker (arbsec/orchestraitor#799
# R18). Inside the list item this `~~~` opens a region, so the checked marker
# below it is an EXAMPLE; at top level the whole line is ordinary text and the
# marker would be evidence. The scanner cannot tell which, so the reading is
# REFUSED (`container_scope`). Remedy: put the example in a column-0 fenced block.
- ~~~
  - [x] <!-- orc:tests --> example inside the list item
