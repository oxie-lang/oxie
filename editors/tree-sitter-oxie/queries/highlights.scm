; oxie syntax highlighting. Later patterns win, so general rules come first.

(identifier) @variable
(type_identifier) @type
(constant) @constant
(primitive_type) @type.builtin
(lifetime) @label

; ------------------------------------------------------------------ keywords

(keyword) @keyword
(modifier) @keyword

[
  "fun" "fn" "class" "struct" "union" "enum" "interface" "trait" "object" "mod" "impl" "import" "use"
  "typealias" "test" "val" "var" "let" "init" "for" "where" "as" "from" "throws" "mut" "dyn"
  "data class" "error class" "sealed class" "value class" "shared class" "error enum" "companion object"
] @keyword

(self) @variable.builtin
(boolean) @boolean
(null) @constant.builtin

; ------------------------------------------------------------------ literals

(number) @number
(char_literal) @string
(string) @string
(multiline_string) @string
(raw_string) @string
(escape_sequence) @escape
(dollar) @string

; ------------------------------------------------------------------ comments

(line_comment) @comment
(block_comment) @comment
(doc_comment) @comment.doc

; ------------------------------------------------------------------ punctuation and operators

(operator) @operator
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
["," ";" ":" "::" "." "?."] @punctuation.delimiter

; ------------------------------------------------------------------ declarations and uses

(annotation "@" @attribute)
(annotation name: (annotation_name (identifier) @attribute))

(parameter_name (identifier) @variable.parameter)
; `own` before a parameter's name (a soft keyword: a parameter may be named `own`)
((parameters (identifier) @keyword . (parameter)) (#eq? @keyword "own"))
(property_name (identifier) @property)

(member_access name: (identifier) @property)
(member_access name: (type_identifier) @constructor)
(member_access name: (constant) @constant)

(call_expression function: (identifier) @function.call)
(call_expression function: (type_identifier) @constructor)
(method_call name: (identifier) @function.method)
(macro_invocation macro: (identifier) @function.macro)

(function_declaration name: (identifier) @function)
(class_declaration name: (type_identifier) @type)
(enum_declaration name: (type_identifier) @type)
(interface_declaration name: (type_identifier) @type)
(object_declaration name: (_) @type)
(type_alias name: (type_identifier) @type)
(enum_entry name: (type_identifier) @constructor)
(test_declaration name: (string) @string.special)

; ------------------------------------------------------------------ string templates

(interpolation "${" @punctuation.special "}" @punctuation.special)
(simple_interpolation) @variable.special

; Rust-style attributes: #[derive(Debug)], #![allow(dead_code)]
(rust_attribute "#" @attribute)
(rust_attribute (bracketed (identifier) @attribute))
(rust_attribute (bracketed (call_expression function: (identifier) @attribute)))
