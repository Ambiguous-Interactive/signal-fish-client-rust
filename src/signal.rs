//! Typed WebRTC signaling payloads (protocol v3).
//!
//! [`PeerSignal`] is a convenience view over the opaque `signal` field carried by
//! [`ClientMessage::Signal`](crate::protocol::ClientMessage::Signal) and
//! [`ServerMessage::Signal`](crate::protocol::ServerMessage::Signal). Those wire
//! fields are `serde_json::Value` so that an unknown future signal shape can never
//! break deserialization on the receive path; `PeerSignal` lets consumers work
//! with the common offer/answer/ICE shapes ergonomically.
//!
//! The JSON representation is **externally tagged** (serde's default for enums),
//! byte-identical to [`matchbox_socket::PeerSignal`]:
//!
//! ```json
//! { "Offer": "<sdp>" }
//! { "Answer": "<sdp>" }
//! { "IceCandidate": "<candidate>" }
//! ```
//!
//! [`matchbox_socket::PeerSignal`]: https://docs.rs/matchbox_socket/latest/matchbox_socket/enum.PeerSignal.html

use serde::{Deserialize, Serialize};

/// A WebRTC signaling message exchanged between peers (protocol v3).
///
/// Serializes as an externally-tagged enum (`{"Offer": "…"}`), matching the
/// Signal Fish server's matchbox-compatible signal format. Use the
/// [`From`]/[`TryFrom`] conversions to move between `PeerSignal` and the raw
/// `serde_json::Value` carried on the wire.
///
/// # Examples
///
/// ```
/// use signal_fish_client::PeerSignal;
///
/// let offer = PeerSignal::Offer("v=0\r\n".into());
/// let value: serde_json::Value = offer.clone().into();
/// assert_eq!(value, serde_json::json!({ "Offer": "v=0\r\n" }));
///
/// let back = PeerSignal::try_from(&value)?;
/// assert_eq!(back, offer);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerSignal {
    /// An SDP offer.
    Offer(String),
    /// An SDP answer.
    Answer(String),
    /// A single ICE candidate (trickle ICE).
    IceCandidate(String),
}

impl From<PeerSignal> for serde_json::Value {
    fn from(signal: PeerSignal) -> Self {
        // Build the externally-tagged object directly so the conversion is
        // *structurally* infallible: there is no serializer that can fail and
        // no lossy `Null` fallback that could silently corrupt the wire. This
        // mirrors serde's externally-tagged enum representation exactly (see
        // the round-trip tests below, which pin it against `Serialize`).
        match signal {
            PeerSignal::Offer(sdp) => serde_json::json!({ "Offer": sdp }),
            PeerSignal::Answer(sdp) => serde_json::json!({ "Answer": sdp }),
            PeerSignal::IceCandidate(candidate) => {
                serde_json::json!({ "IceCandidate": candidate })
            }
        }
    }
}

impl TryFrom<serde_json::Value> for PeerSignal {
    type Error = serde_json::Error;

    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        serde_json::from_value(value)
    }
}

impl TryFrom<&serde_json::Value> for PeerSignal {
    type Error = serde_json::Error;

    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        serde_json::from_value(value.clone())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    #[test]
    fn offer_serializes_externally_tagged() {
        let json = serde_json::to_string(&PeerSignal::Offer("SDP".into())).expect("serialize");
        assert_eq!(json, r#"{"Offer":"SDP"}"#);
    }

    #[test]
    fn answer_serializes_externally_tagged() {
        let json = serde_json::to_string(&PeerSignal::Answer("SDP".into())).expect("serialize");
        assert_eq!(json, r#"{"Answer":"SDP"}"#);
    }

    #[test]
    fn ice_candidate_serializes_externally_tagged() {
        let json =
            serde_json::to_string(&PeerSignal::IceCandidate("cand".into())).expect("serialize");
        assert_eq!(json, r#"{"IceCandidate":"cand"}"#);
    }

    #[test]
    fn deserializes_from_external_tag_literals() {
        // The INBOUND parse must match the matchbox wire form exactly — a
        // Deserialize impl that diverged from Serialize would otherwise slip.
        assert_eq!(
            serde_json::from_str::<PeerSignal>(r#"{"Offer":"x"}"#).expect("deser offer"),
            PeerSignal::Offer("x".into())
        );
        assert_eq!(
            serde_json::from_str::<PeerSignal>(r#"{"Answer":"y"}"#).expect("deser answer"),
            PeerSignal::Answer("y".into())
        );
        assert_eq!(
            serde_json::from_str::<PeerSignal>(r#"{"IceCandidate":"z"}"#).expect("deser ice"),
            PeerSignal::IceCandidate("z".into())
        );
    }

    #[test]
    fn from_matches_serialize_for_every_variant() {
        // The hand-written `From` must stay byte-for-byte identical to serde's
        // derived `Serialize`; if a variant is ever added to `PeerSignal`
        // without extending `From`, this pins the divergence. (Also proves the
        // conversion never yields `Null`.)
        for sig in [
            PeerSignal::Offer("a".into()),
            PeerSignal::Answer("b".into()),
            PeerSignal::IceCandidate("c".into()),
        ] {
            let via_from: serde_json::Value = sig.clone().into();
            let via_serialize = serde_json::to_value(&sig).expect("serialize");
            assert_eq!(via_from, via_serialize, "From diverged from Serialize");
            assert!(!via_from.is_null(), "From must never yield Null");
        }
    }

    #[test]
    fn value_round_trip() {
        for sig in [
            PeerSignal::Offer("a".into()),
            PeerSignal::Answer("b".into()),
            PeerSignal::IceCandidate("c".into()),
        ] {
            let value: serde_json::Value = sig.clone().into();
            let back = PeerSignal::try_from(&value).expect("try_from value");
            assert_eq!(back, sig);
        }
    }

    #[test]
    fn try_from_unknown_shape_errors() {
        let value = serde_json::json!({ "Renegotiate": true });
        assert!(PeerSignal::try_from(value).is_err());
    }
}
