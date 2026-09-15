#!/bin/bash
# ============================================================================
#  启动 摩尔庄园 5.5.0(touchHLE)· 账号菜单模式(MOLE_ACCOUNT_MENU=1)
#  这个模式不自动合成登录,放原版走真 passport 流程 —— 点标题界面的登录/切换账号
#  会弹出原版【账号管理菜单】(申请米米号/切换帐号/修改密码/绑定邮箱/找回/快速登录)。
#  passport HTTP 由 touchHLE 代理到私服(明文 80,Host: account-mapi.61.com)。
#  双击运行;终端实时打印日志;Ctrl-C 结束。
# ============================================================================

TOUCHHLE_DIR="/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/20-touchHLE-src/touchHLE"
cd "$TOUCHHLE_DIR" || { echo "❌ 进不去 touchHLE 目录:$TOUCHHLE_DIR"; read -r; exit 1; }

# ===================== 可改配置 =====================
export MOLE_MIMI="${MOLE_MIMI:-88888888}"                  # 默认米米号(账号菜单模式下可在游戏里切换)
export MOLE_SERVER="${MOLE_SERVER:-159.54.175.68:7821}"     # 私服 TCP IP:端口
export MOLE_ACCOUNT_MENU=1                                  # ★账号菜单模式:走真 passport,弹账号菜单
export MOLE_PASSWORD="${MOLE_PASSWORD:-}"                   # 密码(空=服务器宽松接受)
export MOLE_HUD="${MOLE_HUD:-0}"
export MOLE_FIX_MAPEXTEND="${MOLE_FIX_MAPEXTEND:-1}"        # 保持 any_enabled 为真(intercept 生效)
# passport 端点默认 = MOLE_SERVER 的 host + 80(Caddy 的 http://account-mapi.61.com 块 → web 8081)。
# 如需直连其它地址覆盖:export MOLE_PASSPORT="login.moleworld.net:80"
# ====================================================

APP="/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/01-cracked/Payload/MoleWorld.app"
BIN="./target/release/touchHLE"
LOG="/tmp/mole_account_menu.log"

if [ ! -x "$BIN" ]; then
  echo "❌ 找不到 touchHLE 二进制:$TOUCHHLE_DIR/$BIN"
  echo "   先在该目录跑:CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo build --release"
  read -r; exit 2
fi

echo "============================================================"
echo "🎮  摩尔庄园 5.5.0 · ★账号菜单模式"
echo "    私服   : $MOLE_SERVER"
echo "    passport: 连私服 host:80,Host=account-mapi.61.com → web passport"
echo "    玩法   : 点标题的登录/切换账号 → 应弹出原版账号管理菜单"
echo "    日志   : $LOG  (grep '[MOLECHEAT] passport 代理' 看代理是否触发)"
echo "    结束   : Ctrl-C"
echo "============================================================"

"$BIN" "$APP" --landscape-right --device-family=ipad --allow-network-access 2>&1 | tee "$LOG"

echo "============================================================"
echo "游戏已退出。日志在 $LOG。按回车关闭。"
read -r
