export class Counter {
  constructor(start) {
    this.value = start;
  }

  increment() {
    this.value += 1;
    return this.report();
  }

  report() {
    return this.value;
  }
}
