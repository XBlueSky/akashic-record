mixin Walker {
  void walk() {
    step();
  }

  void step() {}
}

class Robot with Walker {
  void activate() {
    walk();
  }
}
