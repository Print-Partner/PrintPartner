#!/usr/bin/env bash
# One pp-iso slot: ask GitHub for a one-job JIT runner config, start a fresh container with
# it, wait for the job, delete the container, repeat. Runs under pp-iso-runner@.service.
#
# The registration credential (a token with repository Administration permission) is read
# from a file and used only here, on the host. The container gets only the JIT config for
# one runner, which GitHub accepts for a single job.
set -euo pipefail

slot=${1:?usage: pp-iso-loop.sh <slot>}
: "${PP_ISO_REPO:?set PP_ISO_REPO=owner/name in /etc/pp-iso/runner.env}"
: "${PP_ISO_IMAGE:?set PP_ISO_IMAGE in /etc/pp-iso/runner.env}"
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
engine=${PP_ISO_ENGINE:-podman}
api=${PP_ISO_API:-https://api.github.com}
cpus=${PP_ISO_CPUS:-4}
memory=${PP_ISO_MEMORY:-14g}
pids=${PP_ISO_PIDS:-4096}
max_job_seconds=${PP_ISO_MAX_JOB_SECONDS:-21600}
max_jobs=${PP_ISO_MAX_JOBS:-0}
token_file=${PP_ISO_TOKEN_FILE:-${CREDENTIALS_DIRECTORY:-/etc/pp-iso}/token}
scratch=${RUNTIME_DIRECTORY:-${TMPDIR:-/tmp}}

[ -r "$token_file" ] || { echo "cannot read the token file $token_file" >&2; exit 1; }
umask 077
header=$(mktemp "$scratch/pp-iso-header.XXXXXX")
name=
cleanup() {
  rm -f "$header"
  [ -z "$name" ] || "$engine" rm -f "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 143' TERM INT

jobs_done=0
failures=0
while [ "$max_jobs" -eq 0 ] || [ "$jobs_done" -lt "$max_jobs" ]; do
  name="pp-iso-$(hostname -s)-$slot-$(date +%s)-$RANDOM"
  printf 'Authorization: Bearer %s\n' "$(tr -d '\n' < "$token_file")" > "$header"
  body=$(printf '{"name":"%s","runner_group_id":1,"labels":["self-hosted","linux","x64","pp-iso"],"work_folder":"_work"}' "$name")
  if ! response=$(curl -fsS --max-time 30 -X POST -H @"$header" -H 'Accept: application/vnd.github+json' \
        -H 'X-GitHub-Api-Version: 2022-11-28' -d "$body" "$api/repos/$PP_ISO_REPO/actions/runners/generate-jitconfig"); then
    failures=$((failures + 1))
    delay=$((failures < 6 ? 5 * (1 << failures) : 300))
    echo "pp-iso slot $slot: the JIT config request failed ($failures in a row); retrying in $delay s" >&2
    name=
    sleep "$delay"
    continue
  fi
  failures=0
  jit=$(printf '%s' "$response" | jq -er .encoded_jit_config)

  echo "pp-iso slot $slot: starting container $name"
  # No volume, no host network, no docker socket, no host devices. The JIT config is passed by
  # name so it does not show in the process list. The container is removed even if it is killed.
  ACTIONS_RUNNER_INPUT_JITCONFIG=$jit timeout --kill-after=60 "$max_job_seconds" "$engine" run --rm \
    --name "$name" --hostname "$name" \
    --cap-drop=ALL --cap-add=CHOWN --cap-add=DAC_OVERRIDE --cap-add=FOWNER --cap-add=SETUID \
    --cap-add=SETGID --cap-add=KILL --cap-add=FSETID --cap-add=SETFCAP --cap-add=SYS_CHROOT \
    --cpus "$cpus" --memory "$memory" --memory-swap "$memory" --pids-limit "$pids" \
    --label "pp-iso.slot=$slot" \
    --env ACTIONS_RUNNER_INPUT_JITCONFIG \
    "$PP_ISO_IMAGE" || echo "pp-iso slot $slot: container $name exited with status $?" >&2
  "$engine" rm -f "$name" >/dev/null 2>&1 || true
  name=
  unset jit response
  jobs_done=$((jobs_done + 1))
  sleep 1
done
