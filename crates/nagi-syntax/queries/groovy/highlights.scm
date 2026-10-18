; Groovy（Gradle のビルドスクリプト・Jenkinsfile）の最小限の色分け。クレートに規則が入っていないので nagi が書いた
["abstract" "as" "assert" "break" "case" "catch" "class" "continue" "def" "default" "do" "else" "enum" "extends" "final" "finally" "for" "if" "implements" "import" "in" "instanceof" "interface" "new" "package" "private" "protected" "public" "record" "return" "static" "switch" "synchronized" "throw" "throws" "try" "while" "yield"] @keyword
["byte" "char" "double" "float" "int" "long" "short"] @type.builtin
(line_comment) @comment
(block_comment) @comment
(string_literal) @string
(null_literal) @constant.builtin
(type_identifier) @type
(annotation name: (identifier) @attribute)
(marker_annotation name: (identifier) @attribute)
(method_declaration name: (identifier) @function)
(function_definition name: (identifier) @function)
(method_invocation name: (identifier) @function)
