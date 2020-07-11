local uv = require 'luv'
local splitter = require 'typeracer/splitter'
local api = vim.api
local insert = table.insert
local format = string.format
local concat = table.concat

local M = {}

local function make_client(host, port, callback)
  assert(host and port)
  local current_lobby
  local words
  local is_leader
  local state = {
    word = 1,
    char = 1,
    err = false,
  }
  local stream = uv.new_tcp()
  local is_counting_down = nil

  local buffer
  local started
  local finished

  local client_id

  local players = {}
  local R = {}
  stream:connect(host, port, function(err)
    assert(not err)
    local function send(command)
      stream:write{command, "\n"}
    end

    local function send_state()
      send(("STATE %d %d %d"):format(state.word - 1, state.char - 1, state.err and 1 or 0))
    end

    R.create = function(callback)
      assert(not current_lobby)
      send "CREATE"
      -- TODO(ashkan): use callback to warn connect?
    end

    R.join = function(code)
      assert(not current_lobby)
      send("JOIN "..code)
      -- TODO(ashkan): use callback to warn connect?
    end

    R.join_random = function(code)
      assert(not current_lobby)
      send("JOIN RANDOM")
      -- TODO(ashkan): use callback to warn connect?
    end

    local command_handler = {}

    local function redraw()
      if not buffer then return end
      local player_ids = vim.tbl_keys(players)
      table.sort(player_ids)
      local player_lines = {}
      for _, k in ipairs(player_ids) do
        local player = players[k]
        local is_me = k == client_id
        local word_state = {}
        for i = player.word, #words do
          local word = words[i]
          if i == player.word then
            word = "_"..word:sub(1, player.char).."_"..word:sub(player.char+1)
          end
          insert(word_state, word)
        end
        word_state = concat(word_state, " ")
        insert(player_lines, format("%sP%d [%d]: %s", is_me and "*" or " ", k, player.word, word_state))
      end
      local state_line
      if finished then
        state_line = format("DONE! WINNER: %d", finished)
      elseif started then
        state_line = "GO!!"
      elseif is_counting_down then
        state_line = format("COUNTDOWN %d", is_counting_down)
      else
        state_line = "WAITING"
      end
      local lines = {
        current_lobby,
        state_line,
        player_lines,
      }
      api.nvim_buf_set_lines(buffer, 0, -1, false, vim.tbl_flatten(lines))
    end

    function command_handler.WORDS(args)
      words = args
    end

    function command_handler.STATE(args)
      local client_id, cword, cchar, has_err = unpack(args)
      players[tonumber(client_id)] = {
        word = tonumber(cword)+1;
        char = tonumber(cchar)+1;
        err = has_err == "1";
      }
    end

    function command_handler.CONNECTED(args)
      client_id = tonumber(args[1])
    end

    local function attach_keybindings(buffer)
      local keys = "abcdefghijklmnopqrstuvwxyz"
      keys = keys..keys:upper()
      keys = keys.."0123456789 _-'"
      for i = 1, #keys do
        local k = keys:sub(i,i)
        api.nvim_buf_set_keymap(buffer, "n", k, format([[<cmd>lua require'typeracer'.check_key(%q)<cr>]], k), { noremap = true })
      end
    end

    local function check_key(key)
      assert(words)
      -- TODO(ashkan): cleanup
      local target = assert(words[state.word]):sub(state.char,state.char)
      if target == key then
        state.err = false
        state.char = state.char + 1
        if state.char > #words[state.word] then
          state.word = state.word + 1
          state.char = 1
        end
      else
        state.err = true
      end
      send_state()
    end

    function command_handler.CREATED(args)
      current_lobby = args[1]
      is_leader = true
      buffer = api.nvim_create_buf(true, true)
      attach_keybindings(buffer)
      api.nvim_set_current_buf(buffer)
      api.nvim_win_set_option(0, "wrap", false)
      R.start = function()
        send "START"
      end
      R.check_key = check_key
    end

    function command_handler.JOINED(args)
      assert(not current_lobby)
      current_lobby = args[1]
      buffer = api.nvim_create_buf(true, true)
      attach_keybindings(buffer)
      api.nvim_set_current_buf(buffer)
      api.nvim_win_set_option(0, "wrap", false)
      R.check_key = check_key
    end

    function command_handler.JOIN_FAILED(args)
      assert(not current_lobby)
      error("Failed to join!")
    end

    function command_handler.COUNTDOWN(args)
      is_counting_down = tonumber(args[1])
      local timer = uv.new_timer()
      timer:start(0, 1e3, function()
        is_counting_down = is_counting_down - 1
        if is_counting_down <= 0 then
          timer:stop()
          timer:close()
        else
          vim.schedule(redraw)
        end
      end)
    end

    function command_handler.STARTING(args)
      started = true
    end

    function command_handler.FINISHED(args)
      finished = tonumber(args[1])
    end

    function command_handler.default(args)
      print("default:", args)
    end

    local function handle_line(line)
      local parts = splitter("%s+").collect(line)
      local command = table.remove(parts, 1)
      -- TODO(ashkan): remove the fallback?
      assert(command_handler[command] or command_handler.default, "Invalid event received")(parts)
      redraw()
    end

    local buf = splitter("\n", true)
    stream:read_start(vim.schedule_wrap(function(err, data)
      -- TODO(ashkan): better error handling.
      assert(not err)
      -- io.stderr:write("chunk:", tostring(data), "\n")
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
    end))
    if callback then
      callback(R)
    end
  end)
  return R
end

M.client = nil


function M.create(host, port)
  assert(not M.client)
  local kiani_io_addr = uv.getaddrinfo("kiani.io")[1].addr
  return make_client(host or kiani_io_addr, port or 1234, function(client)
    M.client = client
    client.create()
  end)
end

function M.join(code, host, port)
  assert(not M.client)
  assert(code)
  local kiani_io_addr = uv.getaddrinfo("kiani.io")[1].addr
  return make_client(host or kiani_io_addr, port or 1234, function(client)
    M.client = client
    client.join(code)
  end)
end

function M.join_random(host, port)
  assert(not M.client)
  local kiani_io_addr = uv.getaddrinfo("kiani.io")[1].addr
  return make_client(host or kiani_io_addr, port or 1234, function(client)
    M.client = client
    client.join_random()
  end)
end

function M.check_key(key)
  assert(M.client).check_key(key)
end

return M
