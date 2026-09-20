#!/usr/bin/env bash
# stock-mm 进程管理：start / stop / restart / status
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${ROOT}/target/release/stock-mm"
PID_FILE="${ROOT}/data/stock-mm.pid"
LOG_FILE="${ROOT}/data/stock-mm.log"
STOP_TIMEOUT="${STOP_TIMEOUT:-20}"
REBUILD=0

usage() {
  cat <<EOF
用法: $(basename "$0") [--rebuild] {start|stop|restart|status}

  start     后台启动（已在运行则退出）
  stop      先 SIGTERM，超时后再 SIGKILL
  restart   停止后再启动
  status    查看运行状态

  --rebuild  启动前强制 cargo build --release，并在缺少 web/dist 时构建前端

环境变量:
  STOP_TIMEOUT  优雅退出等待秒数，默认 20
EOF
}

is_pid_alive() {
  local pid="$1"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

is_our_process() {
  local pid="$1"
  local comm
  comm="$(ps -p "$pid" -o comm= 2>/dev/null | awk '{print $1}')"
  [[ "$comm" == "stock-mm" ]]
}

running_pid() {
  if [[ -f "$PID_FILE" ]]; then
    local pid
    pid="$(tr -d '[:space:]' <"$PID_FILE" || true)"
    if is_pid_alive "$pid" && is_our_process "$pid"; then
      echo "$pid"
      return 0
    fi
    rm -f "$PID_FILE"
  fi
  return 1
}

listen_addr() {
  if [[ -f "${ROOT}/.env" ]]; then
    awk -F= '/^[[:space:]]*API_LISTEN=/{gsub(/[[:space:]"]/, "", $2); print $2}' "${ROOT}/.env" | tail -n1
  fi
}

ensure_layout() {
  mkdir -p "${ROOT}/data"
  if [[ ! -f "${ROOT}/.env" ]]; then
    echo "警告: 未找到 ${ROOT}/.env ，将使用默认配置。可先: cp .env.example .env" >&2
  fi
}

ensure_web() {
  if [[ -f "${ROOT}/web/dist/index.html" && "$REBUILD" -eq 0 ]]; then
    return 0
  fi
  if [[ ! -d "${ROOT}/web" ]]; then
    return 0
  fi
  echo "构建前端 web/dist ..."
  (cd "${ROOT}/web" && npm install && npm run build)
}

ensure_bin() {
  if [[ -x "$BIN" && "$REBUILD" -eq 0 ]]; then
    return 0
  fi
  echo "构建 ${BIN} ..."
  (cd "$ROOT" && cargo build --release)
  if [[ ! -x "$BIN" ]]; then
    echo "错误: 构建后仍找不到 ${BIN}" >&2
    exit 1
  fi
}

cmd_status() {
  local pid
  if pid="$(running_pid)"; then
    echo "stock-mm 正在运行  pid=${pid}  listen=${LISTEN:-127.0.0.1:8080}"
    echo "日志: ${LOG_FILE}"
    return 0
  fi
  echo "stock-mm 未运行"
  return 1
}

cmd_start() {
  local pid
  if pid="$(running_pid)"; then
    echo "stock-mm 已在运行  pid=${pid}"
    return 0
  fi
  ensure_layout
  ensure_web
  ensure_bin

  echo "启动 stock-mm ..."
  (
    cd "$ROOT"
    nohup "$BIN" >>"$LOG_FILE" 2>&1 &
    echo $! >"$PID_FILE"
  )
  sleep 0.4
  if pid="$(running_pid)"; then
    echo "已启动  pid=${pid}  listen=${LISTEN:-127.0.0.1:8080}"
    echo "日志: ${LOG_FILE}"
    return 0
  fi
  echo "启动失败，最近日志:" >&2
  tail -n 40 "$LOG_FILE" >&2 || true
  rm -f "$PID_FILE"
  exit 1
}

cmd_stop() {
  local pid
  if ! pid="$(running_pid)"; then
    echo "stock-mm 未运行"
    return 0
  fi

  echo "停止 stock-mm  pid=${pid} ..."
  kill -TERM "$pid" 2>/dev/null || true

  local i=0
  while is_pid_alive "$pid"; do
    if (( i >= STOP_TIMEOUT )); then
      echo "等待 ${STOP_TIMEOUT}s 后仍未退出，发送 SIGKILL"
      kill -KILL "$pid" 2>/dev/null || true
      break
    fi
    sleep 1
    i=$((i + 1))
  done

  if is_pid_alive "$pid"; then
    echo "错误: 无法停止 pid=${pid}" >&2
    exit 1
  fi
  rm -f "$PID_FILE"
  echo "已停止"
}

LISTEN="$(listen_addr || true)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rebuild)
      REBUILD=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    start|stop|restart|status)
      break
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done

cmd="${1:-}"
case "$cmd" in
  start) cmd_start ;;
  stop) cmd_stop ;;
  restart) cmd_stop; cmd_start ;;
  status) cmd_status ;;
  *)
    usage >&2
    exit 2
    ;;
esac
