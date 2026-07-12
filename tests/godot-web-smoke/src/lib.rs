use godot::prelude::*;
use signal_fish_client::protocol::GameDataEncoding;
use signal_fish_client::{
    GodotWebSocketTransport, JoinRoomParams, SignalFishConfig, SignalFishEvent,
    SignalFishPollingClient,
};

const SERVER_URL: &str = "ws://127.0.0.1:3536/v2/ws";
const APP_ID: &str = "e2e-test-app";
const BINARY_PAYLOAD: &[u8] = &[0, 1, 2, 255];

type Client = SignalFishPollingClient<GodotWebSocketTransport>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PairKind {
    Json,
    Binary,
}

impl PairKind {
    fn label(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Binary => "binary",
        }
    }

    fn game_name(self) -> &'static str {
        match self {
            Self::Json => "godot-web-smoke-json",
            Self::Binary => "godot-web-smoke-binary",
        }
    }

    fn encoding(self) -> GameDataEncoding {
        match self {
            Self::Json => GameDataEncoding::Json,
            Self::Binary => GameDataEncoding::MessagePack,
        }
    }
}

struct SmokePair {
    kind: PairKind,
    first: Option<Client>,
    second: Option<Client>,
    room_code: Option<String>,
    relay_sent: bool,
    relay_received: bool,
    pong_received: bool,
    closing: bool,
    shutdown_done: bool,
    close_attributed: bool,
    server_close_ready_logged: bool,
}

impl SmokePair {
    fn new(kind: PairKind) -> Self {
        Self {
            kind,
            first: connect_client(kind, "a"),
            second: None,
            room_code: None,
            relay_sent: false,
            relay_received: false,
            pong_received: false,
            closing: false,
            shutdown_done: false,
            close_attributed: false,
            server_close_ready_logged: false,
        }
    }

    fn poll(&mut self) {
        if self.shutdown_done || self.close_attributed {
            return;
        }
        if self.closing {
            self.drive_close();
            return;
        }

        let first_events = self.first.as_mut().map(Client::poll).unwrap_or_default();
        for event in first_events {
            self.handle_first(event);
        }

        if self.second.is_none() && self.room_code.is_some() {
            self.second = connect_client(self.kind, "b");
        }

        let second_events = self.second.as_mut().map(Client::poll).unwrap_or_default();
        for event in second_events {
            self.handle_second(event);
        }

        if self.kind == PairKind::Json && self.relay_received && self.pong_received {
            godot_print!("SIGNAL_FISH_SMOKE json-ready-for-shutdown");
            close_client(&mut self.first);
            close_client(&mut self.second);
            self.closing = true;
        }
        if self.kind == PairKind::Binary
            && self.relay_received
            && self.pong_received
            && !self.server_close_ready_logged
        {
            godot_print!("SIGNAL_FISH_SMOKE binary-ready-for-server-close");
            self.server_close_ready_logged = true;
        }
    }

