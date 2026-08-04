interface Animal {
  name: string;
  speak(): string;
}

interface Runnable {
  run(): void;
}

interface Pet extends Animal {
  owner: string;
}

class Dog implements Animal, Runnable {
  name: string;

  constructor(name: string) {
    this.name = name;
  }

  speak(): string {
    return `Woof! I am ${this.name}`;
  }

  run(): void {
    console.log(`${this.name} is running`);
  }
}

class GuideDog extends Dog {
  handler: string;

  constructor(name: string, handler: string) {
    super(name);
    this.handler = handler;
  }
}
