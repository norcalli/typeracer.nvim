use anyhow::{Error, Result};
use log::*;
use rand::{
    seq::{IteratorRandom, SliceRandom},
    Rng,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, prelude::*};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};
use structopt::StructOpt;

#[derive(StructOpt)]
struct Opt {
    #[structopt(long, short, default_value = "0.0.0.0")]
    address: String,
    #[structopt(long, short, default_value = "1234")]
    port: u16,
}

enum LobbyState {
    WaitingForStart,
    Countdown(Instant),
    RaceRunning,
    RaceFinished,
    Dead,
}

// #[derive(Hash)]
const LOBBY_CODE_LENGTH: usize = 5;
type LobbyCode = [u8; LOBBY_CODE_LENGTH];

type ClientId = u64;

const COUNTDOWN_TIME: Duration = Duration::from_secs(5);

const WORD_COUNT: usize = 20;

struct Lobby {
    code: LobbyCode,
    leader_id: ClientId,
    state: LobbyState,
    winner: Option<ClientId>,
    clients: HashSet<ClientId>,
    // TODO(ashkan): make reference?
    words: Vec<String>,
}

struct ClientState {
    stream: TcpStream,
    read_buffer: Vec<u8>,
    id: ClientId,
    lobby: Option<LobbyCode>,
    state: PlayerState,
}

// impl ClientState {
//     pub fn new(id: ClientId, stream: TcpStream) -> Self {
//         ClientState {
//             stream,
//             id,
//         }
//     }
// }

enum ParseError {}

/// Information needed to:
/// - Check for win condition.
/// - Render current progress to other players.
#[derive(Eq, PartialEq, Debug, Default, Copy, Clone)]
struct PlayerState {
    current_word: usize,
    current_completed_character: usize,
    did_make_mistake: bool,
}

#[derive(Debug)]
enum Command {
    Start,
    Create,
    State(PlayerState),
    Join(LobbyCode),
    JoinRandom,
    Restart,
    Disconnect,
    Words,
}

fn parse_command(buffer: &[u8]) -> Result<Command> {
    if buffer == b"START" {
        return Ok(Command::Start);
    } else if buffer == b"CREATE" {
        return Ok(Command::Create);
    } else if buffer.starts_with(b"STATE ") {
        // STATE 1 30 1
        let buffer = &buffer[b"STATE ".len()..];
        let mut state = PlayerState::default();
        let mut it = buffer.split(|c| c.is_ascii_whitespace()).peekable();
        macro_rules! parse_next {
            ($it:ident, $ty:ty) => {{
                while let Some(s) = $it.peek() {
                    if s.is_empty() {
                        $it.next();
                    } else {
                        break;
                    }
                }
                anyhow::ensure!(
                    $it.peek().is_some(),
                    "Reached end of input while parsing STATE"
                );
                std::str::from_utf8(it.next().expect("HUH?"))?.parse::<$ty>()?
            }};
        }
        // TODO(ashkan): wrap error message with context.
        state.current_word = parse_next!(it, usize);
        state.current_completed_character = parse_next!(it, usize);
        state.did_make_mistake = parse_next!(it, u8) == 1;
        return Ok(Command::State(state));
    } else if buffer == b"JOIN RANDOM" {
        return Ok(Command::JoinRandom);
    } else if buffer.starts_with(b"JOIN ") {
        let buffer = &buffer[b"JOIN ".len()..];
        anyhow::ensure!(
            buffer.len() == LOBBY_CODE_LENGTH,
            "Invalid lobby code length: {}",
            buffer.len()
        );
        let mut code = [0u8; LOBBY_CODE_LENGTH];
        code.copy_from_slice(buffer);
        return Ok(Command::Join(code));
    } else if buffer == b"RESTART" {
        return Ok(Command::Restart);
    }
    Err(anyhow::anyhow!("Invalid command found"))
}

const PLACEHOLDER_CODE: [u8; 5] = [0; LOBBY_CODE_LENGTH];

#[derive(Debug)]
enum ParseAction {
    CreateLobby {
        leader_id: ClientId,
    },
    StartLobby,
    SendWords {
        lobby_code: LobbyCode,
        client_id: ClientId,
    },
    // NO OP = NO OPERATION
    Noop,
    UpdatedState {
        lobby_code: LobbyCode,
        client_id: ClientId,
        new_state: PlayerState,
    },
    JoinLobby {
        lobby_code: LobbyCode,
        client_id: ClientId,
    },
    RestartLobby,
    Disconnect {
        client_id: ClientId,
    },
}

