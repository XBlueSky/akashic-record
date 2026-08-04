package com.example;

import java.util.ArrayList;
import com.example.shapes.Shape;

/**
 * Exercises object_creation_expression (`new Foo()`) call-name resolution.
 * Before the fix, every constructor call here dropped its Call edge because
 * the resolver read a non-existent `name` field instead of the `type` field.
 */
public class Factory {

    /**
     * Builds three things via constructors plus one method_invocation, so the
     * golden must show Call edges to Widget, ArrayList, Shape, and configure.
     */
    public Widget build() {
        Widget w = new Widget();                 // type_identifier        -> "Widget"
        ArrayList<String> names = new ArrayList<String>(); // generic_type -> "ArrayList"
        Shape s = new com.example.shapes.Shape();// scoped_type_identifier -> "Shape"
        w.configure(s, names);                   // method_invocation      -> "configure"
        return w;
    }
}
