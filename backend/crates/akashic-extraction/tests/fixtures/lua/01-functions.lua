local M = {}

function M.foo(x)
  return helper(x)
end

function M:bar(y)
  return self.value + y
end

local function helper(n)
  return n * 2
end

return M
