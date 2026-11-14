# CASE 19: a TAB-led fence opener reaches the 4-column tab stop, so it is a
# CONTAINER-SCOPE construct: literal indented code at top level, a real region
# inside a list item, and the scanner cannot tell which (arbsec/orchestraitor#799
# R18). The reading is REFUSED (`container_scope`); the marker below still counts.
	```
- [x] <!-- orc:tests --> real item (a tab reaches the 4-column tab stop)
```
