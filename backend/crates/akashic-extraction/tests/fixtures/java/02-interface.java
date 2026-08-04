package com.example.shapes;

import java.util.List;

/**
 * Represents a geometric shape with area and perimeter operations.
 * All concrete shapes must implement this interface contract.
 */
public interface Shape {
    /**
     * Calculates and returns the area of this shape.
     * Implementations must provide the geometric area formula.
     */
    double area();

    /**
     * Calculates and returns the perimeter of this shape.
     * Implementations must provide the perimeter formula.
     */
    double perimeter();

    /**
     * Returns a human-readable description of this shape.
     * Default implementation provides a generic description.
     */
    default String describe() {
        return "geometric shape with area=" + area();
    }

    /**
     * Checks whether this shape is larger than another shape.
     * Default comparison is based on area calculation results.
     */
    default boolean isLargerThan(Shape other) {
        return this.area() > other.area();
    }
}
