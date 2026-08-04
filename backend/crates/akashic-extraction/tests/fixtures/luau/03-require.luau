require("a.b")
local json = require "c"

local function setup(): string
  local data = decode(json)
  return data
end

return setup
