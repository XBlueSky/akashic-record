# A simple calculator class demonstrating method extraction.
class Calculator
  def initialize(initial)
    @state = initial
  end

  def add(a, b)
    a + b
  end

  def helper
    add(1, 2)
  end

  def self.version
    "1.0"
  end
end
