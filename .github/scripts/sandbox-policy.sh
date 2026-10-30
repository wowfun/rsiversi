# Source from a Bash CI step; the caller's EXIT owns policy restoration.
policy_state="${RUNNER_TEMP:?}/rsi-sandbox-policy.json"
rsi_restore_sandbox_policy() {
  local status=$?
  python3 "$GITHUB_WORKSPACE/.github/scripts/sandbox-policy.py" restore "$policy_state" || {
    if [[ $status == 0 ]]; then status=1; fi
  }
  exit "$status"
}
trap rsi_restore_sandbox_policy EXIT
python3 "${GITHUB_WORKSPACE:?}/.github/scripts/sandbox-policy.py" prepare "$policy_state" "${1:?sandbox policy profile is required}"
