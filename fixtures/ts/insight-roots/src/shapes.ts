interface Shape {
  area(): number;
}

export class Circle implements Shape {
  area() {
    return 1;
  }
}

class Base {
  run() {}
}

class Child extends Base {
  run() {}
}
