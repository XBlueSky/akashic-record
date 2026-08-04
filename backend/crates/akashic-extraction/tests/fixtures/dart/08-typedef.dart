// Modern typedef form: `typedef Name = Type;`
typedef IntList = List<int>;

// Generic alias: name is still the first direct type_identifier child;
// `Map` lives nested inside the `type` RHS, so it is not mistaken for the name.
typedef StringMap<V> = Map<String, V>;

// Legacy function-typedef form: leading `int` return type is nested in a
// `type` child, while `Comparator` is the direct alias name.
typedef int Comparator(int a, int b);

// Library-private alias (leading underscore) — visibility-by-naming path.
typedef _Handler = void Function(String event);
