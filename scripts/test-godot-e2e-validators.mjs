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
    max_poll_us: 1_000, multi_frame_poll: true, peak_queue_depth: 4,
    confirmed_input_checksum: "123", target_state_checksum: "456", game_ready: true,
    sync_in_sync: true, queue_depth: 0, current_queue_age_ms: 0, peak_queue_age_ms: 40,
    relay_inbound_depth: 0, relay_outbound_depth: 0, confirmation_lag_current: 0,
    confirmation_lag_max: 6, wait_recommendation_count: 0, stall_count: 0,
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
    admission_watermark_violations: 0,
  };
  const samples = Array.from({ length: 8 }, (_, index) => ({
    elapsed_ms: index * 10, command_depth: 0, current_queue_age_ms: 0,
  }));
  assert.equal(validateLoadSummary(summary, samples).ok, true);
  for (const [label, mutation] of [
    ["current age", (value) => { value.current_queue_age_ms = 1; }],
    ["drained samples", (value) => { value.final_drained_samples = 7; }],
    ["peak age", (value) => { value.peak_queue_age_ms = 501; }],
    ["admission", (value) => { value.admission_watermark_violations = 1; }],
    ["missing latency", (value) => { delete value.p99_latency_us; }],
    ["missing callback", (value) => { delete value.max_poll_us; }],
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
});

test("Fortress oracle rejects each critical negative control", () => {
  const first = peer("a");
  const second = peer("b");
  assert.equal(validateFortressPeer(first).ok, true);
  assert.equal(validateFortressPair(first, second).ok, true);
  const impairedBoundary = structuredClone(first);
  impairedBoundary.confirmation_lag_current = 13;
  impairedBoundary.confirmation_lag_max = 13;
  assert.equal(validateFortressPeer(impairedBoundary, { lagLimit: 13 }).ok, true);
  impairedBoundary.confirmation_lag_max = 14;
  assert.equal(validateFortressPeer(impairedBoundary, { lagLimit: 13 }).ok, false);

  for (const [label, mutation] of [
    ["frame confirmation", (value) => { value.checksum_through = 599; }],
    ["current queue age", (value) => { value.current_queue_age_ms = 1; }],
    ["peak queue age", (value) => { value.peak_queue_age_ms = 501; }],
    ["current lag", (value) => { value.confirmation_lag_current = 9; }],
    ["maximum lag", (value) => { value.confirmation_lag_max = 9; }],
    ["wait", (value) => { value.wait_recommendation_count = 1; }],
    ["stall", (value) => { value.stall_count = 1; }],
    ["admission", (value) => { value.admission_watermark_violations = 1; }],
    ["settlement schema", (value) => { delete value.settlement_frame_limit; }],
    ["timeout schema", (value) => { delete value.session_timeout_ms; }],
    ["callback schema", (value) => { delete value.max_poll_us; }],
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

  for (const [label, mutation] of [
    ["checksum", (value) => { value.target_state_checksum = "different"; }],
    ["delivery count", (value) => { value.relay_decoded -= 1; }],
    ["teardown watermark", (value) => { value.peer_left_final_seq = 0; }],
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
