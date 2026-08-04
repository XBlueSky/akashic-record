interface Shape {
    fun area(): Double
}

class Circle(val radius: Double) : Shape {
    override fun area(): Double {
        return 3.14159 * radius * radius
    }
}

object ShapeRegistry {
    val shapes = mutableListOf<Shape>()

    fun register(shape: Shape) {
        shapes.add(shape)
    }

    companion object {
        fun empty(): ShapeRegistry {
            return ShapeRegistry
        }
    }
}
