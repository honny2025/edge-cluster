/**
 * 与后端共用的类型定义。
 * 字段名与 docs/design/tech-decisions.md §2 字段表严格一致。
 * 状态枚举与后端 app/models/enums.py 保持同步。
 *
 * 本文件的每一条类型都对应后端一个**唯一**的映射入口
 * （`MetricOut.from_row` / `routes_models._out` / `routes_services.out` / ...）。
 * 后端加字段时先改那边，再改这里 —— 反过来会让界面显示"—"，
 * 而真相是"字段有值、只是没被送出来"，比拿不到数据更难查。
 */

export type NodeStatus = 'never_seen' | 'online' | 'offline'

/** 七态，对应需求 §6 的 Runtime 状态机（Failed 即 error） */
export type ServiceStatus =
  | 'stopped' | 'starting' | 'running' | 'restarting' | 'stopping' | 'crashed' | 'error'

/** 九态，对应需求 §6 的部署状态机 */
export type DeployNodeStatus =
  | 'pending' | 'uploading' | 'downloading' | 'verifying' | 'installing' | 'testing'
  | 'ready' | 'failed' | 'cancelled'

export type DeployTaskStatus =
  | 'pending' | 'running' | 'partial' | 'succeeded' | 'failed' | 'cancelled'

export type ApiType = 'llm' | 'vlm' | 'vision' | 'ocr' | 'other'
export type Framework = 'llama.cpp' | 'onnx' | 'mnn' | 'qnn' | 'other'

// ---------------------------------------------------------------- 节点

/**
 * 设备能力字典。
 * 三态语义：数值项 0/1，未知用 null；**未知不等于"没有"**。
 * `root_state` 为 -1 表示无法判定是否 root。
 */
export interface NodeCapabilities {
  can_proc_stat?: boolean
  can_proc_net?: boolean
  can_thermal?: boolean
  termux_api?: boolean
  gpu_counters?: boolean
  /** QNN Runtime 是否可用：1 有 / 0 没有 / null 未知 */
  qnn_runtime?: number | null
  /** -1 未知 / 0 没有 / 1 有 */
  root_state?: number
  root?: boolean
}

export interface NodeInfo {
  node_id: string
  name: string
  model: string
  soc: string
  android_version: string
  /** 以下三项读不到时后端给 "Unknown" 字符串（不是空串），前端应显示"未识别" */
  cpu: string
  gpu: string
  npu: string
  qnn_version: string
  /** 1 有 root / 0 没有 / -1 未知。**用 -1 而不是 null**，别把"未知"压成"没有" */
  is_root: number
  capabilities: NodeCapabilities
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
  /** 分项温度 / 频率 / 电池。**全部可为 null：拿不到就是 null，不是 0** */
  cpu_temp: number | null
  gpu_temp: number | null
  cpu_freq_mhz: number | null
  gpu_freq_mhz: number | null
  battery_percent: number | null
  battery_temp: number | null
  /** charging / discharging / full / not_charging / unknown 等，原样透传 */
  battery_status: string | null
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
  recent_deployments: DeploymentOut[]
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
  restarting: { text: '重启中', color: 'processing' },
  stopping: { text: '停止中', color: 'warning' },
  stopped: { text: '已停止', color: 'default' },
  crashed: { text: '已崩溃', color: 'error' },
  error: { text: '异常', color: 'error' },
}

// ---------------------------------------------------------------- 模型库（Model Registry）

/** Deployment Package 里的一个附属物（需求 §6） */
export interface ManifestItem {
  name: string
  /** model / context / dlc / lib / config / other */
  kind: string
  size: number
  sha256: string
}

export interface ModelOut {
  model_id: string
  name: string
  version: string
  file_name: string
  file_size: number
  framework: Framework
  format: string
  sha256: string
  /** ---- Model Registry 元数据（需求 §5，兼容性检查的输入）---- */
  quantization: string
  target_soc: string
  qnn_version: string
  backends: string[]
  deps: string[]
  run_params: Record<string, unknown>
  manifest: ManifestItem[]
  self_test_command: string
  self_test_timeout: number
  description: string
  uploaded_at: string | null
  download_count: number
}

export interface ModelListOut {
  items: ModelOut[]
  total: number
}

/** 可增量更新的登记字段。**不要传没打算改的字段** —— 后端按 exclude_unset 处理 */
export interface ModelPatchIn {
  name?: string
  version?: string
  description?: string
  framework?: Framework
  format?: string
  quantization?: string
  target_soc?: string
  qnn_version?: string
  backends?: string[]
  deps?: string[]
  run_params?: Record<string, unknown>
  self_test_command?: string
  self_test_timeout?: number
}

/** 兼容性单项判定。verdict 三态：pass / fail / unknown（**拿不到 ≠ 不兼容**） */
export type CompatVerdict = 'pass' | 'fail' | 'unknown'

