#!/usr/bin/env bash
set -euo pipefail

artifact_root="${LOADNGO_TASK_NODE_ARTIFACT_ROOT:-/var/lib/loadngo-task-node/receipts}"
mkdir -p "${artifact_root}"

assignment_id="${LOADNGO_TASK_ASSIGNMENT_ID:?LOADNGO_TASK_ASSIGNMENT_ID is required}"
receipt_path="${artifact_root}/${assignment_id}.txt"

artifact_hint="${LOADNGO_TASK_ARTIFACT_HINT:-}"

{
  printf 'request_id=%s\n' "${LOADNGO_TASK_REQUEST_ID:-}"
  printf 'offer_id=%s\n' "${LOADNGO_TASK_OFFER_ID:-}"
  printf 'assignment_id=%s\n' "${assignment_id}"
  printf 'submitter_node_id=%s\n' "${LOADNGO_TASK_SUBMITTER_NODE_ID:-}"
  printf 'worker_node_id=%s\n' "${LOADNGO_TASK_WORKER_NODE_ID:-}"
  printf 'summary=%s\n' "${LOADNGO_TASK_SUMMARY:-}"
  printf 'success_criteria=%s\n' "${LOADNGO_TASK_SUCCESS_CRITERIA:-}"
  printf 'artifact_hint=%s\n' "${artifact_hint}"
  printf '\n'

  case "${artifact_hint}" in
    qcoin://*/tip)
      target="${artifact_hint#qcoin://}"
      target="${target%/tip}"
      cargo run -q -p qcoin-node --manifest-path /home/jay/pudding/qcoin/Cargo.toml -- node-info --target "${target}"
      printf '\n'
      cargo run -q -p qcoin-node --manifest-path /home/jay/pudding/qcoin/Cargo.toml -- tip --target "${target}"
      ;;
    repo-tip://*)
      repo_path="${artifact_hint#repo-tip://}"
      if [[ -d "${repo_path}/.git" ]]; then
        printf 'repo_branch='
        git -C "${repo_path}" branch --show-current
        printf 'repo_head='
        git -C "${repo_path}" rev-parse HEAD
        printf '\n'
        git -C "${repo_path}" status --short --branch
      else
        printf 'unsupported_repo_tip_path=%s\n' "${repo_path}"
      fi
      ;;
    *)
      printf 'unsupported_artifact_hint=%s\n' "${artifact_hint}"
      ;;
  esac
} > "${receipt_path}"

printf 'wrote %s\n' "${receipt_path}"
