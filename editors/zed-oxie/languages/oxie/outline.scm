(class_declaration
  name: (type_identifier) @name) @item

(enum_declaration
  name: (type_identifier) @name) @item

(interface_declaration
  name: (type_identifier) @name) @item

(object_declaration
  name: (_) @name) @item

(impl_declaration
  "impl" @context
  (type_identifier) @name) @item

(function_declaration
  name: (identifier) @name) @item

(enum_entry
  name: (type_identifier) @name) @item

(property_declaration
  name: (property_name) @name) @item

(class_parameter
  name: (property_name) @name) @item

(type_alias
  name: (type_identifier) @name) @item

(test_declaration
  "test" @context
  name: (string) @name) @item
