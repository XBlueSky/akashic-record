abstract class Animal {
  String get name;
  String speak();
}

mixin Runner {
  void run() {
    print('running');
  }
}

class Dog extends Animal with Runner {
  final String name;

  Dog(this.name);

  @override
  String speak() => 'Woof!';
}

class GuideDog extends Dog {
  final String handler;

  GuideDog(String name, this.handler) : super(name);
}