fn transition_client(
    client: &mut ClientState,
    lobby: Option<&Lobby>,
    command: Command,
) -> Result<ParseAction> {
    use anyhow::{anyhow, bail, ensure};
    if let Command::Disconnect = command {
        return Ok(ParseAction::Disconnect {
            client_id: client.id,
        });
    }
    if lobby.is_none() {
        // ensure!(matches!(command, Command::Create), "Got a command other than CREATE with no lobby");
        return Ok(match command {
            Command::Create => ParseAction::CreateLobby {
                leader_id: client.id,
            },
            Command::Join(code) => {
                client.lobby = Some(code);
                client.state = PlayerState::default();
                ParseAction::JoinLobby {
                    lobby_code: code,
                    client_id: client.id,
                }
            }
            Command::JoinRandom => {
                client.state = PlayerState::default();
                ParseAction::JoinLobby {
                    lobby_code: PLACEHOLDER_CODE,
                    client_id: client.id,
                }
            }
            Command::Disconnect => unreachable!(),
            _ => {
                bail!("Invalid command when we don't have a lobby: {:?}", command);
            }
        });
    }
    let lobby = lobby.unwrap();
    match command {
        Command::Create => {
            warn!(
                "Got a CREATE command for an existing lobby. {:?}",
                lobby.code
            );
            Ok(ParseAction::Noop)
        }
        Command::Start => {
            ensure!(
                matches!(
                    lobby.state,
                    LobbyState::WaitingForStart | LobbyState::RaceRunning
                ),
                "Got start on lobby in a state we didn't expect."
            );
            if client.id == lobby.leader_id {
                Ok(ParseAction::StartLobby)
            } else {
                warn!("Player {} is misbehaving :(", client.id);
                Ok(ParseAction::Noop)
            }
        }
        Command::State(mut new_state) => {
            match lobby.state {
                LobbyState::RaceRunning => {}
                LobbyState::WaitingForStart | LobbyState::Countdown(_) => {
                    new_state = PlayerState::default();
                }
                _ => return Ok(ParseAction::Noop),
            }
            // TODO(ashkan): should probably check no one is cheating by updating state more than
            // one character at a time.
            client.state = new_state;
            Ok(ParseAction::UpdatedState {
                lobby_code: lobby.code,
                client_id: client.id,
                new_state,
            })
        }
        Command::Join(code) => {
            bail!("Tried to join while already in a lobby: {:?}", code);
        }
        Command::JoinRandom => {
            bail!("Tried to join a random lobby while already in a lobby");
        }
        Command::Restart => Ok(ParseAction::RestartLobby),
        Command::Words => Ok(ParseAction::SendWords {
            client_id: client.id,
            lobby_code: lobby.code,
        }),
        Command::Disconnect => unreachable!(),
    }
}

// fn parse_client(client: &mut ClientState, lobby: Option<&Lobby>) -> Result<ParseAction> {
//     Ok(match client.read_buffer.iter().position(|&c| c == b'\n') {
//         None => ParseAction::Noop,
//         Some(position) => {
//             let command = parse_command(&client.read_buffer[..position])?;
//             let action = transition_client(client, lobby, command)?;
//             // TODO(ashkan): remove the part we matched against.
//             client.read_buffer.drain(..=position);
//             action
//         }
//     })
// }

fn generate_lobby_code() -> LobbyCode {
    let mut code = LobbyCode::default();
    // Fill with random bytes.
    rand::thread_rng().fill(&mut code);
    // Move the bytes into our desired range.
    for v in code.iter_mut() {
        *v = (*v % 26) + b'A';
    }
    code
}

const WORDS: &str = include_str!("../words.txt");