    fn handle_first(&mut self, event: SignalFishEvent) {
        let label = self.kind.label();
        match event {
            SignalFishEvent::Connected => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-connected-first");
            }
            SignalFishEvent::Authenticated { .. } => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-authenticated-first");
                if let Some(client) = &mut self.first {
                    let _ = client.ping();
                    let _ = client.join_room(JoinRoomParams::new(
                        self.kind.game_name(),
                        format!("Godot-{label}-A"),
                    ));
                }
            }
            SignalFishEvent::Pong => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-pong-ok");
                self.pong_received = true;
            }
            SignalFishEvent::RoomJoined { room_code, .. } => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-joined-first");
                self.room_code = Some(room_code);
            }
            SignalFishEvent::Disconnected { reason, .. } => {
                self.handle_disconnect("first", reason.as_deref());
            }
            _ => {}
        }
    }

    fn handle_second(&mut self, event: SignalFishEvent) {
        let label = self.kind.label();
        match event {
            SignalFishEvent::Connected => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-connected-second");
            }
            SignalFishEvent::Authenticated { .. } => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-authenticated-second");
                if let (Some(client), Some(room_code)) =
                    (&mut self.second, self.room_code.as_deref())
                {
                    let params =
                        JoinRoomParams::new(self.kind.game_name(), format!("Godot-{label}-B"))
                            .with_room_code(room_code);
                    let _ = client.join_room(params);
                }
            }
            SignalFishEvent::RoomJoined { .. } if !self.relay_sent => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-joined-second");
                self.send_relay();
            }
            SignalFishEvent::GameData { data, .. }
                if self.kind == PairKind::Json
                    && data.get("smoke").and_then(serde_json::Value::as_str)
                        == Some("text-relay") =>
            {
                godot_print!("SIGNAL_FISH_SMOKE text-relay-ok");
                self.relay_received = true;
            }
            SignalFishEvent::GameDataBinary {
                payload, encoding, ..
            } if self.kind == PairKind::Binary
                && encoding == GameDataEncoding::MessagePack
                && payload == BINARY_PAYLOAD =>
            {
                godot_print!("SIGNAL_FISH_SMOKE binary-relay-ok");
                self.relay_received = true;
            }
            SignalFishEvent::Disconnected { reason, .. } => {
                self.handle_disconnect("second", reason.as_deref());
            }
            _ => {}
        }
    }

    fn send_relay(&mut self) {
        let Some(client) = &mut self.first else {
            return;
        };
        let result = match self.kind {
            PairKind::Json => client.send_game_data(serde_json::json!({
                "smoke": "text-relay"
            })),
            PairKind::Binary => client.send_binary_game_data(BINARY_PAYLOAD.to_vec()),
        };
        if let Err(error) = result {
            godot_error!(
                "SIGNAL_FISH_SMOKE {}-relay-send-error {error}",
                self.kind.label()
            );
        } else {
            self.relay_sent = true;
        }
    }

    fn handle_disconnect(&mut self, peer: &str, reason: Option<&str>) {
        let label = self.kind.label();
        if self.kind == PairKind::Binary
            && self.relay_received
            && reason.is_some_and(|reason| reason.contains("code=Some(4000)"))
        {
            godot_print!("SIGNAL_FISH_SMOKE close-attribution-ok {peer}");
            self.close_attributed = true;
            close_client(&mut self.first);
            close_client(&mut self.second);
        } else if !self.closing {
            godot_error!("SIGNAL_FISH_SMOKE {label}-unexpected-disconnect {peer} {reason:?}");
        }
    }

    fn drive_close(&mut self) {
        if let Some(client) = &mut self.first {
            let _ = client.poll();
        }
        if let Some(client) = &mut self.second {
            let _ = client.poll();
        }
        let first_done = self
            .first
            .as_ref()
            .is_none_or(|client| !client.is_closing());
        let second_done = self
            .second
            .as_ref()
            .is_none_or(|client| !client.is_closing());
        if first_done && second_done {
            godot_print!("SIGNAL_FISH_SMOKE json-shutdown-ok");
            self.shutdown_done = true;
        }
    }
}

#[derive(GodotClass)]
#[class(base = Node)]
struct SignalFishSmoke {
    base: Base<Node>,
    json: SmokePair,
    binary: SmokePair,
    complete: bool,
}

#[godot_api]
impl INode for SignalFishSmoke {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            json: SmokePair::new(PairKind::Json),
            binary: SmokePair::new(PairKind::Binary),
            complete: false,
        }
    }

    fn ready(&mut self) {
        godot_print!("SIGNAL_FISH_SMOKE fixture-ready");
    }

    fn process(&mut self, _delta: f64) {
        if self.complete {
            return;
        }
        self.json.poll();
        self.binary.poll();
        if self.json.shutdown_done && self.binary.close_attributed {
            godot_print!("SIGNAL_FISH_SMOKE complete");
            self.complete = true;
            if let Some(mut tree) = self.base().get_tree() {
                tree.quit();
            }
        }
    }
}

fn connect_client(kind: PairKind, suffix: &str) -> Option<Client> {
    match GodotWebSocketTransport::connect(SERVER_URL) {
        Ok(transport) => {
            let mut config = SignalFishConfig::new(APP_ID).enable_v3();
            config.platform = Some(format!("godot-smoke-{}-{suffix}", kind.label()));
            config.game_data_format = Some(kind.encoding());
            Some(SignalFishPollingClient::new(transport, config))
        }
        Err(error) => {
            godot_error!("SIGNAL_FISH_SMOKE {}-transport-error {error}", kind.label());
            None
        }
    }
}

fn close_client(client: &mut Option<Client>) {
    if let Some(client) = client {
        client.close();
    }
}

struct SmokeExtension;

// The CI negative-control build enables this feature to force the raw
// Emscripten WebSocket imports into an otherwise valid Godot GDExtension.
// Official templates cannot resolve those optional JavaScript-library symbols.
#[cfg(feature = "raw-emscripten-proof")]
#[allow(deprecated)]
fn exercise_raw_emscripten_import() {
    let _ = signal_fish_client::EmscriptenWebSocketTransport::connect("ws://127.0.0.1:3536/ws");
}

#[gdextension]
unsafe impl ExtensionLibrary for SmokeExtension {
    fn on_level_init(_level: InitLevel) {
        #[cfg(feature = "raw-emscripten-proof")]
        exercise_raw_emscripten_import();
    }
}
