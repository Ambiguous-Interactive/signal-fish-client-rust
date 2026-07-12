use godot::prelude::*;
use signal_fish_client::{
    GodotWebSocketTransport, JoinRoomParams, SignalFishConfig, SignalFishEvent,
    SignalFishPollingClient,
};

const SERVER_URL: &str = "ws://127.0.0.1:3536/v2/ws";
const APP_ID: &str = "e2e-test-app";
const GAME_NAME: &str = "godot-web-smoke";

type Client = SignalFishPollingClient<GodotWebSocketTransport>;

#[derive(GodotClass)]
#[class(base = Node)]
struct SignalFishSmoke {
    base: Base<Node>,
    first: Option<Client>,
    second: Option<Client>,
    room_code: Option<String>,
    relay_sent: bool,
    finished: bool,
}

#[godot_api]
impl INode for SignalFishSmoke {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            first: None,
            second: None,
            room_code: None,
            relay_sent: false,
            finished: false,
        }
    }

    fn ready(&mut self) {
        self.first = connect_client("godot-smoke-a");
        if self.first.is_some() {
            godot_print!("SIGNAL_FISH_SMOKE transport-created first");
        }
    }

    fn process(&mut self, _delta: f64) {
        if self.finished {
            return;
        }

        let first_events = self.first.as_mut().map(Client::poll).unwrap_or_default();
        for event in first_events {
            match event {
                SignalFishEvent::Connected => {
                    godot_print!("SIGNAL_FISH_SMOKE connected first");
                }
                SignalFishEvent::Authenticated { .. } => {
                    godot_print!("SIGNAL_FISH_SMOKE authenticated first");
                    if let Some(client) = &mut self.first {
                        let _ = client.ping();
                        let _ = client.join_room(JoinRoomParams::new(GAME_NAME, "Godot-A"));
                    }
                }
                SignalFishEvent::Pong => {
                    godot_print!("SIGNAL_FISH_SMOKE pong first");
                }
                SignalFishEvent::RoomJoined { room_code, .. } => {
                    godot_print!("SIGNAL_FISH_SMOKE joined first {room_code}");
                    self.room_code = Some(room_code);
                }
                SignalFishEvent::Disconnected { reason, .. } => {
                    godot_error!("SIGNAL_FISH_SMOKE disconnected first {reason:?}");
                    self.finished = true;
                }
                _ => {}
            }
        }

        if self.second.is_none() && self.room_code.is_some() {
            self.second = connect_client("godot-smoke-b");
            if self.second.is_some() {
                godot_print!("SIGNAL_FISH_SMOKE transport-created second");
            }
        }

        let second_events = self.second.as_mut().map(Client::poll).unwrap_or_default();
        for event in second_events {
            match event {
                SignalFishEvent::Connected => {
                    godot_print!("SIGNAL_FISH_SMOKE connected second");
                }
                SignalFishEvent::Authenticated { .. } => {
                    godot_print!("SIGNAL_FISH_SMOKE authenticated second");
                    if let (Some(client), Some(room_code)) =
                        (&mut self.second, self.room_code.as_deref())
                    {
                        let params =
                            JoinRoomParams::new(GAME_NAME, "Godot-B").with_room_code(room_code);
                        let _ = client.join_room(params);
                    }
                }
                SignalFishEvent::RoomJoined { .. } if !self.relay_sent => {
                    godot_print!("SIGNAL_FISH_SMOKE joined second");
                    if let Some(client) = &mut self.first {
                        let _ = client.send_game_data(serde_json::json!({
                            "smoke": "text-relay"
                        }));
                        self.relay_sent = true;
                    }
                }
                SignalFishEvent::GameData { data, .. }
                    if data.get("smoke").and_then(serde_json::Value::as_str)
                        == Some("text-relay") =>
                {
                    godot_print!("SIGNAL_FISH_SMOKE text-relay-ok");
                    close_client(&mut self.first);
                    close_client(&mut self.second);
                    godot_print!("SIGNAL_FISH_SMOKE complete");
                    self.finished = true;
                }
                SignalFishEvent::Disconnected { reason, .. } => {
                    godot_error!("SIGNAL_FISH_SMOKE disconnected second {reason:?}");
                    self.finished = true;
                }
                _ => {}
            }
        }
    }
}

fn connect_client(platform: &str) -> Option<Client> {
    match GodotWebSocketTransport::connect(SERVER_URL) {
        Ok(transport) => {
            let mut config = SignalFishConfig::new(APP_ID);
            config.platform = Some(platform.to_string());
            Some(SignalFishPollingClient::new(transport, config))
        }
        Err(error) => {
            godot_error!("SIGNAL_FISH_SMOKE transport-error {error}");
            None
        }
    }
}

fn close_client(client: &mut Option<Client>) {
    if let Some(client) = client {
        client.close();
        let _ = client.poll();
    }
}

struct SmokeExtension;

#[gdextension]
unsafe impl ExtensionLibrary for SmokeExtension {}
