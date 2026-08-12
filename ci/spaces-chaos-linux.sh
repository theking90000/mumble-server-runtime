#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRY_RUN=0
SMOKE=0
ACKNOWLEDGED=0
SKIP_BUILD=0
RESULT_ROOT="$ROOT/spaces-chaos-results"
FAULT="takeover-partition"

usage() {
  echo "usage: $0 [--dry-run] [--smoke] [--ack-dedicated-host] [--skip-build] [--result-root DIR] [--fault NAME]"
}

while (($#)); do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --smoke) SMOKE=1 ;;
    --ack-dedicated-host) ACKNOWLEDGED=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    --result-root)
      shift
      RESULT_ROOT="${1:?--result-root requires a directory}"
      ;;
    --fault)
      shift
      FAULT="${1:?--fault requires a name}"
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
  shift
done

case "$FAULT" in
  grpc-one-way|grpc-two-way|latency-jitter|bandwidth|mumble-udp-loss|mumble-tcp-cut|takeover-partition) ;;
  *)
    echo "unsupported fault: $FAULT" >&2
    exit 2
    ;;
esac

RUN_TOKEN="${VOXLOOM_CHAOS_RUN_TOKEN:-$$}"
RUN_TOKEN="${RUN_TOKEN: -5}"
BRIDGE="vxlb${RUN_TOKEN}"
DRIVER_PREFIX="vxl-${RUN_TOKEN}-d"
WORKER_PREFIX="vxl-${RUN_TOKEN}-w"
HOST_IP="10.203.0.1"
PHASE_DIR="$RESULT_ROOT/phase-control-$RUN_TOKEN"
CAMPAIGN_ROOT="$RESULT_ROOT/campaign-$RUN_TOKEN"
HARNESS_PID=""
CREATED_NAMESPACES=()

run() {
  if ((DRY_RUN)); then
    printf '+'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

namespace_name() {
  local kind="$1"
  local index="$2"
  if [[ "$kind" == driver ]]; then
    printf '%s%s' "$DRIVER_PREFIX" "$index"
  else
    printf '%s%s' "$WORKER_PREFIX" "$index"
  fi
}

cleanup() {
  local status=$?
  trap - EXIT INT TERM
  set +e
  if [[ -n "$HARNESS_PID" ]] && kill -0 "$HARNESS_PID" 2>/dev/null; then
    kill "$HARNESS_PID" 2>/dev/null
    wait "$HARNESS_PID" 2>/dev/null
  fi
  for namespace in "${CREATED_NAMESPACES[@]}"; do
    ip netns exec "$namespace" nft delete table inet voxloom_chaos 2>/dev/null
    ip netns exec "$namespace" tc qdisc delete dev eth0 root 2>/dev/null
    ip netns delete "$namespace" 2>/dev/null
  done
  ip link delete "$BRIDGE" 2>/dev/null
  exit "$status"
}

best_effort() {
  if ((DRY_RUN)); then
    run "$@"
  else
    "$@" 2>/dev/null || true
  fi
}

setup_namespace() {
  local namespace="$1"
  local ordinal="$2"
  local host_interface="vh${RUN_TOKEN}${ordinal}"
  local namespace_interface="vn${RUN_TOKEN}${ordinal}"
  run ip netns add "$namespace"
  CREATED_NAMESPACES+=("$namespace")
  run ip link add "$host_interface" type veth peer name "$namespace_interface"
  run ip link set "$host_interface" master "$BRIDGE"
  run ip link set "$host_interface" up
  run ip link set "$namespace_interface" netns "$namespace"
  run ip -n "$namespace" link set lo up
  run ip -n "$namespace" link set "$namespace_interface" name eth0
  run ip -n "$namespace" addr add "10.203.0.$((ordinal + 10))/24" dev eth0
  run ip -n "$namespace" link set eth0 up
  run ip -n "$namespace" route add default via "$HOST_IP"
}

add_nft_base() {
  local namespace="$1"
  run ip netns exec "$namespace" nft add table inet voxloom_chaos
  run ip netns exec "$namespace" nft 'add chain inet voxloom_chaos output { type filter hook output priority 0; policy accept; }'
  run ip netns exec "$namespace" nft 'add chain inet voxloom_chaos input { type filter hook input priority 0; policy accept; }'
}

apply_fault() {
  local grpc_port="$1"
  local mumble_port="$2"
  local driver0
  local worker0
  driver0="$(namespace_name driver 0)"
  worker0="$(namespace_name worker 0)"
  case "$FAULT" in
    grpc-one-way)
      add_nft_base "$driver0"
      run ip netns exec "$driver0" nft add rule inet voxloom_chaos output ip daddr "$HOST_IP" tcp dport "$grpc_port" drop
      ;;
    grpc-two-way|takeover-partition)
      add_nft_base "$driver0"
      run ip netns exec "$driver0" nft add rule inet voxloom_chaos output ip daddr "$HOST_IP" tcp dport "$grpc_port" drop
      run ip netns exec "$driver0" nft add rule inet voxloom_chaos input ip saddr "$HOST_IP" tcp sport "$grpc_port" drop
      ;;
    latency-jitter)
      run ip netns exec "$driver0" tc qdisc replace dev eth0 root netem delay 100ms 20ms distribution normal
      ;;
    bandwidth)
      run ip netns exec "$driver0" tc qdisc replace dev eth0 root netem rate 2mbit
      ;;
    mumble-udp-loss)
      add_nft_base "$worker0"
      run ip netns exec "$worker0" nft add rule inet voxloom_chaos output ip daddr "$HOST_IP" udp dport "$mumble_port" drop
      run ip netns exec "$worker0" nft add rule inet voxloom_chaos input ip saddr "$HOST_IP" udp sport "$mumble_port" drop
      ;;
    mumble-tcp-cut)
      add_nft_base "$worker0"
      run ip netns exec "$worker0" nft add rule inet voxloom_chaos output ip daddr "$HOST_IP" tcp dport "$mumble_port" drop
      run ip netns exec "$worker0" nft add rule inet voxloom_chaos input ip saddr "$HOST_IP" tcp sport "$mumble_port" drop
      ;;
  esac
}

