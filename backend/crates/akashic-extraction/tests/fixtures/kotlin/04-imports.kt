package com.example.app

import kotlin.math.PI
import kotlin.collections.List
import com.example.helpers.format
import com.example.util.*

fun circleArea(radius: Double): Double {
    return PI * radius * radius
}

class Greeter {
    fun greet(name: String): String {
        return format(name)
    }
}
