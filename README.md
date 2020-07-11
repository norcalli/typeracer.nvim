# Installation

`Plug norcalli/typeracer.nvim`

# Instructions

`lua require'typeracer'.create()` to create a new game. By default it will try the server hosted on kiani.io

This will create a new window with a 5 letter code you can use to join a game.

`lua require'typeracer'.join(code)` to join a game, e.g. `lua require'typeracer.join "ABCDE"`

`lua require'typeracer'.join_random()` to join a random game.


If you are the one who created the game, use `lua require 'typeracer'.client.start()` to start the countdown.
