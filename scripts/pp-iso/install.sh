#!/usr/bin/env bash
# Installs the pp-iso runner kit on a Linux host with systemd and rootless podman.
#   sudo ./install.sh              install the kit, build the image, start the slots
#   sudo ./install.sh --restart    the same, and restart slots that are already running
#   sudo ./install.sh --set-token  store the registration token (typed at a hidden prompt)
# Run it from a checkout of scripts/pp-iso. It never prints or logs the token.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
etc=/etc/pp-iso
lib=/usr/local/lib/pp-iso
user=pp-iso

[ "$(id -u)" -eq 0 ] || { echo "run this as root (sudo)" >&2; exit 1; }
install -d -m 0755 "$etc"

if [ "${1:-}" = "--set-token" ]; then
  printf 'Token for repository Administration (self-hosted runners) only; input is hidden: '
  read -rs token
  echo
  [ -n "$token" ] || { echo "empty token, nothing stored" >&2; exit 1; }
  (umask 077; printf '%s\n' "$token" > "$etc/token.new")
  chown root:root "$etc/token.new"
  chmod 0600 "$etc/token.new"
  mv "$etc/token.new" "$etc/token"
  unset token
  echo "stored $etc/token (root:root, 0600)"
  exit 0
fi

command -v podman >/dev/null || { echo "install podman first (apt install podman)" >&2; exit 1; }
command -v systemctl >/dev/null || { echo "systemd is required" >&2; exit 1; }
[ -f "$etc/runner.env" ] || { install -m 0644 "$here/runner.env.example" "$etc/runner.env"; echo "created $etc/runner.env; edit PP_ISO_SLOTS and the reserves, then run this again"; exit 0; }
# shellcheck disable=SC1091
. "$etc/runner.env"

