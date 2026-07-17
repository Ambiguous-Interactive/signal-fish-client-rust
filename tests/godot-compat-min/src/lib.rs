//! Compile-time minimum-version compatibility proof for the Godot adapter.

use godot::classes::WebSocketPeer;
use godot::obj::{Gd, NewGd};
use signal_fish_client::{SignalFishConfig, SignalFishPollingClient};
use signal_fish_client_godot::GodotWebSocketTransport;

/// Construct a Godot-owned peer and pass it directly into the adapter.
///
/// This function proves that the fixture and adapter resolve one type-identical
/// `godot` binding family rather than two incompatible `Gd` definitions.
pub fn wrap_direct_peer() -> GodotWebSocketTransport {
    let peer: Gd<WebSocketPeer> = WebSocketPeer::new_gd();
    GodotWebSocketTransport::from_peer(peer)
}

/// Pass the adapter transport into the lockstep core polling client.
///
/// This proves the fixture consumes both public crates directly, matching the
/// dependency shape documented for downstream Godot applications.
pub fn polling_client() -> SignalFishPollingClient<GodotWebSocketTransport> {
    SignalFishPollingClient::new(wrap_direct_peer(), SignalFishConfig::new("compatibility-proof"))
}
