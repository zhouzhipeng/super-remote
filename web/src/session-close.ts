export function disconnectMessage(reason: string): string {
  if (reason === "replaced_by_new_connection") return "此连接已被新的远程连接替代。";
  if (reason === "session_unavailable") return "此连接已失效，已有更新的连接获得了控制权。";
  if (reason === "peer_closed") return "另一端已结束远程桌面连接。";
  return reason || "主机已结束远程桌面连接。";
}
