# A module acting as a namespace around a nested class and module method.
module Geometry
  def self.pi
    3.14159
  end

  class Circle
    def initialize(radius)
      @radius = radius
    end

    def area
      Geometry.pi * @radius * @radius
    end
  end

  module Utils
    def self.square(x)
      x * x
    end
  end
end
