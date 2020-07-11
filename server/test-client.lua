local uv = require 'luv'
local splitter = require 'splitter'
local client = uv.new_tcp("inet")
client:connect("127.0.0.1", 1234, function(err)
  assert(not err)
  print("CONNECTED")
  assert(client:nodelay(true) == 0)
  print(client:write("CREATE\n", print))
  print(client:write("START\n", print))
  local buf = splitter("\n", true)
  local timer
  local current_word = 1
  local current_character = 1
  local has_error = false
  local words = nil
  local function handle_line(line)
    print("line:", line)
    if line == "STARTING" then
      assert(words)
      print("starting updates")
      timer = uv.new_timer()
      timer:start(0, 10, function()
        -- Pretend like we typed a new character correctly.
        current_character = current_character + 1
        local cword = words[current_word]
        if not cword then
          timer:stop()
          timer:close()
          return
        end
        if current_character > #cword then
          current_character = 1
          current_word = current_word + 1
        end
        local message = ("STATE %d %d %d\n"):format(current_word, current_character, has_error and 1 or 0)
        print(message)
        client:write(message)
      end)
    elseif line:match("^WORDS ") then
      local tail = line:match("WORDS (.*)")
      words = splitter("%s+").collect(tail)
    end
  end

  client:read_start(function(err, data)
    assert(not err)
    io.stderr:write("chunk:", tostring(data), "\n")
    if data == nil then
      for line in buf.drain() do
        handle_line(line)
      end
    else
      buf.update(data)
      for line in buf.iter do
        handle_line(line)
      end
    end
  end)
end)
uv.run()
