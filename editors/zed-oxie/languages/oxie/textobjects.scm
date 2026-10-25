(function_declaration) @function.around
(function_declaration
  body: (block "{" (_)* @function.inside "}"))

(class_declaration) @class.around
(class_declaration
  body: (class_body "{" (_)* @class.inside "}"))
(enum_declaration) @class.around
(enum_declaration
  body: (enum_body "{" (_)* @class.inside "}"))
(interface_declaration) @class.around
(interface_declaration
  body: (class_body "{" (_)* @class.inside "}"))
(object_declaration) @class.around

(test_declaration) @function.around

(line_comment)+ @comment.around
(doc_comment)+ @comment.around
(block_comment) @comment.around
