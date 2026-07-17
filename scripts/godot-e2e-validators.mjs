export function linearSlope(samples, field) {
  if (samples.length < 2) return Number.NaN;
  if (samples.some((sample, index) =>
    !Number.isFinite(sample.elapsed_ms) || !Number.isFinite(sample[field]) ||
    (index > 0 && sample.elapsed_ms < samples[index - 1].elapsed_ms)
  )) return Number.NaN;
  const meanX = samples.reduce((sum, sample) => sum + sample.elapsed_ms, 0) / samples.length;
  const meanY = samples.reduce((sum, sample) => sum + sample[field], 0) / samples.length;
  const numerator = samples.reduce(
    (sum, sample) => sum + (sample.elapsed_ms - meanX) * (sample[field] - meanY),
    0,
  );
  const denominator = samples.reduce(
    (sum, sample) => sum + (sample.elapsed_ms - meanX) ** 2,
    0,
  );
  return denominator > 0 ? numerator / denominator : Number.NaN;
}

export function validateFinalSlope(samples, field) {
  const finalSamples = samples.slice(-8);
  const slope = linearSlope(finalSamples, field);
  return {
    ok: finalSamples.length === 8 && Number.isFinite(slope) && slope <= 0,
    slope,
    samples: finalSamples,
  };
}

function isNonnegativeNumber(value) {
  return Number.isFinite(value) && value >= 0;
}

