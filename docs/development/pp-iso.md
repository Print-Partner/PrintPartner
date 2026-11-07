# pp-iso: isolated ephemeral runners

pp-iso is a pool of self-hosted Linux runners for jobs that run code a pull request
can change. Every job gets a fresh container that is deleted when the job ends.
The runner label is `pp-iso`. It is separate from `pp-local`, which keeps its
shared Cargo registry and sccache for trusted `main`, merge-group and manual runs.

Status: the kit, the proof workflow and the `PP_PR_RUNNER` switch exist
([ci.md](ci.md#pull-request-runner-switch)). Nothing runs on pp-iso until a host is
installed and `PP_PR_RUNNER=pp-iso` is set; the switch covers the Linux Rust jobs only.

## Why

[Cargo cache trust](cargo-cache-trust.md) explains why pp-local must not run
PR-editable jobs: its registry and sccache are shared across jobs, so one job can
poison what a later job builds. PRs therefore run on GitHub-hosted `ubuntu-latest`,
which is slower and limited by hosted concurrency. pp-iso gives PRs self-hosted
capacity without sharing state.

| | pp-local | pp-iso |
| --- | --- | --- |
| Runs | `main`, merge groups, manual | Linux Rust PR jobs when `PP_PR_RUNNER=pp-iso` |
| Runner | JIT, one job per container | JIT, one job per container |
| Cargo registry, sccache | shared, host-mounted | none; `actions/cache` only |
| Volumes from the host | yes | none |
| Host docker socket | not exposed to PR code | none |
| Registration credential | host only | host only, never in the container |

## Rules

- One job per container. The loop asks GitHub for a JIT runner config that accepts one
  job, starts a new container with it, waits for the container to exit, and removes it.
- No volumes, no host network, no host devices, no docker socket, no shared Cargo
  registry or sccache. Caches come only from `actions/cache`.
- The container runs the runner as the non-root user `runner` (uid 1001). It runs under
  rootless podman as the unprivileged host user `pp-iso`, so container root maps to an
  unprivileged uid on the host. `runner` has passwordless `sudo` because workflow
  steps call `sudo apt-get`, so any step can be root inside the container. That root is
  confined by the user namespace and by the rules above; it is not restricted to `apt`.
- All capabilities are dropped except `CHOWN`, `DAC_OVERRIDE`, `FOWNER`, `FSETID`, `KILL`,
  `SETUID`, `SETGID`, `SETFCAP` and `SYS_CHROOT`, which `apt`, `sudo` and job steps use. Each job has CPU, memory
  and process limits and a maximum run time.
- The registration credential is a token with repository Administration (self-hosted
  runners) permission. It lives in `/etc/pp-iso/token` (root:root, 0600), is given to the
  service through systemd `LoadCredential`, and is used only by the loop on the host.
  The container receives the one-job JIT config only, passed through the environment so it
  is not in the process list. A JIT config registers a single runner and cannot register
  more or read the token.

## Threat model

A PR adds its own workflow that targets `pp-iso`.

- It gets a clean container. Nothing from earlier jobs exists in it: no registry, no build
  cache, no workspace, no files.
- It can run any code as `runner`, and root inside the user namespace. It cannot reach the
  host filesystem, the docker socket, or other containers' files.
- It cannot read the registration credential: the token is not in the container, its
  environment, or any mounted file.
- When the job ends the container is deleted. A job that runs longer than
  `PP_ISO_MAX_JOB_SECONDS` (default 21600), counted from when the runner starts the job, is
  killed. A container that fails to start, or exits within 30 s without the runner logging `Running job`, makes the loop back off, up to 300 s,
  and each retry registers one new runner, so a broken host does not flood the runner list.
- It can use the JIT config for its own runner, which is good for that job only.
- It can use host CPU and memory up to the per-job limits, and the network the container
  has. Network egress is not filtered by this kit.

Not covered: a rootless-container escape (keep podman and the kernel patched), and
secrets a workflow passes to its own job, which any code in that job can read. pp-iso
jobs must not receive deploy secrets. pp-local must not run PR-editable jobs.

## Host setup

Use the Linux hosts that run pp-local today. Each host needs systemd, rootless podman
(`sudo apt-get install -y podman uidmap slirp4netns`) and `curl` and `jq`.

1. Create a token. In GitHub, create a fine-grained personal access token
   for `poitee/PrintPartner` only, with the repository permission
   **Administration: read and write** and nothing else. Use a personal access token,
   not a GitHub App installation token: installation tokens expire after an hour and nothing in
   the kit refreshes the token file. Note the token's expiry date and rotate it before then. Do not paste it anywhere except the
   hidden prompt below.
2. Give pp-iso a bounded disk. `install.sh` refuses unless `/var/lib/pp-iso` is a separate
   filesystem on a block device or on a fully allocated loop image (not a bind mount or a
   sparse file), so a job that fills its disk uses only space reserved for it and cannot
   starve pp-local or the OS. The slot units also require that mount, so after a reboot with
   the mount missing they do not start. For example, a loop image of a size the host disk
   really has free (check `df -h /var`): `sudo mkdir -p /var/lib/pp-iso && sudo fallocate -l 200G /var/pp-iso.img && sudo mkfs.ext4 -q /var/pp-iso.img`,
   then add `/var/pp-iso.img /var/lib/pp-iso ext4 loop,nofail 0 2` to `/etc/fstab` and run
   `sudo mount /var/lib/pp-iso`. Container layers, job workspaces and the image live there.
3. On the host, check out this repository anywhere and `cd scripts/pp-iso`. `install.sh` copies
   the image files to `/usr/local/lib/pp-iso/image`, so `pp-iso` can build from there.
4. `sudo ./install.sh`. First run creates `/etc/pp-iso/runner.env` and stops.
5. Edit `/etc/pp-iso/runner.env`: set `PP_ISO_SLOTS` and the pp-local reserve (see sizing).
6. `sudo ./install.sh --set-token` and type the token at the hidden prompt.
7. `sudo ./install.sh` again. It checks the pool fits the host, builds the image as the
   `pp-iso` user, installs the units (with cgroup delegation, so CPU and memory limits apply),
   starts a test container with those limits from a system service as `pp-iso`, and only then
   starts the slots. If the test container fails, nothing starts.
8. In GitHub, Settings, Actions, Runners: each slot shows as an idle `pp-iso` runner only
   while it waits; a runner that took a job disappears after it. A slot stopped while idle
   can leave an offline registration; remove it there if it lingers.
9. Run the **pp-iso proof** workflow from the Actions tab. It must pass.

To change the settings or update the kit (including a new runner version), edit `runner.env`
if needed and run `sudo ./install.sh --restart`; it points `PP_ISO_IMAGE` at the image it just
built.
Without `--restart` it refuses while slots run, because a running slot keeps its old limits;
with it, a job in flight is cancelled and can be re-run.

To stop a host: `sudo systemctl stop 'pp-iso-runner@*'`, and to keep the slots off after a
reboot `sudo systemctl disable pp-iso-runner@1.service` for each slot number. To rotate the token
run `sudo ./install.sh --set-token` and `sudo systemctl restart 'pp-iso-runner@*'`.

## Pool size

`install.sh` prints the room left on the host: CPUs and memory minus the pp-local
reserve, divided by the per-job size (default 4 CPUs, 14 GB). It refuses a
`PP_ISO_SLOTS` that does not fit. Set `PP_LOCAL_RESERVE_CPUS` and
`PP_LOCAL_RESERVE_MEMORY_GB` to what the two pp-local slots on that host use at peak,
so pp-iso never starves them. The per-host CPU and memory are not recorded in this
repository; read them with `nproc` and `free -g` when installing.

## Proof

`.github/workflows/pp-iso-proof.yml` is a manual workflow. Two jobs run one after the
other on `pp-iso`. The first records its container name and writes marker files in
`$HOME/.cargo/registry`, `/home/runner` and `/tmp`. The second fails if it runs in the same
container or finds any marker, and prints both container names. Run it after install and
attach the log to GRE-1252.

## Files

- `scripts/pp-iso/Dockerfile`: image with the pinned Actions runner and the `runner` user.
- `scripts/pp-iso/pp-iso-loop.sh`: the per-slot loop.
- `scripts/pp-iso/pp-iso-runner@.service`: the systemd unit, one instance per slot.
- `scripts/pp-iso/install.sh`, `scripts/pp-iso/runner.env.example`: host install.
