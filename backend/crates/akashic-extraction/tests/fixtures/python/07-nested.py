def outer(items):
    """Process items via a nested helper."""
    factor = 2

    def scale(x):
        """Nested: scale a value by the captured factor."""
        return x * factor

    def total():
        """Nested: sum the scaled items."""
        return sum(scale(i) for i in items)

    doubler = lambda y: y * 2
    return total()


def standalone():
    """A top-level function calling outer()."""
    return outer([1, 2, 3])
