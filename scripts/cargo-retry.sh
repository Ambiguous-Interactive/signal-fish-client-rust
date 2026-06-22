#!/usr/bin/env bash
# Retry a Cargo subcommand that may touch the network.
#
# Cargo has its own retry setting, but CI can still see one-shot failures while
# downloading sparse registry entries. This wrapper keeps those transient
# failures from failing a whole workflow while preserving the original exit code
# when the command keeps failing.

set -u

ATTEMPTS="${CARGO_RETRY_ATTEMPTS:-3}"
DELAY_SECONDS="${CARGO_RETRY_DELAY_SECONDS:-2}"

case "$ATTEMPTS" in
    ''|*[!0-9]*|0)
        echo "CARGO_RETRY_ATTEMPTS must be a positive integer, got '$ATTEMPTS'." >&2
        exit 2
        ;;
esac

case "$DELAY_SECONDS" in
    ''|*[!0-9]*)
        echo "CARGO_RETRY_DELAY_SECONDS must be a non-negative integer, got '$DELAY_SECONDS'." >&2
        exit 2
        ;;
esac

if [ "$#" -eq 0 ]; then
    echo "Usage: $0 <cargo-subcommand> [args...]" >&2
    exit 2
fi

attempt=1
while [ "$attempt" -le "$ATTEMPTS" ]; do
    cargo "$@"
    status=$?
    if [ "$status" -eq 0 ]; then
        exit 0
    fi

    if [ "$attempt" -eq "$ATTEMPTS" ]; then
        echo "cargo $* failed after $ATTEMPTS attempt(s); exit code $status." >&2
        exit "$status"
    fi

    echo "cargo $* failed on attempt $attempt/$ATTEMPTS; retrying in ${DELAY_SECONDS}s." >&2
    sleep "$DELAY_SECONDS"
    attempt=$((attempt + 1))
    DELAY_SECONDS=$((DELAY_SECONDS * 2))
done
