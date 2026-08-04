# A module used as a mixin plus a class that includes it and uses attr_accessor.
module Greetable
  def greet
    "Hello, #{name}"
  end
end

class Person
  include Greetable

  attr_accessor :name

  def initialize(name)
    @name = name
  end
end
