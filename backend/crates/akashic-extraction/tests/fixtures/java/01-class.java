package com.example;

import java.util.List;
import java.util.ArrayList;

/**
 * A simple calculator class demonstrating method extraction.
 * This class provides basic arithmetic operations.
 */
public class Calculator {
    private int state;
    private String label;

    /**
     * Constructs a Calculator with an initial state value.
     * Sets up the internal state for subsequent operations.
     */
    public Calculator(int initial) {
        this.state = initial;
        this.label = "calculator-" + initial;
    }

    /**
     * Adds two integers and returns the result.
     * This is the primary arithmetic operation of this class.
     */
    public int add(int a, int b) {
        int result = a + b;
        return result;
    }

    /**
     * A private helper that delegates to the public add method.
     * Demonstrates intra-class method calls for graph extraction.
     */
    private int helper() {
        return add(1, 2);
    }

    /**
     * Returns a string representation of the calculator.
     * Uses the internal label field for display purposes.
     */
    public String toString() {
        return label;
    }
}
