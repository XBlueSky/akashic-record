require "json"
require "set"
require_relative "../lib/helper"

class Loader
  def load(path)
    JSON.parse(path)
  end
end
