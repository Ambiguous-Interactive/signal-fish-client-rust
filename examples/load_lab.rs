//! Load & latency lab for the Signal Fish relay path.
//!
//! A small measurement harness for running controlled experiments against a
//! **real** Signal Fish server (never run it against a production deployment
//! you don't own). Produces CSV on stdout for analysis.
//!
//! ```text
//! cargo run --example load_lab --features transport-websocket -- <mode> [key=value ...]
//!
//! modes
//!   ping                 baseline Ping→Pong RTT on an idle connection
//!   throughput           offered-rate sweep: accepted/delivered rates + latency knee
//!   slow-consumer        one slow-draining room member; measures how much it
//!                        paces the sender and the healthy recipients
//!   control-starvation   Pong RTT at a backlogged recipient vs baseline
//!
//! common options (defaults)
//!   url=ws://127.0.0.1:3536/v2/ws   app=load-lab-app   payload=256   secs=10
//! throughput options
//!   rates=50,100,200,400,800,1600   recipients=3
//! slow-consumer options
//!   rate=120   drain_ms=100
//! control-starvation options
//!   drain_ms=5   ping_every_ms=1000
//! ```
//!
//! The server must accept the app id (run it with
//! `SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH=false` and
//! `SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__ENFORCE=false` for a local lab
//! instance). Timestamps ride inside the payload, so latency is measured on
//! one process clock: run all roles from this single binary.

use std::error::Error;
use std::time::{Duration, Instant};

use signal_fish_client::{
    JoinRoomParams, SignalFishClient, SignalFishConfig, SignalFishEvent, WebSocketTransport,
};

// ── Options ─────────────────────────────────────────────────────────

#[derive(Clone)]
struct Options {
    mode: String,
    url: String,
    app: String,
    payload: usize,
    secs: u64,
    rates: Vec<u32>,
    recipients: usize,
    rate: u32,
    drain_ms: u64,
    ping_every_ms: u64,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, Box<dyn Error>> {
        let mode = args.first().cloned().ok_or(
            "usage: load_lab <ping|throughput|slow-consumer|control-starvation> [key=value ...]",
        )?;
        let mut opts = Self {
            mode,
            url: "ws://127.0.0.1:3536/v2/ws".to_string(),
            app: "load-lab-app".to_string(),
            payload: 256,
            secs: 10,
            rates: vec![50, 100, 200, 400, 800, 1600],
            recipients: 3,
            rate: 120,
            drain_ms: 100,
            ping_every_ms: 1000,
        };
        for arg in args.iter().skip(1) {
            let (key, value) = arg
                .split_once('=')
                .ok_or_else(|| format!("expected key=value, got `{arg}`"))?;
            match key {
                "url" => opts.url = value.to_string(),
                "app" => opts.app = value.to_string(),
                "payload" => opts.payload = value.parse()?,
                "secs" => opts.secs = value.parse()?,
                "rates" => {
                    opts.rates = value
                        .split(',')
                        .map(str::parse)
                        .collect::<Result<Vec<u32>, _>>()?;
                }
                "recipients" => opts.recipients = value.parse()?,
                "rate" => opts.rate = value.parse()?,
                "drain_ms" => opts.drain_ms = value.parse()?,
                "ping_every_ms" => opts.ping_every_ms = value.parse()?,
                other => return Err(format!("unknown option `{other}`").into()),
            }
        }
        Ok(opts)
    }
}

// ── Small helpers ───────────────────────────────────────────────────

fn epoch() -> &'static Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(Instant::now)
}

