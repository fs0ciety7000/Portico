#!/usr/bin/env bash
# Throwaway environment for the desktop E2E test: a HOME and XDG dirs of
# their own (never the real ~/.ssh), an OpenSSH sshd on 127.0.0.1, an
# ssh-agent, and an ~/.ssh/config with three hosts.
#
#   e2e/env.sh <root>     creates everything under <root> and writes
#                         <root>/env (variables to source), <root>/pids
#                         and <root>/dirs (directories made outside <root>)
#
# The caller kills the processes listed in <root>/pids and removes <root>
# and the directories listed in <root>/dirs.
set -euo pipefail

root=${1:?usage: env.sh <root>}
mkdir -p "$root"
root=$(cd "$root" && pwd)
home=$root/home
srv=$root/sshd
remote=$root/remote
mkdir -p "$home/.ssh" "$srv" "$remote" "$root/xdg/config" "$root/xdg/data" "$root/xdg/cache" "$root/xdg/runtime" "$root/shots"
chmod 700 "$home/.ssh" "$root/xdg/runtime"

port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
user=$(id -un)

# Server side: host key, authorized_keys (bootstrap key), and a HOME for the
# sessions so copy-id writes there.
ssh-keygen -q -t ed25519 -N "" -C e2e-host -f "$srv/host_ed25519"
ssh-keygen -q -t ed25519 -N "" -C e2e-bootstrap -f "$home/.ssh/bootstrap"
cp "$home/.ssh/bootstrap.pub" "$srv/authorized_keys"
cat >"$srv/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $srv/host_ed25519
PidFile $srv/sshd.pid
AuthorizedKeysFile $srv/authorized_keys $remote/.ssh/authorized_keys
SetEnv HOME=$remote
StrictModes no
UsePAM no
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin yes
AllowTcpForwarding yes
# SFTP starts in the throwaway remote HOME (not the passwd one).
Subsystem sftp internal-sftp -d $remote
LogLevel VERBOSE
EOF
mkdir -p /run/sshd 2>/dev/null || true
/usr/sbin/sshd -D -e -f "$srv/sshd_config" 2>"$srv/sshd.log" &
echo $! >>"$root/pids"

# A web server behind the -L tunnel test.
mkdir -p "$remote/www"
echo "portico-e2e-tunnel" >"$remote/www/index.html"
# A file for the SFTP download test.
echo "hello from the server" >"$remote/hello.txt"
web_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
python3 -m http.server "$web_port" --bind 127.0.0.1 --directory "$remote/www" >/dev/null 2>&1 &
echo $! >>"$root/pids"

# Client side. `alpha` carries a SetEnv value the logs panel must mask.
cat >"$home/.ssh/config" <<EOF
Host alpha
  HostName 127.0.0.1
  Port $port
  User $user
  IdentityFile ~/.ssh/bootstrap
  IdentitiesOnly yes
  SetEnv PORTICO_E2E_TOKEN=s3cr3t-e2e-value

Host jumped
  HostName 127.0.0.1
  Port $port
  User $user
  IdentityFile ~/.ssh/bootstrap
  IdentitiesOnly yes
  ProxyJump alpha

Host gamma
  HostName 127.0.0.1
  Port $port
  User $user
  IdentityFile ~/.ssh/portico_e2e
  IdentitiesOnly yes
EOF
chmod 600 "$home/.ssh/config"

# A Unix socket path is limited to ~108 bytes: a short directory of its own,
# removed by the caller (listed in <root>/dirs).
sock_dir=$(mktemp -d /tmp/portico-agent.XXXXXX)
echo "$sock_dir" >>"$root/dirs"
eval "$(ssh-agent -s -a "$sock_dir/agent.sock")" >/dev/null
echo "$SSH_AGENT_PID" >>"$root/pids"

cat >"$root/env" <<EOF
export HOME=$home
export XDG_CONFIG_HOME=$root/xdg/config
export XDG_DATA_HOME=$root/xdg/data
export XDG_CACHE_HOME=$root/xdg/cache
export XDG_RUNTIME_DIR=$root/xdg/runtime
export SSH_AUTH_SOCK=$sock_dir/agent.sock
# No session bus: no system keyring, so the encrypted vault is the store.
export DBUS_SESSION_BUS_ADDRESS=disabled:
export E2E_ROOT=$root
export E2E_PORT=$port
export E2E_WEB_PORT=$web_port
export E2E_REMOTE_HOME=$remote
export E2E_USER=$user
export USER=$user
EOF

for _ in $(seq 100); do
  if python3 -c "import socket; socket.create_connection(('127.0.0.1', $port), 0.2)" 2>/dev/null; then exit 0; fi
  sleep 0.05
done
echo "sshd did not start:" >&2
cat "$srv/sshd.log" >&2
exit 1
