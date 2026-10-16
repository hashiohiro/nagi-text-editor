; Kotlin（tree-sitter-kotlin-ng 1.1.0）のハイライト。grammar に同梱が無いので Nagi で用意する。
; ノード名は grammar の src/node-types.json から。色数を絞るため、使うキャプチャは
; keyword / string / comment / type / number / constant / function / attribute / escape だけにする。

[
  "abstract" "actual" "annotation" "as" "by" "catch" "class" "companion" "const" "constructor"
  "crossinline" "data" "do" "else" "enum" "expect" "external" "final" "finally" "for" "fun"
  "get" "if" "import" "in" "infix" "init" "inline" "inner" "interface" "internal" "is"
  "lateinit" "noinline" "object" "open" "operator" "out" "override" "package" "private"
  "protected" "public" "return" "sealed" "set" "suspend" "tailrec" "throw" "try"
  "typealias" "val" "value" "var" "vararg" "when" "where" "while"
] @keyword

(this_expression) @keyword
(super_expression) @keyword

[(line_comment) (block_comment) (shebang)] @comment

[(string_literal) (multiline_string_literal) (character_literal)] @string
(escape_sequence) @escape
(interpolation) @punctuation

[(number_literal) (float_literal)] @number

(user_type (identifier) @type)
(class_declaration (identifier) @type)
(object_declaration (identifier) @type)
(type_alias (identifier) @type)

(function_declaration (identifier) @function)
(call_expression (identifier) @function)

(annotation) @attribute
(label) @attribute

((identifier) @constant.builtin
  (#any-of? @constant.builtin "true" "false" "null"))
