import assert from "node:assert/strict";
import test from "node:test";

import {
  validateFortressPair,
  validateFortressPeer,
  validateFinalSlope,
  validateLoadSummary,
  validateServerConservation,
} from "./godot-e2e-validators.mjs";

function peer(role) {
  return {
    passed: true, role, target_frames: 600, game_frame: 600, checksum_through: 600,
    settlement_frame_limit: 620, session_timeout_ms: 40_000, simulation_elapsed_ms: 20_000,
    simulation_target_fps: 18, observed_simulation_fps: 18,
    startup_barrier_completed: true,
    startup_start_unix_ms: 1_750_000_000_000,
    startup_proposal_sent: role === "b", startup_proposal_received: role === "a",
    startup_ack_sent: role === "a", startup_ack_received: role === "b",
    startup_commit_sent: role === "b", startup_commit_received: role === "a",
    startup_barrier_release_local_frame: 0, startup_barrier_elapsed_ms: 2_000,
    startup_release_lateness_ms: 20,
    max_poll_us: 1_000, multi_frame_poll: true, peak_queue_depth: 4,
    confirmed_input_checksum: "123", target_state_checksum: "456", game_ready: true,
    sync_in_sync: true, queue_depth: 0, current_queue_age_ms: 0, peak_queue_age_ms: 40,
    relay_inbound_depth: 0, relay_outbound_depth: 0, confirmation_lag_current: 0,
    confirmation_lag_max: 12, confirmation_lag_warmup_frames: 60,
    confirmation_lag_warmup_max: 12, confirmation_lag_steady_max: 6,
    wait_recommendation_count: 0, stall_count: 0,
    relay_messages_per_simulated_frame: 2.5, relay_dropped: 0, relay_malformed: 0,
    backend_capacity_hits: 0, admission_watermark_violations: 0, checksums_compared: 10,
    checksums_matched: 10, checksums_mismatched: 0, events_discarded: 0,
    desync_events: 0, frames_advanced: 610, visual_frames: 600, resimulated_frames: 10,
    local_id: role === "a" ? "a" : "b", remote_id: role === "a" ? "b" : "a",
    relay_encoded: 1_500, relay_decoded: 1_500, impairment_activated: true,
    impairment_released: true, rollback_count: 2, pre_impairment_rollback_count: 0,
    prediction_miss_count: 2, pre_impairment_prediction_miss_count: 0,
    pre_impairment_resimulated_frames: 0, max_rollback_depth: 1, load_requests: 2,
    peer_left_observed: role === "a", peer_left_epoch: role === "a" ? 1 : null,
    peer_left_final_seq: role === "a" ? 10 : null,
    poll_hitch_completed: true, poll_hitch_frames_advanced: 4,
  };
}