heal_fault() {
  local driver0
  local worker0
  driver0="$(namespace_name driver 0)"
  worker0="$(namespace_name worker 0)"
  best_effort ip netns exec "$driver0" nft delete table inet voxloom_chaos
  best_effort ip netns exec "$worker0" nft delete table inet voxloom_chaos
  best_effort ip netns exec "$driver0" tc qdisc delete dev eth0 root
  best_effort ip netns exec "$worker0" tc qdisc delete dev eth0 root
}

snapshot_proc() {
  local phase="$1"
  local output="$RESULT_ROOT/proc-${phase}.csv"
  printf 'phase,role,pid,threads,fds,sockets\n' >"$output"
  local role
  local pid
  for role in coordinator driver-0 driver-1 worker-0 worker-1; do
    if [[ "$role" == coordinator ]]; then
      pid="$HARNESS_PID"
    else
      local kind="${role%%-*}"
      local index="${role##*-}"
      pid="$(ip netns pids "$(namespace_name "$kind" "$index")" | head -n 1)"
    fi
    if [[ -z "$pid" || ! -d "/proc/$pid" ]]; then
      continue
    fi
    local threads
    local fds
    local sockets
    threads="$(awk '/^Threads:/ {print $2}' "/proc/$pid/status")"
    fds="$(find "/proc/$pid/fd" -mindepth 1 -maxdepth 1 -print | wc -l)"
    sockets="$(find "/proc/$pid/fd" -mindepth 1 -maxdepth 1 -type l -lname 'socket:*' -print | wc -l)"
    printf '%s,%s,%s,%s,%s,%s\n' "$phase" "$role" "$pid" "$threads" "$fds" "$sockets" >>"$output"
  done
}

wait_for_file() {
  local path="$1"
  local deadline=$((SECONDS + 120))
  while [[ ! -f "$path" ]]; do
    if ((SECONDS >= deadline)); then
      echo "timed out waiting for $path" >&2
      return 1
    fi
    sleep 0.1
  done
}

if ((DRY_RUN)); then
  RESULT_ROOT="/tmp/voxloom-chaos-dry-run"
  PHASE_DIR="$RESULT_ROOT/phase-control-$RUN_TOKEN"
  CAMPAIGN_ROOT="$RESULT_ROOT/campaign-$RUN_TOKEN"
