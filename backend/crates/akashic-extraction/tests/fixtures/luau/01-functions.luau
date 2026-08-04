local M = {}

function M.foo(x: number): number
  return helper(x)
end

function M:bar(y: number): number
  return self.value + y
end

local function helper(n: number): number
  return n * 2
end

return M