test("load oracle rejects independently corrupted age and admission fields", () => {
  const summary = {
    passed: true, final_queue_depth: 0, current_queue_age_ms: 0, peak_queue_age_ms: 100,
    final_drained_samples: 8,
    load_error: false, p99_latency_us: 100_000, max_poll_us: 1_000, buffering_safe: true,
    admission_watermark_violations: 0, offered_per_client: [2_176, 2_176],
    received_per_client: [2_176, 2_176], peak_queue_depth: 32,
    peak_aggregate_queue_depth: 64, per_client_peak_queue_depth: [32, 32],
    multi_frame_poll: true, buffered_bytes: 0, accepted_frames: 4_352, admission_hits: 0,
    within_absolute_adaptive_ceiling: true,
    binary_pair_admission_watermark_violations: 0,
    binary_pair_one_frame_escape_bytes: 64,
    binary_pair_within_absolute_adaptive_ceiling: true,
    binary_pair_peak_buffered_bytes: [1_000, 1_000],
    binary_pair_effective_watermark_bytes: [4_096, 4_096],
    binary_pair_per_client_escape_bytes: [32, 32],
    per_client_peak_buffered_bytes: [1_000, 1_000],
    per_client_effective_watermark_bytes: [4_096, 4_096],
    per_client_one_frame_escape_bytes: [32, 32], one_frame_escape_bytes: 128,
    max_poll_work_frames: 64, max_poll_work_bytes: 65_536, max_poll_receive_frames: 64,
  };
  const samples = Array.from({ length: 8 }, (_, index) => ({
    elapsed_ms: index * 10, command_depth: 0, current_queue_age_ms: 0,
    poll_work_frames: 64, poll_work_bytes: 65_536, poll_receive_frames: 64,
    send_budget_exhaustions: 0, receive_budget_exhaustions: 0,
  }));
  assert.equal(validateLoadSummary(summary, samples).ok, true);
  for (const [label, mutation] of [
    ["current age", (value) => { value.current_queue_age_ms = 1; }],
    ["drained samples", (value) => { value.final_drained_samples = 7; }],
    ["peak age", (value) => { value.peak_queue_age_ms = 501; }],
    ["admission", (value) => { value.admission_watermark_violations = 1; }],
    ["offered conservation", (value) => { value.offered_per_client[0] -= 1; }],
    ["received schema", (value) => { delete value.received_per_client; }],
    ["multi-frame poll", (value) => { value.multi_frame_poll = false; }],
    ["aggregate depth", (value) => { value.peak_aggregate_queue_depth = 65; }],
    ["per-client depth", (value) => { value.per_client_peak_queue_depth[0] = 33; }],
    ["undrained bytes", (value) => { value.buffered_bytes = 1; }],
    ["accepted frames", (value) => { value.accepted_frames = 4_351; }],
    ["adaptive ceiling", (value) => { value.within_absolute_adaptive_ceiling = false; }],
    ["binary admission", (value) => { value.binary_pair_admission_watermark_violations = 1; }],
    ["buffer schema", (value) => { delete value.per_client_peak_buffered_bytes; }],
    ["binary buffer schema", (value) => { delete value.binary_pair_peak_buffered_bytes; }],
    ["buffer ceiling", (value) => { value.per_client_peak_buffered_bytes[0] = 32_769; }],
    ["binary buffer ceiling", (value) => {
      value.binary_pair_peak_buffered_bytes[0] = 32_769;
    }],
    ["escape conservation", (value) => { value.one_frame_escape_bytes -= 1; }],
    ["missing latency", (value) => { delete value.p99_latency_us; }],
    ["missing callback", (value) => { delete value.max_poll_us; }],
    ["summary send work", (value) => { value.max_poll_work_frames = 65; }],
    ["summary receive work", (value) => { delete value.max_poll_receive_frames; }],
  ]) {
    const corrupted = structuredClone(summary);
    mutation(corrupted);
    assert.equal(validateLoadSummary(corrupted, samples).ok, false, label);
  }
  assert.equal(validateLoadSummary(summary, samples.slice(1)).ok, false, "requires eight samples");
  const staleSamples = structuredClone(samples);
  staleSamples.forEach((sample, index) => { sample.current_queue_age_ms = index; });
  assert.equal(validateLoadSummary(summary, staleSamples).ok, false);
  const flatButNotDrained = structuredClone(samples);
  flatButNotDrained.forEach((sample) => {
    sample.command_depth = 1;
    sample.current_queue_age_ms = 1;
  });
  assert.equal(validateLoadSummary(summary, flatButNotDrained).ok, false);
  for (const [label, field, value] of [
    ["send frame work", "poll_work_frames", 65],
    ["send byte work", "poll_work_bytes", 65_537],
    ["receive work", "poll_receive_frames", 65],
    ["budget schema", "send_budget_exhaustions", undefined],
  ]) {
    const overBudget = structuredClone(samples);
    if (value === undefined) delete overBudget[0][field];
    else overBudget[0][field] = value;
    assert.equal(validateLoadSummary(summary, overBudget).ok, false, label);
  }
});