else
  if [[ "$(uname -s)" != Linux ]]; then
    echo "real chaos execution requires Linux; use --dry-run elsewhere" >&2
    exit 1
  fi
  if ((EUID != 0)); then
    echo "real chaos execution requires root for ip netns, tc, and nft" >&2
    exit 1
  fi
  if ((ACKNOWLEDGED == 0)); then
    echo "refusing chaos without --ack-dedicated-host" >&2
    exit 1
  fi
  trap cleanup EXIT INT TERM
fi

run mkdir -p "$RESULT_ROOT" "$PHASE_DIR" "$CAMPAIGN_ROOT"
run ip link add "$BRIDGE" type bridge
run ip addr add "$HOST_IP/24" dev "$BRIDGE"
run ip link set "$BRIDGE" up
setup_namespace "$(namespace_name driver 0)" 0
setup_namespace "$(namespace_name driver 1)" 1
setup_namespace "$(namespace_name worker 0)" 2
setup_namespace "$(namespace_name worker 1)" 3

if ((DRY_RUN)); then
  apply_fault 4000 64738
  run target/debug/mumble-spaces-stress run --mode managed --managed-bind-ip "$HOST_IP" \
    --driver-netns-prefix "$DRIVER_PREFIX" --worker-netns-prefix "$WORKER_PREFIX" \
    --phase-control "$PHASE_DIR" --controllers 2 --participants 8 \
    --participants-per-space 8 --scenario batch-takeover --duration 2s \
    --result-root "$CAMPAIGN_ROOT"
  heal_fault
  run ip netns exec "$(namespace_name driver 0)" true
  run ip netns exec "$(namespace_name worker 0)" true
  for namespace in "${CREATED_NAMESPACES[@]}"; do
    run ip netns delete "$namespace"
  done
  run ip link delete "$BRIDGE"
  echo "spaces-chaos-linux: dry-run complete"
  exit 0
fi

if ((SMOKE == 0)); then
  echo "only --smoke performs a campaign; use --dry-run to inspect operations" >&2
  exit 2
fi

cd "$ROOT"
if ((SKIP_BUILD == 0)); then
  GRADLE_USER_HOME=.gradle ./gradlew \
    :implementations:spaces:tools:load-driver-java:installDist
  cargo build -p mumble-spaces-stress
fi

target/debug/mumble-spaces-stress run \
  --mode managed \
  --managed-bind-ip "$HOST_IP" \
  --driver-netns-prefix "$DRIVER_PREFIX" \
  --worker-netns-prefix "$WORKER_PREFIX" \
  --phase-control "$PHASE_DIR" \
  --controllers 2 \
  --participants 8 \
  --participants-per-space 8 \
  --scenario batch-takeover \
  --duration 2s \
  --result-root "$CAMPAIGN_ROOT" \
  >"$RESULT_ROOT/coordinator.log" 2>&1 &
HARNESS_PID=$!

wait_for_file "$PHASE_DIR/stable.ready"
RUN_DIRECTORY="$(find "$CAMPAIGN_ROOT" -mindepth 1 -maxdepth 1 -type d -print -quit)"
SERVER_LINE="$(sed -n '1p' "$RUN_DIRECTORY/server.log")"
GRPC_PORT="$(printf '%s\n' "$SERVER_LINE" | sed -E 's/.*controller_endpoint=http:\/\/[^:]+:([0-9]+).*/\1/')"
MUMBLE_PORT="$(printf '%s\n' "$SERVER_LINE" | sed -E 's/.*mumble_server=[^:]+:([0-9]+).*/\1/')"

apply_fault "$GRPC_PORT" "$MUMBLE_PORT"
snapshot_proc injected
touch "$PHASE_DIR/injection.go"
sleep 1
heal_fault
snapshot_proc healed

wait "$HARNESS_PID"
HARNESS_PID=""
test -f "$PHASE_DIR/complete.ready"
grep -q '"credential_rotations": 16' "$RUN_DIRECTORY/summary.json"
grep -q '"ownership_violations": 0' "$RUN_DIRECTORY/summary.json"
grep -q '"phase":"injection"' "$RUN_DIRECTORY/events.jsonl"
grep -q '"phase":"reconciliation"' "$RUN_DIRECTORY/events.jsonl"
if grep -R -E '"credential"|secret-token|bearer' "$RESULT_ROOT"; then
  echo "spaces-chaos-linux: secret-shaped data found in artifacts" >&2
  exit 1
fi

echo "spaces-chaos-linux: partitioned takeover healed and converged"
