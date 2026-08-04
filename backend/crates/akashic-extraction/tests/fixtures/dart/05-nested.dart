int outer(List<int> items) {
  const factor = 2;
  int scale(int x) {
    return x * factor;
  }
  int total() {
    return items.map(scale).reduce((a, b) => a + b);
  }
  return total();
}

int standalone() {
  return outer([1, 2, 3]);
}
