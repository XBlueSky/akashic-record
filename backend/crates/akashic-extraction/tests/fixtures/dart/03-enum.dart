enum Status {
  active,
  inactive,
  pending;

  bool isActive() {
    return this == Status.active;
  }
}
