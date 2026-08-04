// not a doc comment — must be ignored
/** Doubles a value. */
export function double(x) {
  return x * 2;
}

/** A simple counter. */
class Counter {
  // increment has no doc; it must NOT inherit the class doc.
  increment() {
    return 1;
  }
}
