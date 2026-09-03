export type ConnectionPhase =
  | "signaling"
  | "session"
  | "candidates"
  | "secure"
  | "video"
  | "ready"
  | "reconnecting"
  | "failed";

export const CONNECTION_STEPS = [
  { phase: "signaling", label: "连接服务" },
  { phase: "session", label: "接管桌面" },
  { phase: "candidates", label: "寻找路径" },
  { phase: "secure", label: "安全握手" },
  { phase: "video", label: "加载画面" },
] as const;

export interface ConnectionProgressSnapshot {
  title: string;
  description: string;
  percent: number;
  activeStep: number;
}

const PHASES: Record<ConnectionPhase, ConnectionProgressSnapshot> = {
  signaling: {
    title: "正在连接服务",
    description: "建立实时信令通道，准备发起远程会话",
    percent: 10,
    activeStep: 0,
  },
  session: {
    title: "正在接管桌面",
    description: "主机正在关闭其他连接，新连接拥有最高优先级",
    percent: 28,
    activeStep: 1,
  },
  candidates: {
    title: "正在寻找最快路径",
    description: "解析局域网地址并测试 WebRTC 网络候选",
    percent: 52,
    activeStep: 2,
  },
  secure: {
    title: "正在建立安全通道",
    description: "网络路径已找到，正在完成 ICE 与 DTLS 握手",
    percent: 76,
    activeStep: 3,
  },
  video: {
    title: "正在加载桌面画面",
    description: "安全通道已连接，等待首个视频关键帧",
    percent: 92,
    activeStep: 4,
  },
  ready: {
    title: "连接完成",
    description: "桌面画面已就绪",
    percent: 100,
    activeStep: CONNECTION_STEPS.length,
  },
  reconnecting: {
    title: "网络波动，正在恢复",
    description: "保留当前会话并重新验证可用网络路径",
    percent: 68,
    activeStep: 3,
  },
  failed: {
    title: "连接未能完成",
    description: "网络候选未能建立可用路径",
    percent: 100,
    activeStep: -1,
  },
};

export function connectionProgressSnapshot(
  phase: ConnectionPhase,
  detail?: string,
): ConnectionProgressSnapshot {
  const snapshot = PHASES[phase];
  return detail ? { ...snapshot, description: detail } : snapshot;
}
