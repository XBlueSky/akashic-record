local function make_counter(start: number)
  local count = start
  local function increment(): number
    count = count + 1
    return count
  end
  return increment
end

function outer(): () -> number
  local inner = function(): number
    return compute()
  end
  return inner
end
