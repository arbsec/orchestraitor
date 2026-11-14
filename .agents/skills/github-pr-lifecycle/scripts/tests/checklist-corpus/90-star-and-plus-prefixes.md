# CASE 90: `*` and `+` list markers are container prefixes too
# (arbsec/orchestraitor#799 R18) — all three bullet characters must refuse, not
# just `-`. The first construct (line 6) is the one reported; the checked marker
# between them is an example inside those list items.
* ~~~
+ ~~~
  - [x] <!-- orc:tests --> example inside a bullet list