[ "$(stat -c %d /var/lib/pp-iso)" != "$(stat -c %d /)" ] && mountpoint -q /var/lib/pp-iso || { echo "/var/lib/pp-iso must be its own filesystem (a dedicated partition or a loop-mounted image of fixed size), so a job cannot fill the host disk; see docs/development/pp-iso.md" >&2; exit 1; }
src=$(findmnt -n -o SOURCE /var/lib/pp-iso)
case "$src" in
  *\[*) echo "/var/lib/pp-iso is a bind mount ($src); mount a block device or a fully allocated loop image directly" >&2; exit 1 ;;
  /dev/loop*)
    backing=$(losetup -nl -O BACK-FILE "$src" | head -n1 | tr -d ' ')
    [ -f "$backing" ] && [ $(( $(stat -c %b "$backing") * $(stat -c %B "$backing") )) -ge "$(stat -c %s "$backing")" ] || { echo "the backing file of /var/lib/pp-iso ($backing) is sparse; fully allocate it (fallocate), so its size is really reserved on the host disk" >&2; exit 1; } ;;
  /dev/*) ;;
  *) echo "/var/lib/pp-iso is mounted from $src; use a block device or a fully allocated loop image" >&2; exit 1 ;;
esac
running=$(systemctl list-units --state=active --plain --no-legend 'pp-iso-runner@*.service' | awk '{print $1}')
if [ -n "$running" ] && [ "${1:-}" != "--restart" ]; then
  echo "pp-iso slots are running and keep their old settings until restarted. Re-run with --restart to restart them now (a job in flight is cancelled and GitHub can re-run it), or stop them first." >&2
  exit 1
fi

id "$user" >/dev/null 2>&1 || useradd --system --create-home --home-dir /var/lib/pp-iso --shell /usr/sbin/nologin "$user"
chown "$user:$user" /var/lib/pp-iso
chmod 0700 /var/lib/pp-iso
first_free=$(awk -F: '{ e = $2 + $3; if (e > m) m = e } END { print (m < 100000 ? 100000 : m) }' /etc/subuid /etc/subgid)
grep -q "^$user:" /etc/subuid || usermod --add-subuids "$first_free-$((first_free + 65535))" "$user"
grep -q "^$user:" /etc/subgid || usermod --add-subgids "$first_free-$((first_free + 65535))" "$user"
loginctl enable-linger "$user"
if getent group docker | grep -qw "$user"; then
  echo "$user is in the docker group; remove it. pp-iso must not reach a host docker socket." >&2
  exit 1
fi

cpus=$(nproc)
mem_gb=$(awk '/MemTotal/ {printf "%d", $2 / 1048576}' /proc/meminfo)
per_cpu=${PP_ISO_CPUS:-4}
per_mem=$(printf '%s' "${PP_ISO_MEMORY:-14g}" | tr -dc '0-9')
by_cpu=$(( (cpus - ${PP_LOCAL_RESERVE_CPUS:-0}) / per_cpu ))
by_mem=$(( (mem_gb - ${PP_LOCAL_RESERVE_MEMORY_GB:-0}) / per_mem ))
fit=$(( by_cpu < by_mem ? by_cpu : by_mem ))
echo "host: $cpus CPUs, $mem_gb GB; pp-local keeps ${PP_LOCAL_RESERVE_CPUS:-0} CPUs and ${PP_LOCAL_RESERVE_MEMORY_GB:-0} GB; each pp-iso job takes $per_cpu CPUs and ${PP_ISO_MEMORY:-14g}; room for $fit slot(s)"
if [ "${PP_ISO_SLOTS:-1}" -gt "$fit" ] || [ "$fit" -lt 1 ]; then
  echo "PP_ISO_SLOTS=${PP_ISO_SLOTS:-1} does not fit; lower it or the per-job size, or raise the pp-local reserve is wrong" >&2
  exit 1
fi

rootless=$(runuser -u "$user" -- env XDG_RUNTIME_DIR="/run/user/$(id -u "$user")" podman info --format '{{.Host.Security.Rootless}}')
[ "$rootless" = true ] || { echo "podman is not rootless for $user; refusing to continue" >&2; exit 1; }

install -d -m 0755 "$lib"
install -m 0755 "$here/pp-iso-loop.sh" "$lib/pp-iso-loop.sh"
uid=$(id -u "$user")
sed "s/__UID__/$uid/g" "$here/pp-iso-runner@.service" > /etc/systemd/system/pp-iso-runner@.service
chmod 0644 /etc/systemd/system/pp-iso-runner@.service
systemctl daemon-reload

version=$(sed -n 's/^ARG RUNNER_VERSION=//p' "$here/Dockerfile")
image="localhost/pp-iso-runner:$version"
runuser -u "$user" -- env XDG_RUNTIME_DIR="/run/user/$(id -u "$user")" HOME=/var/lib/pp-iso podman build -t "$image" "$here"
systemctl start "user@$uid.service"
systemd-run --wait --pipe --collect -p User="$user" -p Delegate=yes \
  -p Environment="XDG_RUNTIME_DIR=/run/user/$uid" -p Environment=HOME=/var/lib/pp-iso \
  podman run --rm --cpus 1 --memory 256m --entrypoint true "$image" \
  || { echo "podman cannot start a container with cpu and memory limits from a system service as $user; fix this before starting the slots" >&2; exit 1; }
[ "${PP_ISO_IMAGE:-}" = "$image" ] || echo "note: set PP_ISO_IMAGE=$image in $etc/runner.env"

[ -s "$etc/token" ] || { echo "no token yet: run 'sudo $0 --set-token', then run this again" >&2; exit 1; }

for unit in $(systemctl list-units --all --plain --no-legend 'pp-iso-runner@*.service' | awk '{print $1}'); do
  n=${unit#pp-iso-runner@}; n=${n%.service}
  [ "$n" -le "${PP_ISO_SLOTS:-1}" ] || systemctl disable --now "$unit"
done
for n in $(seq 1 "${PP_ISO_SLOTS:-1}"); do
  systemctl enable "pp-iso-runner@$n.service"
  systemctl restart "pp-iso-runner@$n.service"
done
systemctl --no-pager --lines=3 status 'pp-iso-runner@*.service' || true
