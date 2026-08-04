function foo() {
  return bar();
}

export function bar() {
  return 42;
}

const baz = () => {
  return foo();
};
