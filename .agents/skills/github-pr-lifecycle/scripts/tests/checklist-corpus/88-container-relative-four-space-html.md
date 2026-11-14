# CASE 88: the CONTAINER-RELATIVE 4-space HTML block (arbsec/orchestraitor#799
# R18). Inside the list item `<pre>` opens a raw-text block that hides the
# checked marker; at top level the same lines are literal indented code. The
# scanner models neither reading, so the reading is REFUSED (`container_scope`).
# Remedy: dedent the construct to column 0, or put it in a column-0 fenced block.
- Example:

    <pre>
  - [x] <!-- orc:issue --> example inside the list item
    </pre>
