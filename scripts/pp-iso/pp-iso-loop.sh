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
runlog=$(mktemp "$scratch/pp-iso-log.XXXXXX")
name=
cleanup() {
  rm -f "$header" "$runlog"
  [ -z "$name" ] || "$engine" rm -f "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 143' TERM INT

jobs_done=0
failures=0
while [ "$max_jobs" -eq 0 ] || [ "$jobs_done" -lt "$max_jobs" ]; do
  left=
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
  jit=$(printf '%s' "$response" | jq -er .encoded_jit_config)

  echo "pp-iso slot $slot: starting container $name"
  # No volume, no host network, no docker socket, no host devices. The JIT config is passed by
  # name so it does not show in the process list. The container is removed even if it is killed.
  begun=$(date +%s)
  if ! ACTIONS_RUNNER_INPUT_JITCONFIG=$jit "$engine" run -d --rm \
    --name "$name" --hostname "$name" \
    --cap-drop=ALL --cap-add=CHOWN --cap-add=DAC_OVERRIDE --cap-add=FOWNER --cap-add=SETUID \
    --cap-add=SETGID --cap-add=KILL --cap-add=FSETID --cap-add=SETFCAP --cap-add=SYS_CHROOT \
    --cpus "$cpus" --memory "$memory" --memory-swap "$memory" --pids-limit "$pids" \
    --label "pp-iso.slot=$slot" \
    --env ACTIONS_RUNNER_INPUT_JITCONFIG \
    "$PP_ISO_IMAGE" >/dev/null; then
    unset jit response
    failures=$((failures + 1))
    delay=$((failures < 6 ? 5 * (1 << failures) : 300))
    echo "pp-iso slot $slot: the container did not start ($failures in a row); retrying in $delay s" >&2
    name=
    sleep "$delay"
    continue
  fi
  unset jit response
  : > "$runlog"
  { "$engine" logs -f "$name" 2>&1 | tee "$runlog"; } &
  logs_pid=$!
  # Wait for the container to exit. The run time limit counts from the moment the runner starts a
  # job (Runner.Worker appears), so a slot that waited a long time for work does not cut a job short.
  job_start=0
  ps_errors=0
  while :; do
    if ! alive=$("$engine" ps -q --filter "name=^$name\$" 2>/dev/null); then
      ps_errors=$((ps_errors + 1))
      [ "$ps_errors" -lt 12 ] || { echo "pp-iso slot $slot: cannot list containers; removing $name" >&2; break; }
      sleep 5
      continue
    fi
    ps_errors=0
    [ -n "$alive" ] || break
    if [ "$job_start" -eq 0 ] && "$engine" top "$name" 2>/dev/null | grep -q 'Runner.Worker'; then
      job_start=$(date +%s)
    fi
    if [ "$job_start" -ne 0 ] && [ $(( $(date +%s) - job_start )) -gt "$max_job_seconds" ]; then
      echo "pp-iso slot $slot: the job in $name ran over $max_job_seconds s; removing the container" >&2
      break
    fi
    sleep 5
  done
  if ! "$engine" rm -f "$name" >/dev/null 2>&1; then
    left=$("$engine" ps -aq --filter "name=^$name\$" 2>/dev/null) || left=unknown
  fi
  if [ -n "$left" ]; then
    echo "pp-iso slot $slot: could not remove $name; stopping the slot so systemd restarts it and the exit trap retries" >&2
    exit 1
  fi
  sleep 1
  kill "$logs_pid" 2>/dev/null || true
  wait "$logs_pid" 2>/dev/null || true
  name=
  jobs_done=$((jobs_done + 1))
  ran_job=0
  { [ "$job_start" -ne 0 ] || grep -q 'Running job' "$runlog"; } && ran_job=1
  if [ "$ran_job" -eq 0 ] && [ $(( $(date +%s) - begun )) -lt 30 ]; then
    failures=$((failures + 1))
    delay=$((failures < 6 ? 5 * (1 << failures) : 300))
    echo "pp-iso slot $slot: the container exited at once without a job ($failures in a row); retrying in $delay s" >&2
    sleep "$delay"
  else
    failures=0
    sleep 1
  fi
done