test("Fortress oracle rejects each critical negative control", () => {
  const first = peer("a");
  const second = peer("b");
  assert.equal(validateFortressPeer(first).ok, true);
  assert.equal(validateFortressPair(first, second).ok, true);
  const impairedBoundary = structuredClone(first);
  impairedBoundary.confirmation_lag_current = 13;
  impairedBoundary.confirmation_lag_steady_max = 13;
  impairedBoundary.confirmation_lag_max = 13;
  assert.equal(validateFortressPeer(impairedBoundary, { lagLimit: 13 }).ok, true);
  impairedBoundary.confirmation_lag_steady_max = 14;
  assert.equal(validateFortressPeer(impairedBoundary, { lagLimit: 13 }).ok, false);
  const lifetimeBoundary = structuredClone(first);
  lifetimeBoundary.confirmation_lag_max = 20;
  lifetimeBoundary.confirmation_lag_warmup_max = 20;
  assert.equal(validateFortressPeer(lifetimeBoundary).ok, true);
  lifetimeBoundary.confirmation_lag_max = 21;
  assert.equal(validateFortressPeer(lifetimeBoundary).ok, false);

  for (const [label, mutation] of [
    ["frame confirmation", (value) => { value.checksum_through = 599; }],
    ["current queue age", (value) => { value.current_queue_age_ms = 1; }],
    ["peak queue age", (value) => { value.peak_queue_age_ms = 501; }],
    ["current lag", (value) => { value.confirmation_lag_current = 9; }],
    ["steady maximum lag", (value) => { value.confirmation_lag_steady_max = 9; }],
    ["warmup maximum lag", (value) => { value.confirmation_lag_warmup_max = 21; }],
    ["warmup schema", (value) => { delete value.confirmation_lag_warmup_frames; }],
    ["lifetime maximum lag", (value) => { value.confirmation_lag_max = 21; }],
    ["stale phase accumulators", (value) => {
      value.confirmation_lag_max = 20;
      value.confirmation_lag_warmup_max = 0;
      value.confirmation_lag_steady_max = 0;
    }],
    ["wait", (value) => { value.wait_recommendation_count = 1; }],
    ["stall", (value) => { value.stall_count = 1; }],
    ["admission", (value) => { value.admission_watermark_violations = 1; }],
    ["settlement schema", (value) => { delete value.settlement_frame_limit; }],
    ["timeout schema", (value) => { delete value.session_timeout_ms; }],
    ["callback schema", (value) => { delete value.max_poll_us; }],
    ["local identity schema", (value) => { delete value.local_id; }],
    ["remote identity schema", (value) => { value.remote_id = ""; }],
    ["distinct identities", (value) => { value.remote_id = value.local_id; }],
    ["startup completion", (value) => { value.startup_barrier_completed = false; }],
    ["startup local frame", (value) => { value.startup_barrier_release_local_frame = 1; }],
    ["startup elapsed time", (value) => { delete value.startup_barrier_elapsed_ms; }],
    ["startup proposal receipt", (value) => { value.startup_proposal_received = false; }],
    ["startup ack send", (value) => { value.startup_ack_sent = false; }],
    ["startup commit receipt", (value) => { value.startup_commit_received = false; }],
    ["startup lateness", (value) => { value.startup_release_lateness_ms = 101; }],
    ["simulation cadence", (value) => { value.observed_simulation_fps = 11; }],
    ["queue-depth schema", (value) => { delete value.peak_queue_depth; }],
    ["age schema", (value) => { delete value.peak_queue_age_ms; }],
    ["lag schema", (value) => { delete value.confirmation_lag_current; }],
    ["cadence schema", (value) => { delete value.relay_messages_per_simulated_frame; }],
    ["hitch advancement", (value) => { value.poll_hitch_frames_advanced = 0; }],
  ]) {
    const corrupted = structuredClone(first);
    mutation(corrupted);
    assert.equal(validateFortressPeer(corrupted, { requireHitch: true }).ok, false, label);
  }

  const divergentStartup = structuredClone(second);
  divergentStartup.startup_start_unix_ms += 1;
  assert.equal(validateFortressPair(first, divergentStartup).ok, false, "startup deadline");
  const divergentReleasePhase = structuredClone(second);
  divergentReleasePhase.startup_release_lateness_ms = 80;
  assert.equal(validateFortressPair(first, divergentReleasePhase).ok, false, "startup phase");
  for (const [label, field] of [
    ["proposal send", "startup_proposal_sent"],
    ["ack receipt", "startup_ack_received"],
    ["commit send", "startup_commit_sent"],
  ]) {
    const corrupted = structuredClone(second);
    corrupted[field] = false;
    assert.equal(validateFortressPeer(corrupted).ok, false, label);
  }

  for (const [label, mutation] of [
    ["checksum", (value) => { value.target_state_checksum = "different"; }],
    ["delivery count", (value) => { value.relay_decoded -= 1; }],
    ["teardown watermark", (value) => { value.peer_left_final_seq = 0; }],
    ["teardown schema", (value) => { delete value.peer_left_epoch; }],
    ["rollback schema", (value) => { delete value.rollback_count; }],
    ["resimulation schema", (value) => { delete value.resimulated_frames; }],
    ["prediction schema", (value) => { delete value.prediction_miss_count; }],
    ["rollback depth schema", (value) => { delete value.max_rollback_depth; }],
    ["load schema", (value) => { delete value.load_requests; }],
  ]) {
    const corrupted = structuredClone(first);
    mutation(corrupted);
    assert.equal(validateFortressPair(corrupted, second).ok, false, label);
  }
});

test("final slope oracle requires eight samples and rejects growth", () => {
  const samples = Array.from({ length: 8 }, (_, elapsed_ms) => ({
    elapsed_ms,
    queue_age_ms: 8 - elapsed_ms,
  }));
  assert.equal(validateFinalSlope(samples, "queue_age_ms").ok, true);
  assert.equal(validateFinalSlope(samples.slice(1), "queue_age_ms").ok, false);
  const growing = structuredClone(samples);
  growing.forEach((sample, index) => { sample.queue_age_ms = index; });
  assert.equal(validateFinalSlope(growing, "queue_age_ms").ok, false);
  const regressingClock = structuredClone(samples);
  regressingClock[6].elapsed_ms = 2;
  assert.equal(validateFinalSlope(regressingClock, "queue_age_ms").ok, false);
  const frozenClock = structuredClone(samples);
  frozenClock.forEach((sample) => { sample.elapsed_ms = 1; });
  assert.equal(validateFinalSlope(frozenClock, "queue_age_ms").ok, false);
});

test("server oracle rejects delivery-count corruption", () => {
  const values = {
    expectedGameData: 10, gameDataForwarded: 10, reliableAttempted: 10,
    reliableDelivered: 10, deliveryAttempts: 10, deliveryTerminals: 10,
    harmfulDeltas: [],
  };
  assert.equal(validateServerConservation(values).ok, true);
  for (const field of ["gameDataForwarded", "reliableAttempted", "deliveryTerminals"]) {
    const corrupted = structuredClone(values);
    corrupted[field] -= 1;
    assert.equal(validateServerConservation(corrupted).ok, false);
  }
});
