class Counter {
  int _count;

  Counter(this._count);

  int increment() {
    _count = _count + 1;
    return _count;
  }

  int incrementTwice() {
    increment();
    increment();
    return _count;
  }

  void _reset() {
    _count = 0;
  }
}