export interface CompatCheck {
  name: string
  verdict: CompatVerdict
  detail: string
  want: string
  got: string
}

export interface CompatResult {
  ok: boolean
  reason: string
  checks: CompatCheck[]
}

export interface PrecheckNodeResult extends CompatResult {
  node_id: string
  node_name: string
}

export interface PrecheckOut {
  model_id: string
  results: PrecheckNodeResult[]
}

/** 兼容性维度 → 中文名 */
export const COMPAT_CHECK_META: Record<string, string> = {
  soc: 'SoC',
  qnn: 'QNN 版本',
  qnn_runtime: 'QNN Runtime',
  backends: '推理后端',
}

/** 附属物类型 → 中文名 */
export const MANIFEST_KIND_META: Record<string, string> = {
  model: '模型',
  context: '上下文',
  dlc: 'DLC',
  lib: '库',
  config: '配置',
  other: '其他',
}

// ---------------------------------------------------------------- 部署任务

export interface DeployNodeOut {
  node_id: string
  node_name: string
  status: DeployNodeStatus
  progress: number
  error: string
  retry_count: number
  bytes_downloaded: number
  bytes_total: number
  /** A1：失败后设备被还原到部署前的样子。空串 = 没发生回滚，
   *  或这次失败根本没走到"改动设备文件"那一步（下载就断了）。 */
  rollback_note: string
  started_at: string | null
  finished_at: string | null
  updated_at: string | null
}

export interface DeploymentOut {
  task_id: string
  model_id: string
  model_name: string
  status: DeployTaskStatus
  target_count: number
  note: string
  created_at: string | null
  finished_at: string | null
  nodes: DeployNodeOut[]
}

export interface DeploymentListOut {
  items: DeploymentOut[]
  total: number
}

export interface DeployQueueStats {
  queued: number
  concurrency: number
}

// ---------------------------------------------------------------- 服务

export interface ServiceOut {
  service_id: string
  node_id: string
  model_id: string | null
  name: string
  status: ServiceStatus
  port: number
  endpoint: string
  api_type: ApiType
  pid: number | null
  last_error: string
  command: string
  args: string[] | null
  health_url: string
  auto_restart: boolean
  /** 需求 §7：设备/Agent 重启后是否自动恢复这个 Runtime。默认关闭 */
  auto_start: boolean
  restart_count: number
  last_exit_code: number | null
  state_changed_at: string | null
  mem_required_mb: number
  mutex_group: string
  last_probe_at: string | null
  probe_ok: number | null
  log_path: string
  created_at: string | null
}

/** 创建服务的入参（对应后端 ServiceCreate） */
export interface ServiceCreateIn {
  node_id: string
  name: string
  command: string
  model_id?: string | null
  port?: number
  api_type?: ApiType
  args?: string[]
  health_url?: string
  auto_restart?: boolean
  auto_start?: boolean
  mem_required_mb?: number
  mutex_group?: string
}

export interface ServiceLogsOut {
  service_id: string
  log_path: string
  lines: string[]
  note?: string
  total_bytes?: number | null
  status?: string | null
}

export interface ServiceStatsOut {
  service_id: string
  requests: number
  errors: number
  last_ms: number
  avg_ms: number
  last_status: number | null
}

export const API_TYPE_META: Record<ApiType, { text: string }> = {
  llm: { text: 'LLM' },
  vlm: { text: 'VLM' },
  vision: { text: '视觉' },
  ocr: { text: 'OCR' },
  other: { text: '其他' },
}

// ---------------------------------------------------------------- 文件管理（需求 §4）

/** 设备自报的可访问根目录。平台不写死任何路径 */
export interface FsRoot {
  id: string
  label: string
  path: string
  readable: boolean
  writable: boolean
  /** 拿不到时为 null（**不是 0**） */
  total: number | null
  free: number | null
}

export interface FsRootsOut {
  roots: FsRoot[]
}

export interface FsEntry {
  name: string
  path: string
  is_dir: boolean
  is_link: boolean
  /** 读不到属性时为 null，不是 0 字节 */
  size: number | null
  mtime: number | null
}

export interface FsListOut {
  path: string
  total: number
  offset: number
  limit: number
  /** 为真说明列表被截断，`total` 仍是完整条目数 */
  truncated: boolean
  /** 已在沙箱之外的父目录为空串 */
  parent: string
  space: { total: number | null; free: number | null }
  entries: FsEntry[]
}

export interface FsStatOut {
  path: string
  name: string
  is_dir: boolean
  size: number
  mtime: number
  mode: string
}

export interface FsDuOut {
  path: string
  total_size: number
  file_count: number
  dir_count: number
  /** 为真说明统计值是**下界**（遍历触顶），不能当精确值展示 */
  truncated: boolean
  space: { total: number | null; free: number | null }
}

