# CASE 87: the CONTAINER-RELATIVE 4-space fence (arbsec/orchestraitor#799 R18).
# The blank line keeps the fence inside the list item, where it is a real code
# block and the marker below is an EXAMPLE. Dedented to column 0 the same text
# is literal indented code and the marker is evidence. The scanner cannot tell
# which, so the reading is REFUSED (`container_scope`). Remedy: dedent the
# construct to column 0, or put the example in a column-0 fenced block.
- Example:

    ~~~
  - [x] <!-- orc:issue --> example inside the list item
    ~~~
