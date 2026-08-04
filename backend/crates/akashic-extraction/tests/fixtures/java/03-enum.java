package com.example;

/**
 * Represents the color primaries in the RGB color model.
 * Each constant maps to a distinct visible light wavelength range.
 */
public enum Color {
    RED,
    GREEN,
    BLUE;

    /**
     * Returns the lowercase name of this color constant.
     * Useful for display and serialization purposes.
     */
    public String lower() {
        return name().toLowerCase();
    }

    /**
     * Returns whether this color is a warm color tone.
     * Only RED is considered warm in the basic RGB model.
     */
    public boolean isWarm() {
        return this == RED;
    }
}