export type TransferDirection = 'pull' | 'push'
export type TransferStatus =
  | 'pending' | 'transferring' | 'done' | 'failed' | 'cancelled' | 'expired'

export interface TransferOut {
  transfer_id: string
  node_id: string
  direction: TransferDirection
  path: string
  file_name: string
  staging_path: string
  size: number
  sha256: string
  status: TransferStatus
  error: string
  bytes_done: number
  created_at: string | null
  finished_at: string | null
}

export interface TransferListOut {
  items: TransferOut[]
  total: number
}

export const TRANSFER_STATUS_META: Record<TransferStatus, { text: string; color: string }> = {
  pending: { text: '等待中', color: 'default' },
  transferring: { text: '传输中', color: 'processing' },
  done: { text: '已完成', color: 'success' },
  failed: { text: '失败', color: 'error' },
  cancelled: { text: '已取消', color: 'default' },
  expired: { text: '已过期', color: 'warning' },
}

export const TRANSFER_DIRECTION_META: Record<TransferDirection, { text: string }> = {
  pull: { text: '设备 → 平台' },
  push: { text: '平台 → 设备' },
}

// ---------------------------------------------------------------- 状态展示

/** 部署节点状态 → 中文名与标签色 */
export const DEPLOY_NODE_STATUS_META: Record<
  DeployNodeStatus,
  { text: string; color: string }
> = {
  pending: { text: '排队中', color: 'default' },
  uploading: { text: '上传中', color: 'processing' },
  downloading: { text: '下载中', color: 'processing' },
  verifying: { text: '校验中', color: 'processing' },
  installing: { text: '落位中', color: 'processing' },
  testing: { text: '自检中', color: 'processing' },
  ready: { text: '已就绪', color: 'success' },
  failed: { text: '失败', color: 'error' },
  cancelled: { text: '已取消', color: 'default' },
}

export const DEPLOY_TASK_STATUS_META: Record<
  DeployTaskStatus,
  { text: string; color: string }
> = {
  pending: { text: '排队中', color: 'default' },
  running: { text: '下发中', color: 'processing' },
  partial: { text: '部分成功', color: 'warning' },
  succeeded: { text: '成功', color: 'success' },
  failed: { text: '失败', color: 'error' },
  cancelled: { text: '已取消', color: 'default' },
}

/** 终态：进入后该节点不再变化，界面才可以放开"取消"等操作 */
export const DEPLOY_TERMINAL_NODE_STATES: DeployNodeStatus[] = ['ready', 'failed', 'cancelled']

// ---------------------------------------------------------------- 审计（需求 §8）

/**
 * 一条操作审计。
 * **只记写操作**（部署 / 启停 / 删除 / 改配置 / 文件操作），读操作不记 ——
 * 否则 Dashboard 每 5 秒一次轮询会把审计表淹掉。
 */
export interface AuditEntry {
  id: number
  ts: string
  /** shared-token / device:<node_id> / anonymous */
  actor: string
  client_ip: string
  /** deploy.create / service.start / file.delete ... */
  action: string
  target_type: string
  target_id: string
  node_id: string
  /** 0 失败 / 1 成功。**失败的写操作同样留痕** —— "谁试过但没成功"也是审计信息 */
  ok: number
  detail: Record<string, unknown> | null
  message: string
}

export interface AuditListOut {
  items: AuditEntry[]
  total: number
  limit: number
  offset: number
}


// --------------------------------------------------------------- 备份与恢复（A4）

/** 备份现状：库多大、有没有 WAL 未合并、已有哪些备份包 */
export interface BackupStatusOut {
  db_file: string
  db_size: number
  /** WAL 里还有已提交但没合并进主文件的写入 —— 为 true 时尤其不能直接复制 edge.db */
  wal_present: boolean
  tables: Record<string, number>
  backup_dir: string
  backup_count: number
  backups: { name: string; size: number; modified: string }[]
  models_dir: string
}

/** 备份包校验结果。problems 非空时**不要**执行恢复 */
export interface BackupInspectOut {
  ok: boolean
  problems: string[]
  manifest: Record<string, unknown> | null
  db_size: number
  db_sha256: string
  /** null = manifest 里没记 sha256（老版本打的包），不是"对不上" */
  sha256_matches: boolean | null
  includes_models: boolean
  model_files_in_package: number
  current_app_version: string
}

export interface BackupRestoreOut {
  ok: boolean
  /** true = 只校验，什么都没动 */
  dry_run: boolean
  message: string
  restored_from: string
  safety_backup: string
  tables: Record<string, number>
  manifest: Record<string, unknown> | null
  includes_models: boolean
  model_files_in_package: number
  preview: BackupInspectOut | null
}
