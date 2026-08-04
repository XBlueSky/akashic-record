require("a.b")
local json = require "c"

local function setup()
  local data = decode(json)
  return data
end

return setup
