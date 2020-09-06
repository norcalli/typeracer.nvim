local uv = require 'luv'
local splitter = require 'typeracer/splitter'
local api = vim.api
local insert = table.insert
local format = string.format
local concat = table.concat

local M = {}

local ns = api.nvim_create_namespace("typeracer")

local function attach_keybindings(buffer)
  local keys = "abcdefghijklmnopqrstuvwxyz"
  keys = keys..keys:upper()
  keys = keys.."0123456789 _-'"
  for _, map in ipairs(api.nvim_get_keymap('n')) do
    pcall(api.nvim_del_keymap, 'n', map.lhs)
  end
  for _, map in ipairs(api.nvim_buf_get_keymap(0, 'n')) do
    pcall(api.nvim_buf_del_keymap, buffer, 'n', map.lhs)
  end
  for i = 1, #keys do
    local k = keys:sub(i,i)
    api.nvim_buf_set_keymap(buffer, "n", k, format([[<cmd>lua require'typeracer'.check_key(%q)<cr>]], k), { noremap = true })
  end
end

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
  local start_time
  local finished

  local client_id

  local players = {}
  local R = {}

  local function calculate_total_keys_typed(player)
    local total_keys_typed = player.char - 1
    for i = 1, (player.word - 1) do
      -- one extra for space
      total_keys_typed = total_keys_typed + #words[i] + 1
    end
    return total_keys_typed
  end

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
      local player_draw_data = {}
      for _, k in ipairs(player_ids) do
        local player = players[k]
        local is_me = k == client_id
        -- For optimistically drawing characters, ignore the network.
        if is_me then
          player = state
        end
        local word_state = {}
        for i = player.word, #words do
          local word = words[i]
          if i == player.word then
            word = word:sub(1, player.char-1).."|"..word:sub(player.char)
          end
          insert(word_state, word)
        end
        word_state = concat(word_state, " ")
        local wpm = 0
        if start_time and words and #words > 0 and player then
          local total_keys_typed = calculate_total_keys_typed(player)
          if total_keys_typed == 0 then
            wpm = 0
          else
            -- One "word" is 5 characters.
            wpm = (total_keys_typed/5.0)/((uv.hrtime() - start_time)/1e9/60.0)
          end
        end
        local prefix = format("%sP%d [%d]: %3d WPM ", is_me and "*" or " ", k, player.word, math.floor(wpm))
        insert(player_lines, prefix..word_state)
        player_draw_data[k] = { prefix_len = #prefix }
      end
      local state_line
      if finished then
        table.sort(player_ids, function(a, b)
          players[a].total_keys = players[a].total_keys or calculate_total_keys_typed(players[a])
          players[b].total_keys = players[b].total_keys or calculate_total_keys_typed(players[b])
          return players[a].total_keys < players[b].total_keys
        end)
        state_line = format("DONE! WINNER: %d. Here are the final standings.", finished)
      elseif start_time then
        state_line = "GO!!"
      elseif is_counting_down then
        state_line = format("COUNTDOWN %d", is_counting_down)
      else
        if is_leader then
          state_line = "WAITING ON YOU! require'typeracer'.start() to start."
        else
          state_line = "WAITING ON LEADER"
        end
      end
      local lines = {
        "LOBBY CODE: "..tostring(current_lobby),
        state_line,
        string.rep("-", 80),
        player_lines,
      }
      api.nvim_buf_set_lines(buffer, 0, -1, false, vim.tbl_flatten(lines))
      api.nvim_buf_clear_namespace(buffer, ns, 0, -1)
      for i, k in ipairs(player_ids) do
        local data = player_draw_data[k]
        local player = players[k]
        local is_me = k == client_id
        -- For optimistically drawing characters, ignore the network.
        if is_me then
          player = state
        end
        local col = data.prefix_len + player.char
        api.nvim_buf_add_highlight(buffer, ns, player.err and "Error" or "Bold", 2+i, col, col+1)
      end
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

    local function check_key(key)
      if not start_time then return end
      if finished then return end
      if not state.word then return end
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

      -- Optimistically draw your characters.
      do
        local p = players[client_id]
        p.word = state.word
        p.char = state.char
        p.err = state.err
        redraw()
      end
    end

    function command_handler.CREATED(args)
      current_lobby = args[1]
      is_leader = true
      buffer = api.nvim_create_buf(true, true)
      attach_keybindings(buffer)
      api.nvim_set_current_buf(buffer)
      api.nvim_win_set_option(0, "wrap", false)
      R.start = function() send "START" end
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

    function command_handler.PLAYER_DISCONNECTED(args)
      players[tonumber(args[1] or -1) or false] = nil
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
      start_time = uv.hrtime()
    end

    function command_handler.NEW_LEADER(args)
      if client_id == tonumber(args[1]) then
        is_leader = true
        R.start = function() send "START" end
      end
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

function M.start()
  assert(assert(M.client, "no session active").start, "you're not the leader. you can't start!")()
end

function M.check_key(key)
  assert(M.client).check_key(key)
end

return M
