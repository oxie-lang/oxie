; `fun main` runs the program
((function_declaration
  name: (identifier) @_name) @run
  (#eq? @_name "main")
  (#set! tag oxie-main))

; `test "name" { }` runs one test
((test_declaration
  name: (string (string_content) @OXIE_TEST_NAME)) @run
  (#set! tag oxie-test))