function isNonnegativeInteger(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

export function validateLoadSummary(summary, samples) {
  const errors = [];
  const depth = validateFinalSlope(samples, "command_depth");
  const age = validateFinalSlope(samples, "current_queue_age_ms");
  const finalSamples = samples.slice(-8);
  if (summary?.passed !== true) errors.push("fixture summary failed");
  if (summary?.final_queue_depth !== 0) errors.push("final command queue was not drained");
  if (summary?.current_queue_age_ms !== 0) errors.push("final queue age was not zero");
  if (summary?.final_drained_samples !== 8 || finalSamples.length !== 8 ||
      finalSamples.some((sample) =>
        sample.command_depth !== 0 || sample.current_queue_age_ms !== 0)) {
    errors.push("final eight samples were not continuously drained");
  }
  if (!Number.isFinite(summary?.peak_queue_age_ms) || summary.peak_queue_age_ms > 500) {
    errors.push("peak queue age exceeded 500 ms");
  }
  if (!depth.ok) {
    errors.push("final command-depth slope was positive or unavailable");
  }
  if (!age.ok) {
    errors.push("final queue-age slope was positive or unavailable");
  }
  if (
    summary?.load_error !== false || !isNonnegativeNumber(summary?.p99_latency_us) ||
    summary.p99_latency_us > 500_000 || !isNonnegativeNumber(summary?.max_poll_us) ||
    summary.max_poll_us >= 50_000
  ) {
    errors.push("load latency, ordering, or callback bound failed");
  }
  if (summary?.buffering_safe !== true || summary?.admission_watermark_violations !== 0) {
    errors.push("transport admission diagnostics failed");
  }
  return { ok: errors.length === 0, errors, depthSlope: depth.slope, ageSlope: age.slope };
}

export function validateFortressPeer(summary, options = {}) {
  const targetFrames = options.targetFrames ?? 600;
  const settlementFrames = options.settlementFrames ?? 20;
  const lagLimit = options.lagLimit ?? 8;
  const sessionTimeoutMs = options.sessionTimeoutMs ?? 40_000;
  const errors = [];
  const decimal = /^\d+$/;
  const startupRoleValid = summary?.role === "a"
    ? summary?.startup_proposal_sent === false && summary?.startup_proposal_received === true &&
      summary?.startup_ack_sent === true && summary?.startup_ack_received === false &&
      summary?.startup_commit_sent === false && summary?.startup_commit_received === true
    : summary?.role === "b" && summary?.startup_proposal_sent === true &&
      summary?.startup_proposal_received === false && summary?.startup_ack_sent === false &&
      summary?.startup_ack_received === true && summary?.startup_commit_sent === true &&
      summary?.startup_commit_received === false;
  if (
    summary?.passed !== true || summary?.target_frames !== targetFrames ||
    summary?.settlement_frame_limit !== targetFrames + settlementFrames ||
    summary?.session_timeout_ms !== sessionTimeoutMs
  ) errors.push("state summary schema or scenario bounds failed");
  if (
    !isNonnegativeInteger(summary?.game_frame) ||
    !isNonnegativeInteger(summary?.checksum_through) ||
    summary?.game_frame < targetFrames ||
    summary?.game_frame > targetFrames + settlementFrames ||
    summary?.checksum_through !== targetFrames
  ) errors.push("target frame was not exactly confirmed");
  if (
    !decimal.test(summary?.confirmed_input_checksum ?? "") ||
    !decimal.test(summary?.target_state_checksum ?? "") ||
    summary?.game_ready !== true || summary?.sync_in_sync !== true
  ) errors.push("checksum or synchronization state failed");
  if (
    !isNonnegativeNumber(summary?.simulation_elapsed_ms) || summary.simulation_elapsed_ms === 0 ||
    summary.simulation_elapsed_ms >= sessionTimeoutMs ||
    summary?.simulation_target_fps !== 18 ||
    !isNonnegativeNumber(summary?.observed_simulation_fps) ||
    summary.observed_simulation_fps < 12 || summary.observed_simulation_fps > 20 ||
    !isNonnegativeNumber(summary?.max_poll_us) || summary.max_poll_us >= 50_000
  ) errors.push("simulation or callback timing bound failed");
  if (
    !startupRoleValid || summary?.startup_barrier_completed !== true ||
    summary?.startup_barrier_release_local_frame !== 0 ||
    !isNonnegativeInteger(summary?.startup_start_unix_ms) || summary.startup_start_unix_ms === 0 ||
    !isNonnegativeNumber(summary?.startup_barrier_elapsed_ms) ||
    !isNonnegativeNumber(summary?.startup_release_lateness_ms) ||
    summary.startup_release_lateness_ms > 100
  ) errors.push("causal startup barrier failed");
  if (
    summary?.multi_frame_poll !== true ||
    !isNonnegativeInteger(summary?.peak_queue_depth) || summary.peak_queue_depth > 64
  ) {
    errors.push("multi-frame scheduling or queue-depth bound failed");
  }
  if (
    summary?.queue_depth !== 0 || summary?.current_queue_age_ms !== 0 ||
    !isNonnegativeNumber(summary?.peak_queue_age_ms) || summary.peak_queue_age_ms > 500 ||
    summary?.relay_inbound_depth !== 0 ||
    summary?.relay_outbound_depth !== 0
  ) errors.push("client or relay queue failed to drain within age bounds");
  if (
    !isNonnegativeInteger(summary?.confirmation_lag_current) ||
    !isNonnegativeInteger(summary?.confirmation_lag_max) ||
    summary?.confirmation_lag_current > lagLimit || summary?.confirmation_lag_max > lagLimit ||
    summary?.wait_recommendation_count !== 0 || summary?.stall_count !== 0
  ) errors.push("gameplay confirmation lag, wait, or stall bound failed");
  if (
    !isNonnegativeNumber(summary?.relay_messages_per_simulated_frame) ||
    summary.relay_messages_per_simulated_frame < 2
  ) {
    errors.push("relay message cadence fell below two per simulated frame");
  }
  if (
    summary?.relay_dropped !== 0 || summary?.relay_malformed !== 0 ||
    summary?.backend_capacity_hits !== 0 || summary?.admission_watermark_violations !== 0 ||
    summary?.checksums_compared < 10 || summary?.checksums_matched !== summary?.checksums_compared ||
    summary?.checksums_mismatched !== 0 || summary?.events_discarded !== 0 ||
    summary?.desync_events !== 0 ||
    summary?.frames_advanced !== summary?.visual_frames + summary?.resimulated_frames
  ) errors.push("integrity, conservation, or admission diagnostics failed");
  if (
    options.requireHitch &&
    (!summary?.poll_hitch_completed || !isNonnegativeInteger(summary?.poll_hitch_frames_advanced) ||
      summary.poll_hitch_frames_advanced === 0)
  ) {
    errors.push("six-callback polling hitch did not preserve gameplay advancement");
  }
  return { ok: errors.length === 0, errors };
}

export function validateFortressPair(first, second) {
  const errors = [];
  if (first?.startup_start_unix_ms !== second?.startup_start_unix_ms) {
    errors.push("peer startup deadlines diverged");
  }
  if (
    !isNonnegativeNumber(first?.startup_release_lateness_ms) ||
    !isNonnegativeNumber(second?.startup_release_lateness_ms) ||
    Math.abs(first.startup_release_lateness_ms - second.startup_release_lateness_ms) > 56
  ) errors.push("peer startup release phases diverged");
  if (
    first?.target_state_checksum !== second?.target_state_checksum ||
    first?.confirmed_input_checksum !== second?.confirmed_input_checksum
  ) errors.push("peer checksums diverged");
  if (
    first?.local_id !== second?.remote_id || second?.local_id !== first?.remote_id ||
    first?.relay_encoded !== second?.relay_decoded ||
    second?.relay_encoded !== first?.relay_decoded
  ) errors.push("roster or relay delivery conservation failed");
  if (
    !second?.impairment_activated || !second?.impairment_released ||
    first?.rollback_count <= first?.pre_impairment_rollback_count ||
    first?.resimulated_frames <= first?.pre_impairment_resimulated_frames ||
    first?.prediction_miss_count <= first?.pre_impairment_prediction_miss_count ||
    first?.max_rollback_depth < 1 || first?.load_requests <= 0
  ) errors.push("deterministic rollback proof was absent");
  if (
    !first?.peer_left_observed || first?.peer_left_epoch <= 0 || first?.peer_left_final_seq <= 0
  ) errors.push("terminal teardown watermark was absent");
  return { ok: errors.length === 0, errors };
}

export function validateServerConservation(values) {
  const errors = [];
  if (
    values.gameDataForwarded !== values.expectedGameData ||
    values.reliableAttempted !== values.expectedGameData ||
    values.reliableDelivered !== values.expectedGameData ||
    values.deliveryAttempts !== values.deliveryTerminals ||
    values.harmfulDeltas.length > 0
  ) errors.push("server delivery conservation failed");
  return { ok: errors.length === 0, errors };
}
