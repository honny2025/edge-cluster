/**
 * 与后端共用的类型定义。
 * 字段名与 docs/design/tech-decisions.md §2 字段表严格一致。
 * 状态枚举与后端 app/models/enums.py 保持同步。
 */

export type NodeStatus = 'never_seen' | 'online' | 'offline'
export type ServiceStatus = 'stopped' | 'starting' | 'running' | 'error'
export type DeployNodeStatus =
  | 'pending' | 'downloading' | 'verifying' | 'installing' | 'ready' | 'failed'
export type DeployTaskStatus = 'pending' | 'running' | 'partial' | 'succeeded' | 'failed'
export type ApiType = 'llm' | 'vlm' | 'vision' | 'ocr' | 'other'
export type Framework = 'llama.cpp' | 'onnx' | 'mnn' | 'qnn' | 'other'

export interface NodeInfo {
  node_id: string
  name: string
  model: string
  soc: string
  android_version: string
  agent_version: string
  ip: string
  status: NodeStatus
  last_heartbeat: string | null
  last_heartbeat_ts: number | null
  online_since: string | null
  uptime_seconds: number
  is_simulated: boolean
  note: string
  created_at: string | null
  updated_at: string | null
}

export interface NodeListOut {
  items: NodeInfo[]
  total: number
}

export interface MetricSample {
  ts: string
  cpu_usage: number | null
  gpu_usage: number | null
  npu_usage: number | null
  mem_used: number | null
  mem_total: number | null
  storage_used: number | null
  storage_total: number | null
  temperature: number | null
  power_watts: number | null
  net_rx_bytes: number | null
  net_tx_bytes: number | null
  load_avg: number | null
  extra: Record<string, unknown> | null
}

export interface MetricSeriesOut {
  node_id: string
  count: number
  items: MetricSample[]
}

export interface NodeStats {
  total: number
  online: number
  offline: number
  never_seen: number
}

export interface SystemInfoOut {
  version: string
  server_time: string
  started_at: string
  uptime_seconds: number
  connected_nodes: number
  node_stats: NodeStats
  heartbeat_interval: number
  offline_threshold: number
  discovery_port: number
}

/** Dashboard WebSocket 推送的事件 */
export interface DashboardEvent {
  type: 'event' | 'ping'
  event?: 'node_online' | 'node_offline' | 'node_updated' | 'node_deleted' | 'discovery_ack'
  data?: Record<string, unknown>
  ts?: number
}

// ---------------------------------------------------------------- 里程碑 2

export type AlertLevel = 'warning' | 'critical'
export type AlertKind = 'cpu' | 'memory' | 'storage' | 'temperature' | 'node_offline'

export interface Alert {
  level: AlertLevel
  kind: AlertKind
  node_id: string
  node_name: string
  message: string
  value: number | null
  threshold: number | null
  unit: string
}

export interface AlertSummary {
  critical: number
  warning: number
  total: number
}

/**
 * 集群资源汇总。
 *
 * 平均值类字段为 null 表示**没有有效样本**（不是 0），前端显示"—"。
 * `counted_nodes` 是真正参与统计的节点数（在线且采样新鲜），
 * 与 `online_nodes` 不等时说明有节点在线但指标采不到。
 */
export interface ResourceSummary {
  cpu_avg: number | null
  gpu_avg: number | null
  npu_avg: number | null
  temperature_avg: number | null
  temperature_max: number | null
  mem_used: number | null
  mem_total: number | null
  mem_percent: number | null
  storage_used: number | null
  storage_total: number | null
  storage_percent: number | null
  counted_nodes: number
  online_nodes: number
  stale_nodes: string[]
}

export interface ServicesSummary {
  running: number
  total: number
}

export interface AbnormalNode {
  node_id: string
  name: string
  status: NodeStatus
  last_heartbeat: string | null
  ip: string
  is_simulated: boolean
}

export interface OverviewOut {
  generated_at: string
  node_stats: NodeStats
  resources: ResourceSummary
  services: ServicesSummary
  alerts: Alert[]
  alert_summary: AlertSummary
  abnormal_nodes: AbnormalNode[]
  /**
   * 各节点最新指标。
   * **离线或采样陈旧的节点为 null** —— 后端从源头就不下发旧值，
   * 前端据此显示"离线"，不可能拿旧数据冒充在线。
   */
  nodes_metrics: Record<string, MetricSample | null>
  recent_deployments: Record<string, unknown>[]
  stale_nodes: string[]
}

export interface ClusterSeriesPoint {
  ts: string
  cpu_avg: number | null
  gpu_avg: number | null
  npu_avg: number | null
  temperature_avg: number | null
  mem_used: number | null
  mem_total: number | null
  mem_percent: number | null
  storage_used: number | null
  storage_total: number | null
  storage_percent: number | null
  node_count: number
}

export interface ClusterSeriesOut {
  minutes: number
  bucket_seconds: number
  online_nodes: number
  count: number
  items: ClusterSeriesPoint[]
}

/** 告警类型 → 中文名与配色 */
export const ALERT_KIND_META: Record<AlertKind, { text: string; color: string }> = {
  cpu: { text: 'CPU 偏高', color: '#4f9cf9' },
  memory: { text: '内存偏高', color: '#faad14' },
  storage: { text: '存储偏高', color: '#13c2c2' },
  temperature: { text: '温度异常', color: '#ff7875' },
  node_offline: { text: '节点离线', color: '#ff4d4f' },
}

/** 状态颜色映射：与"离线/异常必须显眼"的要求一致 */
export const NODE_STATUS_META: Record<NodeStatus, { text: string; color: string }> = {
  online: { text: '在线', color: 'success' },
  offline: { text: '离线', color: 'error' },
  never_seen: { text: '从未上线', color: 'default' },
}

export const SERVICE_STATUS_META: Record<ServiceStatus, { text: string; color: string }> = {
  running: { text: '运行中', color: 'success' },
  starting: { text: '启动中', color: 'processing' },
  stopped: { text: '已停止', color: 'default' },
  error: { text: '异常', color: 'error' },
}
