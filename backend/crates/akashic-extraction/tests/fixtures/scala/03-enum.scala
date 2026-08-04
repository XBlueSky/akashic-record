enum Planet(mass: Double) {
  case Earth extends Planet(5.976e+24)
  case Mars extends Planet(6.421e+23)

  def surfaceGravity(): Double = {
    mass * 9.8
  }

  def describe(): Double = {
    surfaceGravity()
  }
}
