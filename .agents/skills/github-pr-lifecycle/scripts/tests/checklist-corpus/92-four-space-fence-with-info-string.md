# CASE 92: a 4-space-indented fence opener carrying an INFO STRING, with
# ordinary text after it (arbsec/orchestraitor#799 R18). The indentation alone
# makes the scope indeterminate — literal indented code at top level, a real
# region inside a list item — so the reading is REFUSED (`container_scope`) even
# though the body carries no marker at all. Remedy: dedent the fence to column 0.
    ```sh
    echo "ordinary content, no checklist anywhere in this body"
    ```
