// not a doc comment — must be ignored
/** Adds two integers. */
export function add(a: number, b: number): number {
  return a + b;
}

/**
 * A small widget component.
 * Doc spans two lines.
 */
export class Widget {
  // render has no doc of its own; it must NOT inherit the class doc.
  render(): string {
    return "x";
  }
}
