// Exercises dart_resolve_name for getter / setter / operator methods, whose
// names live in getter_signature / setter_signature / operator_signature
// (not function_signature). Before the fix all three chunks were dropped.
class Temperature {
  double _celsius = 0;

  double get fahrenheit => _celsius * 9 / 5 + 32;

  set fahrenheit(double f) {
    _celsius = (f - 32) * 5 / 9;
  }

  Temperature operator +(Temperature other) {
    return Temperature().._celsius = _celsius + other._celsius;
  }
}
