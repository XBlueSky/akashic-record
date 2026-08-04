package com.example;

/**
 * Base interface for all shapes.
 */
public interface Shape {
    double area();
}

/**
 * Drawable interface for renderable objects.
 */
public interface Drawable {
    void draw();
}

/**
 * A circle that extends no class but implements Shape and Drawable.
 */
public class Circle implements Shape, Drawable {
    private double radius;

    public Circle(double radius) {
        this.radius = radius;
    }

    @Override
    public double area() {
        return Math.PI * radius * radius;
    }

    @Override
    public void draw() {
        System.out.println("Drawing circle with radius=" + radius);
    }
}

/**
 * A colored circle that extends Circle.
 */
public class ColoredCircle extends Circle {
    private String color;

    public ColoredCircle(double radius, String color) {
        super(radius);
        this.color = color;
    }
}

/**
 * An interface that extends another interface.
 */
public interface ColoredShape extends Shape {
    String color();
}
