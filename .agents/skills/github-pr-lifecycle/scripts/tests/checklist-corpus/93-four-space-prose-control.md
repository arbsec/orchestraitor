# CASE 93: CONTROL — ordinary 4-space-indented PROSE carries no fence opener and
# no HTML construct, so it is literal indented code that hides nothing and must
# NOT refuse the reading (arbsec/orchestraitor#799 R18). The `<` inside the
# prose is not a line-start construct either. The real marker is evidence.
    ordinary indented prose, counted as code
    a < b and c > d
- [x] <!-- orc:issue --> real item, still evidence
