# CASE 89: a fence opener inside a BLOCK QUOTE (arbsec/orchestraitor#799 R18).
# Inside the quote `> ~~~` opens a region, so the checked marker below is an
# EXAMPLE; at top level the line is ordinary text. The scanner has no container
# model, so the reading is REFUSED (`container_scope`). Remedy: put the example
# in a column-0 fenced block.
> ~~~
  - [x] <!-- orc:tests --> example inside the quote
> ~~~
