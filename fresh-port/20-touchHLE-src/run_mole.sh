#!/bin/bash
# 在我们自建的 touchHLE 上运行 摩尔庄园 5.5.0,抓全部输出,N 秒后自动结束。
# 用法: bash run_mole.sh [秒数=25] [额外的 touchHLE 参数...]
# 例:  bash run_mole.sh 30 --print-fps
# 日志: $MOLE_LOG (默认 /tmp/mole_run.log)
# 纯离线:不传 --allow-network-access,touchHLE 默认禁网。
set -u
D="/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/20-touchHLE-src/touchHLE"
APP="/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/01-cracked/Payload/MoleWorld.app"
BIN="$D/target/release/touchHLE"
SECS="${1:-25}"; shift 2>/dev/null || true
LOG="${MOLE_LOG:-/tmp/mole_run.log}"
: > "$LOG"
if [ ! -x "$BIN" ]; then echo "NO_BINARY: $BIN" | tee -a "$LOG"; exit 2; fi
# touchHLE 需要 CWD 下有 touchHLE_dylibs/ 与 touchHLE_fonts/
cd "$D" || { echo "CD_FAIL" | tee -a "$LOG"; exit 3; }
echo "=== RUN $(date +%H:%M:%S): touchHLE MoleWorld.app --landscape-right --device-family=ipad $* ===" >> "$LOG"
"$BIN" "$APP" --landscape-right --device-family=ipad "$@" >> "$LOG" 2>&1 &
PID=$!
COUNT=0
while kill -0 "$PID" 2>/dev/null && [ "$COUNT" -lt "$SECS" ]; do sleep 1; COUNT=$((COUNT+1)); done
if kill -0 "$PID" 2>/dev/null; then
  echo "=== 运行满 ${SECS}s 仍未退出(可能正常跑/卡住),发送 SIGINT 结束 ===" >> "$LOG"
  kill -INT "$PID" 2>/dev/null; sleep 1; kill -9 "$PID" 2>/dev/null
  STATUS="RAN_${SECS}s_NO_CRASH"
else
  wait "$PID"; RC=$?; STATUS="EXITED_RC=${RC}"
fi
echo "=== END $(date +%H:%M:%S) STATUS=$STATUS ===" >> "$LOG"
# 关键信号汇总(便于快速判读)
{
  echo "=== [SUMMARY] STATUS=$STATUS ==="
  echo "--- panic / unimplemented / assert ---"
  grep -nE "panicked|thread 'main'|unimplemented|not implemented|assertion failed|UNIMPL" "$LOG" | head -30
  echo "--- 到达的游戏类/场景线索 ---"
  grep -nE "Layer|Scene|Director|Logo|Village|Menu|GameData|cocos2d|class " "$LOG" | head -25
  echo "--- FPS / 渲染线索 ---"
  grep -niE "fps|renderbuffer|EAGL|present|frame" "$LOG" | head -10
} > "${LOG}.summary" 2>&1
echo "WROTE ${LOG} and ${LOG}.summary"
