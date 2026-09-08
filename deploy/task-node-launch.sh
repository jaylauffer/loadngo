#!/usr/bin/env bash
set -euo pipefail

: "${LOADNGO_TASK_NODE_BINARY:?LOADNGO_TASK_NODE_BINARY is required}"
: "${LOADNGO_TASK_NODE_WORKDIR:?LOADNGO_TASK_NODE_WORKDIR is required}"
: "${LOADNGO_TASK_NODE_ID:?LOADNGO_TASK_NODE_ID is required}"
: "${LOADNGO_TASK_NODE_REPLY_ENDPOINTS:?LOADNGO_TASK_NODE_REPLY_ENDPOINTS is required}"
: "${LOADNGO_TASK_NODE_EXEC_COMMAND:?LOADNGO_TASK_NODE_EXEC_COMMAND is required}"

cd "${LOADNGO_TASK_NODE_WORKDIR}"

cmd=(
  "${LOADNGO_TASK_NODE_BINARY}"
  --worker-node-id "${LOADNGO_TASK_NODE_ID}"
  --execute-command "${LOADNGO_TASK_NODE_EXEC_COMMAND}"
)

for endpoint in ${LOADNGO_TASK_NODE_REPLY_ENDPOINTS}; do
  cmd+=(--reply-endpoint "${endpoint}")
done

for group in ${LOADNGO_TASK_NODE_MULTICAST_V6:-}; do
  cmd+=(--multicast-v6 "${group}")
done

for group in ${LOADNGO_TASK_NODE_MULTICAST_V4:-}; do
  cmd+=(--multicast-v4 "${group}")
done

for capability in ${LOADNGO_TASK_NODE_CAPABILITIES:-}; do
  cmd+=(--capability "${capability}")
done

if [[ -n "${LOADNGO_TASK_NODE_BIND_PORT:-}" ]]; then
  cmd+=(--bind-port "${LOADNGO_TASK_NODE_BIND_PORT}")
fi

if [[ -n "${LOADNGO_TASK_NODE_ARTIFACT_HINT:-}" ]]; then
  cmd+=(--artifact-hint "${LOADNGO_TASK_NODE_ARTIFACT_HINT}")
fi

if [[ -n "${LOADNGO_TASK_NODE_NOTE:-}" ]]; then
  cmd+=(--note "${LOADNGO_TASK_NODE_NOTE}")
fi

if [[ -n "${LOADNGO_TASK_NODE_RESULT_NOTE:-}" ]]; then
  cmd+=(--result-note "${LOADNGO_TASK_NODE_RESULT_NOTE}")
fi

if [[ -n "${LOADNGO_TASK_NODE_ESTIMATED_DURATION_SECONDS:-}" ]]; then
  cmd+=(--estimated-duration-seconds "${LOADNGO_TASK_NODE_ESTIMATED_DURATION_SECONDS}")
fi

if [[ -n "${LOADNGO_TASK_NODE_MAX_STATUS_INTERVAL_SECONDS:-}" ]]; then
  cmd+=(--max-status-interval-seconds "${LOADNGO_TASK_NODE_MAX_STATUS_INTERVAL_SECONDS}")
fi

if [[ -n "${LOADNGO_TASK_NODE_ACK_TIMEOUT_SECONDS:-}" ]]; then
  cmd+=(--ack-timeout-seconds "${LOADNGO_TASK_NODE_ACK_TIMEOUT_SECONDS}")
fi

if [[ -n "${LOADNGO_TASK_NODE_IDLE_INTERVAL_MILLIS:-}" ]]; then
  cmd+=(--idle-interval-millis "${LOADNGO_TASK_NODE_IDLE_INTERVAL_MILLIS}")
fi

if [[ -n "${LOADNGO_TASK_NODE_RUN_SECONDS:-}" ]]; then
  cmd+=(--run-seconds "${LOADNGO_TASK_NODE_RUN_SECONDS}")
fi

exec "${cmd[@]}"