fn now_nanos() -> u64 {
    u64::try_from(epoch().elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn payload_with_timestamp(pad: usize) -> serde_json::Value {
    serde_json::json!({ "t": now_nanos(), "pad": "x".repeat(pad) })
}

fn latency_ms_from(data: &serde_json::Value) -> Option<f64> {
    let sent = data.get("t")?.as_u64()?;
    let now = now_nanos();
    Some((now.saturating_sub(sent)) as f64 / 1_000_000.0)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted.get(idx).copied().unwrap_or(f64::NAN)
}

fn summarize(mut samples: Vec<f64>) -> (usize, f64, f64, f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = samples.len();
    let max = samples.last().copied().unwrap_or(f64::NAN);
    (
        n,
        percentile(&samples, 0.50),
        percentile(&samples, 0.95),
        percentile(&samples, 0.99),
        max,
    )
}

async fn connect(
    opts: &Options,
    name: &str,
    event_capacity: usize,
) -> Result<
    (
        SignalFishClient,
        tokio::sync::mpsc::Receiver<SignalFishEvent>,
    ),
    Box<dyn Error>,
> {
    let transport = WebSocketTransport::connect(&opts.url).await?;
    let config =
        SignalFishConfig::new(opts.app.clone()).with_event_channel_capacity(event_capacity);
    let (client, mut events) = SignalFishClient::start(transport, config);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(SignalFishEvent::Authenticated { .. })) => break,
            Ok(Some(SignalFishEvent::AuthenticationError { error, .. })) => {
                return Err(format!("{name}: authentication failed: {error}").into());
            }
            Ok(Some(_)) => {}
            _ => return Err(format!("{name}: no Authenticated within 5s").into()),
        }
    }
    Ok((client, events))
}

async fn join(
    client: &SignalFishClient,
    events: &mut tokio::sync::mpsc::Receiver<SignalFishEvent>,
    game: &str,
    name: &str,
    room_code: Option<&str>,
) -> Result<String, Box<dyn Error>> {
    let mut params = JoinRoomParams::new(game, name).with_max_players(16);
    if let Some(code) = room_code {
        params = params.with_room_code(code);
    }
    client.join_room(params)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(SignalFishEvent::RoomJoined { room_code, .. })) => return Ok(room_code),
            Ok(Some(SignalFishEvent::RoomJoinFailed { reason, .. })) => {
                return Err(format!("{name}: join failed: {reason}").into());
            }
            Ok(Some(_)) => {}
            _ => return Err(format!("{name}: no RoomJoined within 5s").into()),
        }
    }
}

/// Drain a receiver's GameData latencies for `secs`, sleeping `drain_ms`
/// after each GameData event (0 = drain at full speed). Returns
/// (latencies_ms, received_count, saw_disconnect).
async fn drain_role(
    mut events: tokio::sync::mpsc::Receiver<SignalFishEvent>,
    secs: u64,
    drain_ms: u64,
) -> (Vec<f64>, u64, bool) {
    let mut latencies = Vec::new();
    let mut received = 0u64;
    let mut disconnected = false;
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        let remaining = end.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(SignalFishEvent::GameData { data, .. })) => {
                received += 1;
                if let Some(ms) = latency_ms_from(&data) {
                    latencies.push(ms);
                }
                if drain_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(drain_ms)).await;
                }
            }
            Ok(Some(SignalFishEvent::Disconnected { .. })) => {
                disconnected = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                disconnected = true;
                break;
            }
            Err(_) => break,
        }
    }
    (latencies, received, disconnected)
}

// ── Modes ───────────────────────────────────────────────────────────

async fn mode_ping(opts: &Options) -> Result<(), Box<dyn Error>> {
    let (client, mut events) = connect(opts, "pinger", 256).await?;
    let mut rtts = Vec::new();
    for _ in 0..20 {
        let sent = Instant::now();
        client.ping()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(remaining, events.recv()).await {
                Ok(Some(SignalFishEvent::Pong)) => break,
                Ok(Some(_)) => {}
                _ => return Err("no Pong within 3s".into()),
            }
        }
        rtts.push(sent.elapsed().as_secs_f64() * 1000.0);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (n, p50, p95, p99, max) = summarize(rtts);
    println!("mode,samples,p50_ms,p95_ms,p99_ms,max_ms");
    println!("ping,{n},{p50:.2},{p95:.2},{p99:.2},{max:.2}");
    Ok(())
}