fn main() -> Result<()> {
    env_logger::init();
    let opt = Opt::from_args();
    let listener = TcpListener::bind((opt.address.as_str(), opt.port))?;
    listener
        .set_nonblocking(true)
        .expect("Cannot set non-blocking");

    let mut client_index: ClientId = 0;
    let mut clients = HashMap::new();

    let mut lobbies: HashMap<LobbyCode, Lobby> = HashMap::new();

    let words: Vec<_> = WORDS.split('\n').filter(|s| !s.is_empty()).collect();

    let mut rng = rand::thread_rng();

    let mut command_buffer = VecDeque::new();
    loop {
        match listener.accept() {
            // TODO(ashkan): what is _addr for?
            Ok((mut stream, _addr)) => {
                let client_id = {
                    client_index += 1;
                    client_index
                };
                stream
                    .set_nonblocking(true)
                    .expect("Failed to set client to non-blocking");
                info!("Client connected: {}", client_id);
                stream.set_nodelay(true)?;
                let buffer = format!("CONNECTED {}\n", client_id);
                if stream.write_all(buffer.as_bytes()).is_ok() {
                    clients.insert(
                        client_id,
                        ClientState {
                            id: client_id,
                            read_buffer: vec![],
                            stream,
                            state: PlayerState::default(),
                            lobby: None,
                        },
                    );
                } else {
                    error!("Failed to initialize client {}", client_id);
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // // wait until network socket is ready, typically implemented
                // // via platform-specific APIs such as epoll or IOCP
                // wait_for_fd();
            }
            Err(e) => panic!("encountered IO error: {}", e),
        }

        for client in clients.values_mut() {
            match client.stream.read_to_end(&mut client.read_buffer) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    trace!(
                        "[client={}] Would block: {}",
                        client.id,
                        client.read_buffer.len()
                    );
                }
                Ok(bytes_read) => {
                    debug!(
                        "[client={}] Received {} bytes. Buflen: {}",
                        client.id,
                        bytes_read,
                        client.read_buffer.len()
                    );
                }
                Err(err) => {
                    error!("[client={}] client read error {}", client.id, err);
                    command_buffer.push_back((client.id, Command::Disconnect));
                }
            }
            match client.stream.take_error() {
                Ok(Some(err)) => {
                    error!("[client={}] found some error {}", client.id, err);
                    command_buffer.push_back((client.id, Command::Disconnect));
                    continue;
                }
                Err(err) => {
                    error!("[client={}] failed to check err? {}", client.id, err);
                    command_buffer.push_back((client.id, Command::Disconnect));
                    continue;
                }
                _ => (),
            }
            if !client.read_buffer.is_empty() {
                let mut last_pos = 0;
                for (i, &c) in client.read_buffer.iter().enumerate() {
                    if c == b'\n' {
                        let line = &client.read_buffer[last_pos..i];
                        match parse_command(line) {
                            Ok(command) => {
                                command_buffer.push_back((client.id, command));
                            }
                            Err(err) => {
                                error!(
                                    "[client={}] Input line: {:?}\nParse error {:?}\nPressing on...",
                                    client.id, line, err
                                    );
                            }
                        }
                        last_pos = i + 1;
                    }
                }
                if last_pos > 0 {
                    client.read_buffer.drain(..last_pos);
                }
            }
        }

        fn try_send(
            client: &mut ClientState,
            message: &[u8],
            command_buffer: &mut VecDeque<(ClientId, Command)>,
        ) {
            if client.stream.write_all(message).is_err() {
                command_buffer.push_back((client.id, Command::Disconnect));
            }
        }

        // TODO(ashkan): we could group these to avoid redundant hashmap lookups...
        while let Some((client_id, command)) = command_buffer.pop_front() {
            debug!("[client={}] command: {:?}", client_id, command);
            let client = match clients.get_mut(&client_id) {
                Some(client) => client,
                None => {
                    debug!("[client={}] not found", client_id);
                    continue;
                }
            };
            let lobby = client.lobby.and_then(|c| lobbies.get_mut(&c));
            let action = match transition_client(client, lobby.as_ref().map(|x| &**x), command) {
                Err(err) => {
                    error!("[client={}] Invalid transition! {:?}", client.id, err);
                    ParseAction::Disconnect { client_id }
                }
                Ok(action) => action,
            };
            match action {
                ParseAction::SendWords {
                    client_id,
                    lobby_code,
                } => {
                    assert_eq!(Some(lobby_code), client.lobby);
                    assert_eq!(client_id, client.id);
                    let lobby = lobby.expect("ALSKDFJASLDJ");
                    let buffer = format!("WORDS {}\n", lobby.words.join(" "));
                    let message = buffer.as_bytes();
                    try_send(
                        clients.get_mut(&client_id).unwrap(),
                        message,
                        &mut command_buffer,
                    );
                }
                ParseAction::Disconnect { client_id } => {
                    if let Some(lobby) = lobby {
                        lobby.clients.remove(&client_id);
                    }
                    clients.remove(&client_id);
                }
                ParseAction::Noop => println!("noop noop"),
                ParseAction::StartLobby => {
                    // TODO(ashkan): this could be empty..?
                    if let Some(mut lobby) = lobby {
                        let buffer = format!("COUNTDOWN {}\n", COUNTDOWN_TIME.as_secs());
                        let message = buffer.as_bytes();
                        for client_id in &lobby.clients {
                            // TODO(ashkan): handle errors here.
                            try_send(
                                clients.get_mut(&client_id).unwrap(),
                                message,
                                &mut command_buffer,
                            );
                        }
                        lobby.state = LobbyState::Countdown(Instant::now() + COUNTDOWN_TIME);
                    }
                }
                ParseAction::CreateLobby { leader_id } => {
                    // Keep tryin' til' we get that code.
                    let code = loop {
                        let code = generate_lobby_code();
                        if !lobbies.contains_key(&code) {
                            break code;
                        }
                    };
                    lobbies.insert(
                        code,
                        Lobby {
                            winner: None,
                            leader_id,
                            code,
                            state: LobbyState::WaitingForStart,
                            clients: [leader_id].iter().copied().collect(),
                            words: words
                                .choose_multiple(&mut rng, WORD_COUNT)
                                .cloned()
                                .map(|x| x.to_owned())
                                .collect(),
                        },
                    );
                    let buffer = format!("CREATED {}\n", std::str::from_utf8(&code)?);
                    let client = clients.get_mut(&client_id).unwrap();
                    client.lobby = Some(code);
                    try_send(client, buffer.as_bytes(), &mut command_buffer);
                    command_buffer.push_back((client_id, Command::Words));
                    command_buffer.push_back((client_id, Command::State(client.state)));
                }
                ParseAction::JoinLobby {
                    lobby_code,
                    client_id,
                } => {
                    // Join random
                    let lobby_code = if lobby_code == PLACEHOLDER_CODE {
                        match lobbies.keys().choose(&mut rng) {
                            Some(key) => *key,
                            None => {
                                try_send(
                                    clients.get_mut(&client_id).unwrap(),
                                    b"JOIN_FAILED\n",
                                    &mut command_buffer,
                                );
                                continue;
                            }
                        }
                    } else {
                        lobby_code
                    };
                    match lobbies.get_mut(&lobby_code) {
                        Some(lobby) => {
                            // TODO(ashkan): check client didn't get inserted twice.
                            lobby.clients.insert(client_id);
                            command_buffer.push_back((client_id, Command::Words));
                            for client_id in &lobby.clients {
                                let client = clients.get(&client_id).unwrap();
                                command_buffer
                                    .push_back((*client_id, Command::State(client.state)));
                            }
                            let buffer = format!("JOINED {}\n", std::str::from_utf8(&lobby_code)?);
                            try_send(
                                clients.get_mut(&client_id).unwrap(),
                                buffer.as_bytes(),
                                &mut command_buffer,
                            );
                        }
                        None => {
                            // TODO(ashkan): return failed code?
                            try_send(
                                clients.get_mut(&client_id).unwrap(),
                                b"JOIN_FAILED\n",
                                &mut command_buffer,
                            );
                        }
                    }
                }
                ParseAction::UpdatedState {
                    lobby_code,
                    client_id,
                    new_state,
                } => {
                    let lobby = lobbies
                        .get_mut(&lobby_code)
                        .expect("Should've had lobby double checked in parse_client");
                    // check if this is finished.
                    info!("Word length {} {:?}", lobby.words.len(), lobby.words);
                    if new_state.current_word >= lobby.words.len() && lobby.winner.is_none() {
                        lobby.winner = Some(client_id);
                        info!(
                            "Lobby {} finished with winner {}",
                            std::str::from_utf8(&lobby_code).unwrap(),
                            client_id
                        );
                        let buffer = format!("FINISHED {}\n", client_id);
                        let message = buffer.as_bytes();
                        for client_id in &lobby.clients {
                            let client = clients.get_mut(&client_id).unwrap();
                            // TODO(ashkan): handle errors here.
                            try_send(client, message, &mut command_buffer);
                        }
                    } else {
                        let buffer = format!(
                            "STATE {} {} {} {}\n",
                            client_id,
                            new_state.current_word,
                            new_state.current_completed_character,
                            new_state.did_make_mistake as i32
                        );
                        let message = buffer.as_bytes();
                        for client_id in &lobby.clients {
                            let client = clients.get_mut(&client_id).unwrap();
                            // TODO(ashkan): handle errors here.
                            try_send(client, message, &mut command_buffer);
                        }
                    }
                }
                ParseAction::RestartLobby => unimplemented!("ALSDKFJALS"),
            }
        }

        for lobby in lobbies.values_mut() {
            match lobby.state {
                LobbyState::Countdown(deadline) if deadline <= Instant::now() => {
                    let message = b"STARTING\n";
                    for client_id in &lobby.clients {
                        let client = clients.get_mut(&client_id).unwrap();
                        // TODO(ashkan): handle errors here.
                        try_send(client, message, &mut command_buffer);
                    }
                    lobby.state = LobbyState::RaceRunning;
                }
                _ => (),
            }
        }

        std::thread::sleep(Duration::from_millis(10));
    }

    // Ok(())
}