async fn mode_throughput(opts: &Options) -> Result<(), Box<dyn Error>> {
    println!(
        "mode,rate_offered,payload_bytes,accepted_per_s,refused,delivered_per_s_per_recipient,p50_ms,p95_ms,p99_ms,max_ms"
    );
    for &rate in &opts.rates {
        let suffix = format!("{}-{rate}", std::process::id());
        let game = format!("lab-thr-{suffix}");

        let (sender, mut sender_events) = connect(opts, "sender", 1024).await?;
        let room_code = join(&sender, &mut sender_events, &game, "sender", None).await?;

        let mut receiver_handles = Vec::new();
        for i in 0..opts.recipients {
            let (rx_client, mut rx_events) = connect(opts, "receiver", 4096).await?;
            join(
                &rx_client,
                &mut rx_events,
                &game,
                &format!("rx{i}"),
                Some(&room_code),
            )
            .await?;
            let secs = opts.secs + 2; // outlast the sender to catch stragglers
            receiver_handles.push((
                rx_client,
                tokio::spawn(async move { drain_role(rx_events, secs, 0).await }),
            ));
        }

        // Paced sender: fail-fast sends, counting refusals as the
        // congestion signal.
        let mut interval = tokio::time::interval(Duration::from_secs_f64(1.0 / f64::from(rate)));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
        let end = Instant::now() + Duration::from_secs(opts.secs);
        let mut accepted = 0u64;
        let mut refused = 0u64;
        while Instant::now() < end {
            interval.tick().await;
            match sender.send_game_data(payload_with_timestamp(opts.payload)) {
                Ok(()) => accepted += 1,
                Err(_) => refused += 1,
            }
            // Keep the sender's own event stream drained.
            while sender_events.try_recv().is_ok() {}
        }
        let accepted_per_s = accepted as f64 / opts.secs as f64;

        let mut all_latencies = Vec::new();
        let mut delivered_total = 0u64;
        for (rx_client, handle) in receiver_handles {
            let (latencies, received, _) = handle.await?;
            delivered_total += received;
            all_latencies.extend(latencies);
            drop(rx_client);
        }
        let delivered_per_s =
            delivered_total as f64 / opts.secs as f64 / std::cmp::max(opts.recipients, 1) as f64;
        let (_, p50, p95, p99, max) = summarize(all_latencies);
        println!(
            "throughput,{rate},{},{accepted_per_s:.1},{refused},{delivered_per_s:.1},{p50:.2},{p95:.2},{p99:.2},{max:.2}",
            opts.payload
        );
        drop(sender);
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(())
}

async fn mode_slow_consumer(opts: &Options) -> Result<(), Box<dyn Error>> {
    let suffix = std::process::id();
    let game = format!("lab-slow-{suffix}");

    let (sender, mut sender_events) = connect(opts, "sender", 1024).await?;
    let room_code = join(&sender, &mut sender_events, &game, "sender", None).await?;

    // Two healthy receivers + one slow one (event capacity 1 so its
    // backpressure reaches the socket quickly).
    let mut healthy = Vec::new();
    for i in 0..2 {
        let (c, mut e) = connect(opts, "healthy", 4096).await?;
        join(&c, &mut e, &game, &format!("healthy{i}"), Some(&room_code)).await?;
        let secs = opts.secs;
        healthy.push((c, tokio::spawn(async move { drain_role(e, secs, 0).await })));
    }
    let (slow_client, mut slow_events) = connect(opts, "slow", 1).await?;
    join(
        &slow_client,
        &mut slow_events,
        &game,
        "slow",
        Some(&room_code),
    )
    .await?;
    let slow_secs = opts.secs;
    let slow_drain = opts.drain_ms;
    let slow_handle =
        tokio::spawn(async move { drain_role(slow_events, slow_secs, slow_drain).await });

    // Sender pushes at the target rate with the waiting (backpressured)
    // variant: its achieved rate reveals how hard the room paces it.
    let mut interval = tokio::time::interval(Duration::from_secs_f64(1.0 / f64::from(opts.rate)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    let end = Instant::now() + Duration::from_secs(opts.secs);
    let mut sent = 0u64;
    while Instant::now() < end {
        interval.tick().await;
        sender
            .send_game_data_reliable(payload_with_timestamp(opts.payload))
            .await?;
        sent += 1;
        while sender_events.try_recv().is_ok() {}
    }
    let achieved = sent as f64 / opts.secs as f64;

    println!(
        "mode,role,offered_per_s,achieved_per_s,received,p50_ms,p95_ms,p99_ms,max_ms,disconnected"
    );
    println!("slow-consumer,sender,{},{achieved:.1},,,,,,", opts.rate);
    for (i, (c, handle)) in healthy.into_iter().enumerate() {
        let (latencies, received, disconnected) = handle.await?;
        let (_, p50, p95, p99, max) = summarize(latencies);
        println!(
            "slow-consumer,healthy{i},,,{received},{p50:.2},{p95:.2},{p99:.2},{max:.2},{disconnected}"
        );
        drop(c);
    }
    let (latencies, received, disconnected) = slow_handle.await?;
    let (_, p50, p95, p99, max) = summarize(latencies);
    println!(
        "slow-consumer,slow(drain {}ms),,,{received},{p50:.2},{p95:.2},{p99:.2},{max:.2},{disconnected}",
        opts.drain_ms
    );
    drop(slow_client);
    Ok(())
}

async fn mode_control_starvation(opts: &Options) -> Result<(), Box<dyn Error>> {
    let suffix = std::process::id();
    let game = format!("lab-ctrl-{suffix}");

    // Victim joins first (owns the room), then the flooder.
    let (victim, mut victim_events) = connect(opts, "victim", 256).await?;
    let room_code = join(&victim, &mut victim_events, &game, "victim", None).await?;
    let (flooder, mut flooder_events) = connect(opts, "flooder", 1024).await?;
    join(
        &flooder,
        &mut flooder_events,
        &game,
        "flooder",
        Some(&room_code),
    )
    .await?;

    // Flood task: waiting sends as fast as the relay absorbs them.
    let payload_size = opts.payload;
    let flood_secs = opts.secs;
    let flood = tokio::spawn(async move {
        let end = Instant::now() + Duration::from_secs(flood_secs);
        let mut sent = 0u64;
        while Instant::now() < end {
            if flooder
                .send_game_data_reliable(payload_with_timestamp(payload_size))
                .await
                .is_err()
            {
                break;
            }
            sent += 1;
            while flooder_events.try_recv().is_ok() {}
        }
        sent
    });

    // Victim: drains slowly (drain_ms per GameData) and pings periodically.
    // Pong RTT includes both the server-side queueing behind relayed
    // GameData and the victim's own backlog — the end-to-end control-plane
    // delay a slow-but-alive client actually experiences.
    let mut pong_rtts: Vec<f64> = Vec::new();
    let mut game_latencies: Vec<f64> = Vec::new();
    let mut received = 0u64;
    let mut ping_outstanding: Option<Instant> = None;
    let mut next_ping = Instant::now();
    let end = Instant::now() + Duration::from_secs(opts.secs);
    while Instant::now() < end {
        if ping_outstanding.is_none() && Instant::now() >= next_ping {
            victim.ping()?;
            ping_outstanding = Some(Instant::now());
            next_ping = Instant::now() + Duration::from_millis(opts.ping_every_ms);
        }
        let remaining = end.saturating_duration_since(Instant::now());
        match tokio::time::timeout(
            remaining.min(Duration::from_millis(50)),
            victim_events.recv(),
        )
        .await
        {
            Ok(Some(SignalFishEvent::GameData { data, .. })) => {
                received += 1;
                if let Some(ms) = latency_ms_from(&data) {
                    game_latencies.push(ms);
                }
                if opts.drain_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(opts.drain_ms)).await;
                }
            }
            Ok(Some(SignalFishEvent::Pong)) => {
                if let Some(sent_at) = ping_outstanding.take() {
                    pong_rtts.push(sent_at.elapsed().as_secs_f64() * 1000.0);
                }
            }
            Ok(Some(SignalFishEvent::Disconnected { .. })) | Ok(None) => break,
            _ => {}
        }
    }
    let flooded = flood.await?;

    let (n_pong, pong_p50, pong_p95, pong_p99, pong_max) = summarize(pong_rtts);
    let (_, gd_p50, _, gd_p99, _) = summarize(game_latencies);
    println!(
        "mode,flooded_msgs,gamedata_received,gd_p50_ms,gd_p99_ms,pongs,pong_p50_ms,pong_p95_ms,pong_p99_ms,pong_max_ms"
    );
    println!(
        "control-starvation,{flooded},{received},{gd_p50:.2},{gd_p99:.2},{n_pong},{pong_p50:.2},{pong_p95:.2},{pong_p99:.2},{pong_max:.2}"
    );
    Ok(())
}

// ── Entry ───────────────────────────────────────────────────────────

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = Options::parse(&args)?;
    match opts.mode.as_str() {
        "ping" => mode_ping(&opts).await,
        "throughput" => mode_throughput(&opts).await,
        "slow-consumer" => mode_slow_consumer(&opts).await,
        "control-starvation" => mode_control_starvation(&opts).await,
        other => Err(format!(
            "unknown mode `{other}`; expected ping|throughput|slow-consumer|control-starvation"
        )
        .into()),
    }
}
